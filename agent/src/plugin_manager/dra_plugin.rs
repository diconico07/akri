use crate::device_manager::DeviceManager;
use akri_shared::{dra::ClaimHandle, uds::unix_stream};
use async_trait::async_trait;
use futures::TryFutureExt;
use tokio::net::UnixListener;
use tonic::transport::Server;

use std::{
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

use super::{
    pluginregistration::{
        registration_server::{Registration, RegistrationServer},
        InfoRequest, PluginInfo, RegistrationStatus, RegistrationStatusResponse,
    },
    v1alpha3::{
        node_server::{Node, NodeServer},
        NodePrepareResourceResponse, NodePrepareResourcesRequest, NodePrepareResourcesResponse,
        NodeUnprepareResourceResponse, NodeUnprepareResourcesRequest,
        NodeUnprepareResourcesResponse,
    },
};

struct DraPlugin {
    device_manager: Arc<dyn DeviceManager>,
}

struct PluginRegistrar {
    endpoint: PathBuf,
    plugin_status: AtomicBool,
}

pub async fn start_plugin(device_manager: Arc<dyn DeviceManager>) {
    let registration_endpoint_path = PathBuf::from("/var/lib/kubelet/plugins_registry/akri.sock");
    let plugin_endpoint_path = PathBuf::from("/var/lib/kubelet/plugins/akri/plugin.sock");
    let _ = tokio::fs::remove_file(&registration_endpoint_path).await;
    let _ = tokio::fs::remove_file(&plugin_endpoint_path).await;
    tokio::fs::create_dir_all(Path::new(&plugin_endpoint_path).parent().unwrap())
        .await
        .expect("Failed to create dir at socket path");
    let registrar = PluginRegistrar {
        endpoint: plugin_endpoint_path.clone(),
        plugin_status: AtomicBool::default(),
    };
    let registrar_server = RegistrationServer::new(registrar);
    let plugin = DraPlugin { device_manager };
    let plugin_server = NodeServer::new(plugin);
    tokio::task::spawn(async move {
        let socket_to_delete = plugin_endpoint_path.clone();
        let incoming = {
            let uds =
                UnixListener::bind(plugin_endpoint_path).expect("Failed to bind to socket path");

            async_stream::stream! {
                loop {
                    let item = uds.accept().map_ok(|(st, _)| unix_stream::UnixStream(st)).await;
                    yield item;
                }
            }
        };
        Server::builder()
            .add_service(plugin_server)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
        trace!(
            "serve - gracefully shutdown ... deleting socket {:?}",
            socket_to_delete
        );
        // Socket may already be deleted in the case of the kubelet restart
        std::fs::remove_file(socket_to_delete).unwrap_or(());
    });

    tokio::task::spawn(async move {
        let socket_to_delete = registration_endpoint_path.clone();
        let incoming = {
            let uds = UnixListener::bind(registration_endpoint_path)
                .expect("Failed to bind to socket path");

            async_stream::stream! {
                loop {
                    let item = uds.accept().map_ok(|(st, _)| unix_stream::UnixStream(st)).await;
                    yield item;
                }
            }
        };
        Server::builder()
            .add_service(registrar_server)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
        trace!(
            "serve - gracefully shutdown ... deleting socket {:?}",
            socket_to_delete
        );
        // Socket may already be deleted in the case of the kubelet restart
        std::fs::remove_file(socket_to_delete).unwrap_or(());
    });
}

#[async_trait]
impl Registration for PluginRegistrar {
    async fn get_info(
        &self,
        _request: tonic::Request<InfoRequest>,
    ) -> Result<tonic::Response<PluginInfo>, tonic::Status> {
        trace!("Plugin Info called");
        Ok(tonic::Response::new(PluginInfo {
            name: "akri.sh".to_string(),
            endpoint: self.endpoint.to_string_lossy().to_string(),
            r#type: "DRAPlugin".to_string(),
            supported_versions: vec!["1.0.0".to_string()],
        }))
    }

    async fn notify_registration_status(
        &self,
        request: tonic::Request<RegistrationStatus>,
    ) -> std::result::Result<tonic::Response<RegistrationStatusResponse>, tonic::Status> {
        info!("Plugin registered: {:?}", request);
        self.plugin_status.store(
            request.into_inner().plugin_registered,
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(tonic::Response::new(RegistrationStatusResponse {}))
    }
}

#[async_trait]
impl Node for DraPlugin {
    async fn node_prepare_resources(
        &self,
        request: tonic::Request<NodePrepareResourcesRequest>,
    ) -> std::result::Result<tonic::Response<NodePrepareResourcesResponse>, tonic::Status> {
        debug!("Got prepare request: {:?}", request);
        let cdis = request
            .into_inner()
            .claims
            .into_iter()
            .map(|c| {
                let cdi = match serde_json::from_str::<ClaimHandle>(&c.resource_handle) {
                    Ok(handle) => {
                        if self.device_manager.has_device(&handle.cdi_name) {
                            NodePrepareResourceResponse {
                                cdi_devices: vec![handle.cdi_name],
                                error: Default::default(),
                            }
                        } else {
                            NodePrepareResourceResponse {
                                cdi_devices: Default::default(),
                                error: "CDI device not found on node".to_string(),
                            }
                        }
                    }
                    Err(e) => NodePrepareResourceResponse {
                        cdi_devices: Default::default(),
                        error: e.to_string(),
                    },
                };
                (c.uid, cdi)
            })
            .collect();
        debug!("Answering to prepare request: {:?}", cdis);
        Ok(tonic::Response::new(NodePrepareResourcesResponse {
            claims: cdis,
        }))
    }

    async fn node_unprepare_resources(
        &self,
        request: tonic::Request<NodeUnprepareResourcesRequest>,
    ) -> std::result::Result<tonic::Response<NodeUnprepareResourcesResponse>, tonic::Status> {
        debug!("Got unprepare request: {:?}", request);
        let resp = request
            .into_inner()
            .claims
            .into_iter()
            .map(|c| {
                (
                    c.uid,
                    NodeUnprepareResourceResponse {
                        error: Default::default(),
                    },
                )
            })
            .collect();
        debug!("Answering to unprepare request: {:?}", resp);
        Ok(tonic::Response::new(NodeUnprepareResourcesResponse {
            claims: resp,
        }))
    }
}
