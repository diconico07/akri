use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufReader, BufWriter, Result},
    path::PathBuf,
};

use super::{cdi, DeviceManager};
use tokio::sync::watch;

pub struct FileBasedDeviceManager {
    path: PathBuf,
}

fn clean_cdi_files(path: &std::path::Path, to_keep: Vec<String>) {
    for file in fs::read_dir(path).unwrap() {
        if let Ok(file) = file {
            let file_name = file.file_name().to_string_lossy().to_string();
            if file_name.starts_with("akri.sh-") {
                if !to_keep.contains(&file_name) {
                    fs::remove_file(file.path()).unwrap();
                }
            }
        }
    }
}

impl FileBasedDeviceManager {
    pub fn new(cdi_path: PathBuf, mut state: watch::Receiver<HashMap<String, cdi::Kind>>) -> Self {
        let local_path = cdi_path.clone();
        tokio::spawn(async move {
            loop {
                {
                    let devices = state.borrow_and_update();
                    let mut current_files = vec![];
                    for (name, kind) in devices.iter() {
                        let file_name = format!("{}.json", name.replace("/", "-"));
                        current_files.push(file_name.clone());
                        let mut path = local_path.join(file_name.clone());
                        trace!("Creating/Updating for path {:?}", path);
                        path.set_extension("json.new");
                        let file = File::create(path.clone()).unwrap();
                        let writer = BufWriter::new(file);
                        serde_json::to_writer(writer, kind).unwrap();
                        let mut final_path = path.clone();
                        final_path.set_extension("");
                        std::fs::rename(path, final_path).unwrap();
                    }
                    clean_cdi_files(&local_path, current_files);
                }
                state.changed().await.unwrap();
            }
        });
        FileBasedDeviceManager { path: cdi_path }
    }

    fn inner_get(&self, fqdn: &str) -> Result<cdi::Kind> {
        let Some((path, device)) = fqdn_to_file(fqdn, &self.path) else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        // Open the file in read-only mode with buffer.
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        // Read the JSON contents of the file as an instance of `User`.
        let kind: cdi::Kind = serde_json::from_reader(reader)?;
        if kind.devices.iter().find(|dev| dev.name == device).is_some() {
            Ok(kind)
        } else {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }
}

impl DeviceManager for FileBasedDeviceManager {
    fn get(&self, fqdn: &str) -> Option<cdi::Device> {
        let Some((_, id)) = fqdn_to_file(fqdn, &self.path) else {
            return None;
        };
        let Ok(kind) = self.inner_get(fqdn) else {
            return None;
        };
        let mut device = kind.devices.iter().find(|dev| dev.name == id)?.clone();
        device.name = format!("{}-{}", kind.kind, id);
        for edit in kind.container_edits.iter().cloned() {
            device.container_edits.env.extend(edit.env);
            device
                .container_edits
                .device_nodes
                .extend(edit.device_nodes);
            device.container_edits.hooks.extend(edit.hooks);
            device.container_edits.mounts.extend(edit.mounts);
        }
        Some(device)
    }

    fn has_device(&self, fqdn: &str) -> bool {
        self.inner_get(fqdn).is_ok()
    }
}

fn fqdn_to_file<'a>(fqdn: &'a str, base: &PathBuf) -> Option<(PathBuf, &'a str)> {
    let Some((kind, device)) = fqdn.split_once('=') else {
        return None;
    };
    let path = base.join(format!("{}.json", kind.replace("/", "-")));
    Some((path, device))
}
