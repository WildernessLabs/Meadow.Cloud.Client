use std::ffi::OsStr;
use std::sync::{Mutex, Arc};
use std::{collections::HashMap, ops::Deref};
use std::path::{Path, PathBuf};
use std::fs::{self, OpenOptions, File};
use std::io::{Write, Cursor, copy, BufReader};
use zip::{ZipArchive};

use crate::{cloud_settings::CloudSettings, update_descriptor::UpdateDescriptor};

pub struct UpdateStore {
    _settings: CloudSettings,
    store_directory: PathBuf,
    updates: HashMap<String, Arc<Mutex<UpdateDescriptor>>>
}

impl UpdateStore {
    const STORE_ROOT_FOLDER:&str = "/home/ctacke/meadow/updates";
    const UPDATE_INFO_FILE_NAME: &str = "info.json";

    pub fn new(settings: CloudSettings) -> UpdateStore {
        let mut store = UpdateStore {
            _settings : settings,
            store_directory: PathBuf::from(Self::STORE_ROOT_FOLDER),
            updates: HashMap::new()
        };
        
        println!("Update data will be stored in '{:?}'", store.store_directory);

        if ! store.store_directory.exists() {
            fs::create_dir(&store.store_directory).unwrap();
        }
        else {
            // TODO: load all existing update descriptors
            for entry in fs::read_dir(&store.store_directory).unwrap() {
                match entry {
                    Ok(e) => {
                        if e.path().is_dir() {
                            // it's a likely update folder, but look for (and parse) an info file to be sure
                            for entry in fs::read_dir(e.path()).unwrap() {
                                match entry {
                                    Ok(f) => {
                                        let fp = f.path();
                                        let file_name = fp.file_name().unwrap_or(OsStr::new(""));
                                        if fp.is_file() && file_name == Self::UPDATE_INFO_FILE_NAME {
                                            println!("Update found: {:?}", e.file_name());

                                            match File::open(fp) {
                                                Ok(file) => {
                                                    let reader = BufReader::new(file);
                                                    match serde_json::from_reader(reader) {
                                                        Ok(descriptor) => store.add(Arc::new(descriptor)),
                                                        Err(err) => {
                                                            println!("Cannot deserialize info for {:?}: {:?}", e.file_name(), err);
                                                        }        
                                                    }
                                                },
                                                Err(err) => {
                                                    println!("Cannot open info file for {:?}: {:?}", e.file_name(), err);
                                                }
                                            }
                                        }
                                    },
                                    Err(_e) => {
                                        // TODO: ???
                                    }
                                }
                            }
                        }
                    },
                    Err(_e) => {
                        // TODO: ???
                    }
                }
            }
        }

        store
    }

    pub fn get_all_messages(&self) -> Vec<Arc<Mutex<UpdateDescriptor>>> {
        self.updates.values().cloned().collect::<Vec<Arc<Mutex<UpdateDescriptor>>>>()        
    }

    pub fn add(&mut self, descriptor: Arc<UpdateDescriptor>) {
        let rf = Arc::new( Mutex::new((*descriptor).clone()));
        let id = descriptor.deref().mpak_id.clone();
        self.updates.insert(id, rf);
        self.save_or_update(descriptor.deref());
    }

    pub fn len(&self) -> i32 {
        self.updates.len() as i32
    }

    pub fn get_message(&self, id: String) -> Option<&Arc<Mutex<UpdateDescriptor>>> {
        self.updates.get(&id)
    }

    pub fn clear(&mut self) {
        self.updates.clear();
    }

    pub async fn extract_update(&self, id: &String, destination_root: String) -> Result<u64, String> {
        let update = self.updates.get(id);
        match update {
            Some(u) => {
                let mut d = u.lock().unwrap();

                let file_name = format!("/home/ctacke/{}.mpak", d.mpak_id);

                let zip_file = File::open(file_name).unwrap();
                let mut archive = ZipArchive::new(zip_file).unwrap();
            
                for i in 0..archive.len() {
                    let mut file = archive.by_index(i).unwrap();
                    let outpath = Path::new(&destination_root).join(file.name());
                    if (&*file.name()).ends_with('/') {
                        std::fs::create_dir_all(&outpath).unwrap();
                    } 
                    else {
                        if let Some(p) = outpath.parent() {
                            if !p.exists() {
                                std::fs::create_dir_all(&p).unwrap();
                            }
                        }
                        let mut outfile = File::create(&outpath).unwrap();
                        std::io::copy(&mut file, &mut outfile).unwrap();
                    }
                }
            
                // mark as "applied"
                d.applied = true;

                // update file
                self.save_or_update(&d);

                // TODO: return something meaningful?
                Ok(1)        
            },
            None => {

                Err(format!("Update {} not known", id))
            }
        }
    }

    pub async fn retrieve_update(&self, id: &String) -> Result<u64, String> {
        
        // is this an update we know about?
        let update = self.updates.get(id);
        match update {
            Some(u) => {
               let mut d = u.lock().unwrap();

                let mut sanitized_url = (&d.mpak_download_url).to_string();
                if !sanitized_url.starts_with("http") {
                    // TODO: support auth/https
                    sanitized_url.insert_str(0, "http://");

                }

                match reqwest::get(&sanitized_url).await {
                    Ok(response) => {
                        // determine where to store the mpak - we will extract on apply
                        let file_name = format!("/home/ctacke/{}.mpak", d.mpak_id);

                        // download the update
                        println!("downloading {} to {}", sanitized_url, file_name);
                        
                        let mut file = File::create(file_name).unwrap();

                        match response.bytes().await {
                            Ok(data) => {
                                let mut content = Cursor::new(data);
                                let size = copy(&mut content, &mut file).unwrap();
                
                                // set the update as retrieved
                                d.retrieved = true;
                
                                // update file
                                self.save_or_update(d.deref());
                
                                // return the size?  file name?  something
                                Ok(size)
        
                            },
                            Err(e) => {
                                return Err(e.to_string());
                            }
                        }                                
                    },
                    Err(e) => {
                        return Err(e.to_string());
                    }
                }
            },
            None => {

                Err(format!("Update {} not known", id))
            }
        }
    }

    fn save_or_update(&self, descriptor: &UpdateDescriptor) {
        println!("{:?}", descriptor);

        // make sure subdir exists
        let mut path = Path::new(Self::STORE_ROOT_FOLDER).join(&descriptor.mpak_id);
        if ! path.exists() {
            fs::create_dir(&path).unwrap();
        }

        // serialize
        let json = serde_json::to_string_pretty(&descriptor).unwrap();

        // erase any existing file
        path.push(&Self::UPDATE_INFO_FILE_NAME);

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .unwrap();

        // save
        file.write_all(json.as_bytes()).unwrap();

    }
}