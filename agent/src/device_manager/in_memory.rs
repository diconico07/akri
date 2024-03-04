use std::collections::HashMap;

use super::{cdi, DeviceManager};
use tokio::sync::watch;

pub struct InMemoryManager {
    state: watch::Receiver<HashMap<String, cdi::Kind>>,
}

impl InMemoryManager {
    pub fn new(state: watch::Receiver<HashMap<String, cdi::Kind>>) -> Self {
        InMemoryManager { state }
    }
}

impl DeviceManager for InMemoryManager {
    /// This method resolves a device from its FQDN (i.e in the form akri.sh/configuration=id)
    /// It returns None if the device is not registered to the device manager
    /// If the device is registered, it resolves its properties by merging the device specific properties
    /// with the configuration (kind) level properties
    fn get(&self, fqdn: &str) -> Option<cdi::Device> {
        let (kind, id) = fqdn.split_once('=').unwrap();
        let state = self.state.borrow();
        let cdi_kind = state.get(kind)?;
        let mut device = cdi_kind.devices.iter().find(|dev| dev.name == id)?.clone();
        device.name = format!("{}-{}", kind, id);
        for edit in cdi_kind.container_edits.iter().cloned() {
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
        let (kind, id) = fqdn.split_once('=').unwrap();
        if let Some(k) = self.state.borrow().get(kind) {
            return k.devices.iter().any(|dev| dev.name == id);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager() {
        let (sender, rec) = watch::channel(Default::default());
        let manager = InMemoryManager::new(rec);

        assert!(!manager.has_device("akri.sh/any=device"));
        assert_eq!(manager.get("akri.sh/any=device"), None);

        let _ = sender.send(HashMap::from([(
            "akri.sh/device".to_string(),
            cdi::Kind {
                kind: "akri.sh/device".to_string(),
                devices: vec![cdi::Device {
                    name: "my-device".to_string(),
                    container_edits: cdi::ContainerEdit {
                        env: vec!["back=home".to_string()],
                        device_nodes: vec![cdi::DeviceNode {
                            path: "/device/level/path".to_string(),
                            ..Default::default()
                        }],
                        mounts: vec![cdi::Mount {
                            host_path: "/device/level/host/path".to_string(),
                            container_path: "/device/level/container/path".to_string(),
                            mount_type: None,
                            options: vec![],
                        }],
                        hooks: vec![cdi::Hook {
                            hook_name: "device_level".to_string(),
                            path: "some/path".to_string(),
                            args: vec![],
                            env: vec![],
                            timeout: None,
                        }],
                    },
                }],
                container_edits: Some(cdi::ContainerEdit {
                    env: vec!["hello=world".to_string()],
                    device_nodes: vec![cdi::DeviceNode {
                        path: "/conf/level/path".to_string(),
                        ..Default::default()
                    }],
                    mounts: vec![cdi::Mount {
                        host_path: "/conf/level/host/path".to_string(),
                        container_path: "/conf/level/container/path".to_string(),
                        mount_type: None,
                        options: vec![],
                    }],
                    hooks: vec![cdi::Hook {
                        hook_name: "config_level".to_string(),
                        path: "some/path".to_string(),
                        args: vec![],
                        env: vec![],
                        timeout: None,
                    }],
                }),
            },
        )]));

        let expected_device = cdi::Device {
            name: "akri.sh/device-my-device".to_string(),
            container_edits: cdi::ContainerEdit {
                env: vec!["back=home".to_string(), "hello=world".to_string()],
                device_nodes: vec![
                    cdi::DeviceNode {
                        path: "/device/level/path".to_string(),
                        ..Default::default()
                    },
                    cdi::DeviceNode {
                        path: "/conf/level/path".to_string(),
                        ..Default::default()
                    },
                ],
                mounts: vec![
                    cdi::Mount {
                        host_path: "/device/level/host/path".to_string(),
                        container_path: "/device/level/container/path".to_string(),
                        mount_type: None,
                        options: vec![],
                    },
                    cdi::Mount {
                        host_path: "/conf/level/host/path".to_string(),
                        container_path: "/conf/level/container/path".to_string(),
                        mount_type: None,
                        options: vec![],
                    },
                ],
                hooks: vec![
                    cdi::Hook {
                        hook_name: "device_level".to_string(),
                        path: "some/path".to_string(),
                        args: vec![],
                        env: vec![],
                        timeout: None,
                    },
                    cdi::Hook {
                        hook_name: "config_level".to_string(),
                        path: "some/path".to_string(),
                        args: vec![],
                        env: vec![],
                        timeout: None,
                    },
                ],
            },
        };

        assert!(manager.has_device("akri.sh/device=my-device"));
        assert_eq!(
            manager.get("akri.sh/device=my-device"),
            Some(expected_device)
        );
    }
}
