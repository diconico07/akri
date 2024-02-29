use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use akri_shared::{
    akri::{
        discovery_configuration::{DiscoveryConfiguration, DiscoveryProperty},
        instance::Instance,
    },
    k8s::crud::IntoApi,
};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::discovery_handler_manager::{
    discovery_handler_registry::DiscoveryHandlerRegistry, DiscoveryError,
};

use kube::{Resource, ResourceExt};
use kube_runtime::{
    controller::Action,
    reflector::{ObjectRef, Store},
    Controller,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    DiscoveryError(#[from] DiscoveryError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub trait DiscoveryConfigurationKubeClient:
    IntoApi<DiscoveryConfiguration> + IntoApi<Instance>
{
}

impl<T: IntoApi<DiscoveryConfiguration> + IntoApi<Instance>> DiscoveryConfigurationKubeClient
    for T
{
}

pub struct ControllerContext {
    pub instances_cache: Store<Instance>,
    pub dh_registry: Arc<dyn DiscoveryHandlerRegistry>,
    pub client: Arc<dyn DiscoveryConfigurationKubeClient>,
    pub agent_instance_name: String,
    pub error_backoffs: Mutex<HashMap<String, Duration>>,
}

pub async fn start_controller(
    ctx: Arc<ControllerContext>,
    rec: mpsc::Receiver<ObjectRef<DiscoveryConfiguration>>,
) {
    let api = ctx.client.all().as_inner();
    let controller = Controller::new(api, Default::default());

    info!("Starting DiscoveryConfiguration Controller");
    controller
        .graceful_shutdown_on(async {
            let mut signal =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            signal.recv().await;
        })
        .reconcile_on(tokio_stream::wrappers::ReceiverStream::new(rec))
        .run(reconcile, error_policy, ctx)
        .for_each(|_| futures::future::ready(()))
        .await;
    info!("Stopping DiscoveryConfiguration Controller");
}

pub async fn reconcile(
    dc: Arc<DiscoveryConfiguration>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, Error> {
    trace!("Reconciling {}", dc.name_any());
    let owner_ref = dc.controller_owner_ref(&()).unwrap();
    if dc.metadata.deletion_timestamp.is_some() {
        ctx.dh_registry.terminate_request(&dc.name_any()).await;

        ctx.client
            .all()
            .remove_finalizer(dc.as_ref(), &ctx.agent_instance_name)
            .await
            .map_err(|e| Error::Other(e.into()))?;

        return Ok(Action::await_change());
    }

    if !dc.finalizers().contains(&ctx.agent_instance_name) {
        ctx.client
            .all()
            .add_finalizer(dc.as_ref(), &ctx.agent_instance_name)
            .await
            .map_err(|e| Error::Other(e.into()))?
    }

    let dh_name = &dc.spec.discovery_handler_name;
    let dh_details = &dc.spec.discovery_details;
    let empty_vec = vec![];
    let dh_properties: &Vec<DiscoveryProperty> =
        dc.spec.discovery_properties.as_ref().unwrap_or(&empty_vec);
    let dh_extra_device_properties = dc.spec.extra_instances_properties.clone();

    let discovered_instances: Vec<Instance> =
        match ctx.dh_registry.get_request(&dc.name_any()).await {
            Some(req) => req
                .get_instances()?
                .into_iter()
                .map(|mut instance| {
                    // Add
                    instance.spec.nodes = vec![ctx.agent_instance_name.to_owned()];
                    instance.owner_references_mut().push(owner_ref.clone());
                    instance.spec.capacity = dc.spec.instances_capacity;
                    instance
                })
                .collect(),
            None => {
                ctx.dh_registry
                    .new_request(
                        &dc.name_any(),
                        dh_name,
                        dh_details,
                        dh_properties,
                        dh_extra_device_properties,
                    )
                    .await?;
                vec![]
            }
        };

    for instance in ctx.instances_cache.state() {
        if instance.owner_references().contains(&owner_ref)
            && !discovered_instances
                .iter()
                .any(|di| di.name_any() == instance.name_any())
        {
            delete_instance(
                ctx.client.as_ref(),
                instance.as_ref(),
                &ctx.agent_instance_name,
            )
            .await?
        }
    }

    for instance in discovered_instances {
        ctx.client
            .all()
            .apply(instance, &ctx.agent_instance_name)
            .await
            .map_err(|e| Error::Other(e.into()))?;
    }

    ctx.error_backoffs.lock().unwrap().remove(&dc.name_any());
    Ok(Action::requeue(Duration::from_secs(600)))
}

pub fn error_policy(
    dc: Arc<DiscoveryConfiguration>,
    error: &Error,
    ctx: Arc<ControllerContext>,
) -> Action {
    let mut error_backoffs = ctx.error_backoffs.lock().unwrap();
    let previous_duration = error_backoffs
        .get(&dc.name_any())
        .cloned()
        .unwrap_or(Duration::from_millis(500));
    let next_duration = previous_duration * 2;
    warn!(
        "Error during reconciliation for {}, retrying in {}s: {:?}",
        dc.name_any(),
        next_duration.as_secs_f32(),
        error
    );
    error_backoffs.insert(dc.name_any(), next_duration);
    Action::requeue(next_duration)
}

async fn delete_instance(
    client: &dyn DiscoveryConfigurationKubeClient,
    instance: &Instance,
    agent_instance_name: &String,
) -> Result<(), Error> {
    if instance.spec.nodes.contains(agent_instance_name) {
        let api = client.all();
        if instance.spec.nodes.len() == 1 {
            api.delete(&instance.name_any())
                .await
                .map_err(|e| Error::Other(e.into()))?;
            return Ok(());
        }
        let mut new_instance = instance.clone();
        new_instance.spec.nodes = vec![];
        api.apply(new_instance, agent_instance_name)
            .await
            .map_err(|e| Error::Other(e.into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use akri_shared::{
        akri::{discovery_configuration::DiscoveryConfigurationSpec, instance::InstanceSpec},
        k8s::crud::{Api, MockApi, MockIntoApi},
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    use kube::core::{ObjectMeta, Status};
    use mockall::predicate::eq;

    use crate::discovery_handler_manager::discovery_handler_registry::{
        MockDiscoveryHandlerRegistry, MockDiscoveryHandlerRequest,
    };

    use super::*;

    #[derive(Default)]
    pub struct MockDiscoveryConfigurationKubeClient {
        instance: MockIntoApi<Instance>,
        config: MockIntoApi<DiscoveryConfiguration>,
    }

    impl IntoApi<Instance> for MockDiscoveryConfigurationKubeClient {
        fn all(&self) -> Box<dyn Api<Instance>> {
            self.instance.all()
        }

        fn namespaced(&self, _namespace: &str) -> Box<dyn Api<Instance>> {
            panic!("Should never happen");
        }

        fn default_namespaced(&self) -> Box<dyn Api<Instance>> {
            panic!("Should never happen");
        }
    }

    impl IntoApi<DiscoveryConfiguration> for MockDiscoveryConfigurationKubeClient {
        fn all(&self) -> Box<dyn Api<DiscoveryConfiguration>> {
            self.config.all()
        }

        fn namespaced(&self, _namespace: &str) -> Box<dyn Api<DiscoveryConfiguration>> {
            panic!("Should never happen");
        }

        fn default_namespaced(&self) -> Box<dyn Api<DiscoveryConfiguration>> {
            panic!("Should never happen");
        }
    }

    #[test]
    fn test_error_policy() {
        let _ = env_logger::builder().is_test(true).try_init();
        let config_1 = Arc::new(DiscoveryConfiguration {
            metadata: ObjectMeta {
                name: Some("config-1".to_string()),
                ..Default::default()
            },
            spec: DiscoveryConfigurationSpec {
                discovery_handler_name: "debugEcho".to_string(),
                discovery_details: String::default(),
                discovery_properties: None,
                instances_capacity: 1,
                extra_instances_properties: Default::default(),
            },
        });
        let config_2 = Arc::new(DiscoveryConfiguration {
            metadata: ObjectMeta {
                name: Some("config-2".to_string()),
                ..Default::default()
            },
            spec: DiscoveryConfigurationSpec {
                discovery_handler_name: "debugEcho".to_string(),
                discovery_details: String::default(),
                discovery_properties: None,
                instances_capacity: 1,
                extra_instances_properties: Default::default(),
            },
        });

        let (store, _) = kube_runtime::reflector::store();

        let ctx = Arc::new(ControllerContext {
            instances_cache: store,
            dh_registry: Arc::new(MockDiscoveryHandlerRegistry::new()),
            client: Arc::new(MockDiscoveryConfigurationKubeClient::default()),
            agent_instance_name: "node-a".to_string(),
            error_backoffs: Default::default(),
        });

        assert_eq!(
            error_policy(
                config_1.clone(),
                &Error::Other(anyhow::anyhow!("Error")),
                ctx.clone()
            ),
            Action::requeue(Duration::from_secs(1))
        );
        assert_eq!(
            error_policy(
                config_1.clone(),
                &Error::Other(anyhow::anyhow!("Error")),
                ctx.clone()
            ),
            Action::requeue(Duration::from_secs(2))
        );
        assert_eq!(
            error_policy(
                config_1.clone(),
                &Error::Other(anyhow::anyhow!("Error")),
                ctx.clone()
            ),
            Action::requeue(Duration::from_secs(4))
        );

        assert_eq!(
            error_policy(
                config_2,
                &Error::Other(anyhow::anyhow!("Error")),
                ctx.clone()
            ),
            Action::requeue(Duration::from_secs(1))
        );

        assert_eq!(
            error_policy(config_1, &Error::Other(anyhow::anyhow!("Error")), ctx),
            Action::requeue(Duration::from_secs(8))
        );
    }

    #[tokio::test]
    async fn test_delete_instance_delete() {
        let instance = Instance {
            metadata: ObjectMeta {
                name: Some("instance-1".to_string()),
                ..Default::default()
            },
            spec: InstanceSpec {
                capacity: 1,
                configuration_name: Default::default(),
                cdi_name: Default::default(),
                broker_properties: Default::default(),
                shared: false,
                nodes: vec!["node-a".to_string()],
                device_usage: Default::default(),
                active_claims: Default::default(),
            },
        };

        let mut mock_client = MockDiscoveryConfigurationKubeClient::default();
        let mut mock_api = MockApi::new();
        let local_instance = instance.clone();
        mock_api
            .expect_delete()
            .with(eq("instance-1"))
            .returning(move |_| Ok(itertools::Either::Left(local_instance.clone())));
        mock_client
            .instance
            .expect_all()
            .return_once(|| Box::new(mock_api));

        assert!(
            delete_instance(&mock_client, &instance, &"node-a".to_string())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_delete_instance_remove_node() {
        let instance = Instance {
            metadata: ObjectMeta {
                name: Some("instance-1".to_string()),
                ..Default::default()
            },
            spec: InstanceSpec {
                capacity: 1,
                configuration_name: Default::default(),
                cdi_name: Default::default(),
                broker_properties: Default::default(),
                shared: false,
                nodes: vec!["node-a".to_string(), "node-b".to_string()],
                device_usage: Default::default(),
                active_claims: Default::default(),
            },
        };

        let mut mock_client = MockDiscoveryConfigurationKubeClient::default();
        let mut mock_api = MockApi::new();
        let local_instance = instance.clone();
        mock_api
            .expect_apply()
            .returning(move |_, _| Ok(local_instance.clone()));
        mock_client
            .instance
            .expect_all()
            .return_once(|| Box::new(mock_api));

        assert!(
            delete_instance(&mock_client, &instance, &"node-a".to_string())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_delete_instance_other_node() {
        let instance = Instance {
            metadata: ObjectMeta {
                name: Some("instance-1".to_string()),
                ..Default::default()
            },
            spec: InstanceSpec {
                capacity: 1,
                configuration_name: Default::default(),
                cdi_name: Default::default(),
                broker_properties: Default::default(),
                shared: false,
                nodes: vec!["node-b".to_string()],
                device_usage: Default::default(),
                active_claims: Default::default(),
            },
        };

        let mut mock_client = MockDiscoveryConfigurationKubeClient::default();
        let mock_api = MockApi::new();
        mock_client
            .instance
            .expect_all()
            .return_once(|| Box::new(mock_api));

        assert!(
            delete_instance(&mock_client, &instance, &"node-a".to_string())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_reconcile_nothing_to_do() {
        let (store, _) = kube_runtime::reflector::store();
        let mut client = MockDiscoveryConfigurationKubeClient::default();
        let api = MockApi::new();
        client.config.expect_all().return_once(|| Box::new(api));

        let mut registry = MockDiscoveryHandlerRegistry::new();
        let mut request = MockDiscoveryHandlerRequest::new();
        request.expect_get_instances().returning(|| Ok(vec![]));
        registry
            .expect_get_request()
            .return_once(|_| Some(Arc::new(request)));

        let ctx = Arc::new(ControllerContext {
            instances_cache: store,
            dh_registry: Arc::new(registry),
            client: Arc::new(client),
            agent_instance_name: "node-a".to_string(),
            error_backoffs: Default::default(),
        });

        let dc = Arc::new(DiscoveryConfiguration {
            metadata: ObjectMeta {
                name: Some("config-1".to_string()),
                uid: Some("00112233-4455-6677-8899-aabbccddeeff".to_string()),
                finalizers: Some(vec!["node-a".to_string()]),
                ..Default::default()
            },
            spec: DiscoveryConfigurationSpec {
                discovery_handler_name: "debugEcho".to_string(),
                discovery_details: String::new(),
                discovery_properties: None,
                instances_capacity: 1,
                extra_instances_properties: Default::default(),
            },
        });

        assert!(reconcile(dc, ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_reconcile_no_request_existing_instances() {
        let (store, mut writer) = kube_runtime::reflector::store();
        writer.apply_watcher_event(&kube_runtime::watcher::Event::Restarted(vec![
            Instance {
                metadata: ObjectMeta {
                    name: Some("instance-1".to_string()),
                    owner_references: Some(vec![OwnerReference {
                        api_version: "akri.sh/v0".to_string(),
                        block_owner_deletion: None,
                        controller: Some(true),
                        kind: "Configuration".to_string(),
                        name: "config-1".to_string(),
                        uid: "00112233-4455-6677-8899-aabbccddeeff".to_string(),
                    }]),
                    ..Default::default()
                },
                spec: InstanceSpec {
                    configuration_name: "config-1".to_string(),
                    cdi_name: "akri.sh/config-1=abcdef".to_string(),
                    capacity: 1,
                    broker_properties: HashMap::new(),
                    shared: true,
                    nodes: vec!["node-a".to_string()],
                    device_usage: Default::default(),
                    active_claims: Default::default(),
                },
            },
            Instance {
                metadata: ObjectMeta {
                    name: Some("instance-2".to_string()),
                    owner_references: Some(vec![OwnerReference {
                        api_version: "akri.sh/v0".to_string(),
                        block_owner_deletion: None,
                        controller: Some(true),
                        kind: "Configuration".to_string(),
                        name: "config-1".to_string(),
                        uid: "00112233-4455-6677-8899-aabbccddeeff".to_string(),
                    }]),
                    ..Default::default()
                },
                spec: InstanceSpec {
                    configuration_name: "config-1".to_string(),
                    cdi_name: "akri.sh/config-1=abcdef".to_string(),
                    capacity: 1,
                    broker_properties: HashMap::new(),
                    shared: true,
                    nodes: vec!["node-b".to_string()],
                    device_usage: Default::default(),
                    active_claims: Default::default(),
                },
            },
            Instance {
                metadata: ObjectMeta {
                    name: Some("instance-3".to_string()),
                    owner_references: Some(vec![OwnerReference {
                        api_version: "akri.sh/v0".to_string(),
                        block_owner_deletion: None,
                        controller: Some(true),
                        kind: "Configuration".to_string(),
                        name: "config-2".to_string(),
                        uid: "11112233-4455-6677-8899-aabbccddeeff".to_string(),
                    }]),
                    ..Default::default()
                },
                spec: InstanceSpec {
                    configuration_name: "config-2".to_string(),
                    cdi_name: "akri.sh/config-2=abcdef".to_string(),
                    capacity: 1,
                    broker_properties: HashMap::new(),
                    shared: true,
                    nodes: vec!["node-a".to_string()],
                    device_usage: Default::default(),
                    active_claims: Default::default(),
                },
            },
        ]));
        let mut client = MockDiscoveryConfigurationKubeClient::default();
        let mut api = MockApi::new();
        api.expect_add_finalizer().returning(|_, _| Ok(()));
        client.config.expect_all().return_once(|| Box::new(api));

        let mut instance_api = MockApi::new();
        instance_api
            .expect_delete()
            .with(eq("instance-1"))
            .returning(|_| Ok(itertools::Either::Right(Status::default())));
        client
            .instance
            .expect_all()
            .return_once(|| Box::new(instance_api));

        let mut registry = MockDiscoveryHandlerRegistry::new();
        registry.expect_get_request().return_once(|_| None);
        //TODO: check arguments here
        registry
            .expect_new_request()
            .returning(|_, _, _, _, _| Ok(()));

        let ctx = Arc::new(ControllerContext {
            instances_cache: store,
            dh_registry: Arc::new(registry),
            client: Arc::new(client),
            agent_instance_name: "node-a".to_string(),
            error_backoffs: Default::default(),
        });

        let dc = Arc::new(DiscoveryConfiguration {
            metadata: ObjectMeta {
                name: Some("config-1".to_string()),
                uid: Some("00112233-4455-6677-8899-aabbccddeeff".to_string()),
                ..Default::default()
            },
            spec: DiscoveryConfigurationSpec {
                discovery_handler_name: "debugEcho".to_string(),
                discovery_details: String::new(),
                discovery_properties: None,
                instances_capacity: 1,
                extra_instances_properties: Default::default(),
            },
        });

        assert!(reconcile(dc, ctx).await.is_ok());
    }
}
