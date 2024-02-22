use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use akri_shared::{
    akri::{discovery_configuration::DiscoveryConfiguration, instance::Instance, spore::Spore},
    k8s,
};
use futures::{stream, StreamExt, TryStreamExt};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{
    core::{DynamicObject, TypeMeta},
    Api, Resource, ResourceExt,
};
use kube_runtime::{
    applier,
    controller::{trigger_owners, trigger_self, Action, ReconcileReason, ReconcileRequest},
    metadata_watcher,
    reflector::{reflector, store, ObjectRef, Store},
    utils::{CancelableJoinHandle, StreamBackoff},
    watcher, WatchStreamExt,
};
use log::{error, trace};
use serde_json::json;
use tokio::{
    runtime::Handle,
    sync::{mpsc::Sender, RwLock},
};

use super::{
    template::{self, render_object},
    watcher_streamer::WatcherStreamer,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    // Temporary Errors
    #[error("Unknown Error: {0:?}")] // Catch-all Error, let's assume it's temporary
    Anyhow(#[from] anyhow::Error),

    // Permanent Errors
    #[error("Template error: {0:?}")]
    Template(#[from] template::Error),
    #[error("Invalid object: {0}")]
    InvalidObject(String),

    #[error("Should Never Happen")]
    Never,
}

type TriggerStream = stream::BoxStream<'static, Result<ReconcileRequest<Spore>, watcher::Error>>;

pub struct Controller {
    trigger_selector: stream::SelectAll<TriggerStream>,
    reader: Store<Spore>,
    context: Arc<Context>,
}

struct Context {
    watched_configurations: Arc<RwLock<HashMap<String, HashSet<ObjectRef<Spore>>>>>,
    kube_interface: Arc<dyn k8s::KubeInterface>,
    type_watcher: Arc<RwLock<TypeWatcher>>,
}

struct TypeWatcher {
    watched: HashSet<TypeMeta>,
    watch_sender: Sender<TriggerStream>,
}

impl Context {
    async fn register_type(&self, new_type: &TypeMeta) -> Result<(), Error> {
        let mut wt_write = self.type_watcher.write().await;
        if wt_write.watched.contains(new_type) {
            return Ok(());
        }
        let ar = self.kube_interface.get_api_resource(new_type).await?;
        let new_trigger: TriggerStream = trigger_owners(
            self.kube_interface
                .get_metadata_watcher_resource(&ar, watcher::Config::default())
                .touched_objects(),
            (),
            ar,
        )
        .boxed();
        wt_write
            .watch_sender
            .send(new_trigger)
            .await
            .map_err(|_| anyhow::anyhow!(""))
            .expect("Watched resources receiver dropped, controller is unsound panicking");
        wt_write.watched.insert(new_type.clone());
        Ok(())
    }
}

impl Controller {
    pub async fn new(
        kube_interface: Arc<impl k8s::KubeInterface + 'static>,
    ) -> anyhow::Result<Self> {
        let (reader, writer) = store::<Spore>();
        let mut trigger_selector = stream::SelectAll::new();
        let spore_api = Api::<Spore>::all(kube_interface.get_kube_client());
        let self_watcher = trigger_self(
            reflector(writer, watcher(spore_api, Default::default())).applied_objects(),
            (),
        )
        .boxed();
        trigger_selector.push(self_watcher);
        let (sender, wt_stream) = WatcherStreamer::new();
        trigger_selector.push(wt_stream.boxed());
        let type_watcher = Arc::new(RwLock::new(TypeWatcher {
            watched: HashSet::new(),
            watch_sender: sender,
        }));
        let context = Arc::new(Context {
            watched_configurations: Arc::new(RwLock::new(HashMap::new())),
            kube_interface: kube_interface.clone(),
            type_watcher,
        });
        let local_watched_configurations = context.watched_configurations.clone();
        let instance_api = Api::<Instance>::all(kube_interface.get_kube_client());
        let instance_watcher = metadata_watcher(instance_api, Default::default())
            .touched_objects()
            .then(move |obj| {
                let local_watched_configurations = local_watched_configurations.clone();
                async move {
                    let meta = obj?;
                    let instance_obj_ref = ObjectRef::from_obj(&meta).erase();
                    let discovery_configuration_name = match meta.owner_references().first() {
                        Some(e) => e.name.clone(),
                        None => "".to_string(),
                    };
                    let iterator = stream::iter(
                        local_watched_configurations
                            .clone()
                            .read()
                            .await
                            .get(&discovery_configuration_name)
                            .into_iter()
                            .flatten()
                            .map(move |mapped_obj_ref| {
                                Ok(ReconcileRequest {
                                    obj_ref: mapped_obj_ref.clone(),
                                    reason: ReconcileReason::RelatedObjectUpdated {
                                        obj_ref: Box::new(instance_obj_ref.clone()),
                                    },
                                })
                            })
                            .collect::<Vec<Result<ReconcileRequest<Spore>, _>>>(),
                    );

                    Ok(iterator)
                }
            })
            .try_flatten()
            .boxed();

        trigger_selector.push(instance_watcher);
        Ok(Self {
            trigger_selector,
            reader,
            context,
        })
    }

    pub async fn run(self) {
        trace!("Running reconciler");
        applier(
            move |obj, ctx| CancelableJoinHandle::spawn(reconcile(obj, ctx), &Handle::current()),
            error_policy,
            self.context,
            self.reader,
            StreamBackoff::new(self.trigger_selector, watcher::DefaultBackoff::default()),
            Default::default(),
        )
        .for_each(|_| futures::future::ready(()))
        .await;
    }
}

async fn reconcile(obj: Arc<Spore>, ctx: Arc<Context>) -> Result<Action, Error> {
    trace!("Reconciling {}", obj);
    let namespace = obj.namespace().ok_or(Error::Never)?;
    let ki = ctx.kube_interface.clone();
    let instances: Vec<Instance> = ki
        .get_instances()
        .await?
        .into_iter()
        .filter(|i| {
            i.metadata.owner_references.iter().flatten().any(|o| {
                ObjectRef::<DiscoveryConfiguration>::from_owner_ref(None, o, ())
                    .is_some_and(|r| r.name == obj.spec.discovery_selector.name)
            })
        })
        .collect();

    let instance_finalizer_name =
        format!("akri.sh/spore_instance_{}", obj.uid().ok_or(Error::Never)?);

    // Correctly handle linked instance watches, use a finalizer to ensure we stop watching for no longer interesting instances
    match obj.metadata.deletion_timestamp {
        Some(_) => {
            let mut write_watched_configurations = ctx.watched_configurations.write().await;
            if let Some(set) =
                write_watched_configurations.get_mut(&obj.spec.discovery_selector.name)
            {
                set.remove(&ObjectRef::from_obj(&obj));
                if set.is_empty() {
                    write_watched_configurations.remove(&obj.spec.discovery_selector.name);
                }
            }
            for instance in instances {
                ki.remove_instance_finalizer(&instance, &instance_finalizer_name)
                    .await?;
            }
            ki.remove_spore_finalizer(obj.as_ref(), "akri.sh/spore_controller")
                .await?;

            return Ok(Action::await_change());
        }
        None => {
            let mut write_watched_configurations = ctx.watched_configurations.write().await;
            let spore_ref = ObjectRef::from_obj(obj.as_ref());
            match write_watched_configurations.get_mut(&obj.spec.discovery_selector.name) {
                None => {
                    write_watched_configurations
                        .insert(obj.spec.discovery_selector.name.clone(), [spore_ref].into());
                }
                Some(set) => {
                    set.insert(spore_ref);
                }
            }
            ki.add_spore_finalizer(obj.as_ref(), "akri.sh/spore_controller")
                .await?;
        }
    }

    let owner_ref = obj.controller_owner_ref(&()).ok_or(Error::Never)?;
    let mut labels: HashMap<String, String> = HashMap::from([
        (
            "app.kubernetes.io/managed-by".to_string(),
            "akri".to_string(),
        ),
        ("akri.sh/spore".to_string(), obj.name_any()),
    ]);

    if !instances.is_empty() {
        let properties: Vec<serde_json::Value> = instances
            .iter()
            .map(|i| {
                json!({
                    "NAME": i.name_any(),
                    "NODES": i.spec.nodes,
                    "PROPERTIES": i.spec.broker_properties,
                })
            })
            .collect();
        let data = json!({
            "LABEL_SELECTOR": {"akri.sh/spore": obj.name_any()},
            "INSTANCES": properties,
        });
        trace!("{}", data);
        if instances
            .iter()
            .all(|instance| instance.metadata.deletion_timestamp.is_some())
        {
            delete_resources(
                &ctx,
                &data,
                obj.spec.once_spore.iter().flatten(),
                &namespace,
            )
            .await?;
        } else {
            apply_resources(
                &ctx,
                &data,
                obj.spec.once_spore.iter().flatten(),
                &owner_ref,
                &labels,
                &namespace,
            )
            .await?;
        }
    }

    for instance in instances {
        let data = json!({
            "LABEL_SELECTOR": {"akri.sh/spore": obj.name_any(), "akri.sh/instance": instance.name_any()},
            "INSTANCE_NAME": instance.name_any(),
            "INSTANCE_PROPERTIES": instance.spec.broker_properties,
            "INSTANCE_NODES": instance.spec.nodes,
        });

        match instance.metadata.deletion_timestamp {
            None => {
                ki.add_instance_finalizer(&instance, &instance_finalizer_name)
                    .await?
            }
            Some(_) => {
                delete_resources(
                    &ctx,
                    &data,
                    obj.spec.device_spore.iter().flatten(),
                    &namespace,
                )
                .await?;
                ki.remove_instance_finalizer(&instance, &instance_finalizer_name)
                    .await?;
                continue;
            }
        };
        labels.insert("akri.sh/instance".to_string(), instance.name_any());
        apply_resources(
            &ctx,
            &data,
            obj.spec.device_spore.iter().flatten(),
            &owner_ref,
            &labels,
            &namespace,
        )
        .await?;
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}

async fn apply_resources(
    ctx: &Context,
    data: &serde_json::Value,
    resources: impl Iterator<Item = &DynamicObject>,
    owner: &OwnerReference,
    labels: &HashMap<String, String>,
    namespace: &str,
) -> Result<(), Error> {
    let ki = ctx.kube_interface.clone();
    for resource in resources {
        ctx.register_type(
            resource
                .types
                .as_ref()
                .ok_or(Error::InvalidObject("No valid type in object".to_string()))?,
        )
        .await?;
        let mut new_resource = render_object(resource, data)?;
        new_resource.owner_references_mut().push(owner.clone());
        new_resource.labels_mut().extend(labels.clone());
        new_resource.meta_mut().namespace = Some(namespace.to_string());
        ki.apply_resource(&new_resource, "akri-spore-controller")
            .await?;
    }
    Ok(())
}

async fn delete_resources(
    ctx: &Context,
    data: &serde_json::Value,
    resources: impl Iterator<Item = &DynamicObject>,
    namespace: &str,
) -> Result<(), Error> {
    let ki = ctx.kube_interface.clone();
    for resource in resources {
        let mut new_resource = render_object(resource, data)?;
        new_resource.meta_mut().namespace = Some(namespace.to_string());
        ki.delete_resource(&new_resource).await?;
    }
    Ok(())
}

/// error_policy handles errors from reconciliation it only requeue the object for now with:
///  - a 300 seconds wait for definitive errors
///  - a 5 seconds wait for non-definitive errors
/// TODO: Send some feedback to user on definitive errors
fn error_policy(object: Arc<Spore>, err: &Error, _ctx: Arc<Context>) -> Action {
    error!("Got error for {}: {}", object, err);
    match err {
        Error::Never | Error::Template(_) | Error::InvalidObject(_) => {
            Action::requeue(Duration::from_secs(300))
        }
        Error::Anyhow(_) => Action::requeue(Duration::from_secs(5)),
    }
}

#[cfg(test)]
mod tests {

    use akri_shared::{
        akri::instance::{InstanceList, InstanceSpec},
        k8s::MockKubeInterface,
        os::file,
    };
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use kube::{
        core::{ListMeta, PartialObjectMetaExt},
        discovery::ApiResource,
    };
    use tokio::sync::mpsc::Receiver;

    use super::*;

    fn create_type_watcher() -> (Arc<RwLock<TypeWatcher>>, Receiver<TriggerStream>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(10);
        let type_watcher = Arc::new(RwLock::new(TypeWatcher {
            watched: HashSet::new(),
            watch_sender: sender,
        }));
        (type_watcher, receiver)
    }

    fn setup_mock_register_type(mki: &mut MockKubeInterface) {
        mki.expect_get_api_resource().returning(|_| {
            Ok(ApiResource {
                group: "example.com".to_string(),
                kind: "Bar".to_string(),
                plural: "bars".to_string(),
                version: "v0".to_string(),
                api_version: "example.com/v0".to_string(),
            })
        });
        mki.expect_get_metadata_watcher_resource()
            .returning(move |_, _| {
                futures::stream::once(async {
                    Ok(watcher::Event::Applied(
                        kube::core::ObjectMeta {
                            name: Some("foo".to_string()),
                            namespace: Some("bar".to_string()),
                            uid: Some("1234".to_string()),
                            ..Default::default()
                        }
                        .into_response_partial(),
                    ))
                })
                .boxed()
            });
    }

    #[tokio::test]
    async fn test_register_type() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (type_watcher, mut rec) = create_type_watcher();
        let mut mki = MockKubeInterface::new();
        setup_mock_register_type(&mut mki);
        let context = Arc::new(Context {
            watched_configurations: Default::default(),
            kube_interface: Arc::new(mki),
            type_watcher,
        });

        context
            .register_type(&TypeMeta {
                api_version: "example.com/v0".to_string(),
                kind: "Bar".to_string(),
            })
            .await
            .unwrap();
        assert!(rec.try_recv().is_ok())
    }

    #[tokio::test]
    async fn test_reconcile_no_instances() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (type_watcher, _rec) = create_type_watcher();
        let mut mki = MockKubeInterface::new();
        mki.expect_get_instances().returning(|| {
            Ok(InstanceList {
                metadata: ListMeta::default(),
                items: Default::default(),
                types: TypeMeta::default(),
            })
        });
        mki.expect_add_spore_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        let context = Arc::new(Context {
            watched_configurations: Default::default(),
            kube_interface: Arc::new(mki),
            type_watcher,
        });
        let spore_json = file::read_file_to_string("../test/json/spore-a.json");
        let spore: Arc<Spore> = Arc::new(serde_json::from_str(&spore_json).unwrap());
        assert_eq!(
            reconcile(spore.clone(), context.clone()).await.unwrap(),
            Action::requeue(Duration::from_secs(300))
        );

        assert_eq!(
            context
                .watched_configurations
                .read()
                .await
                .get(&spore.spec.discovery_selector.name)
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_reconcile_no_instances_deleted() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (type_watcher, _rec) = create_type_watcher();
        let mut mki = MockKubeInterface::new();
        mki.expect_get_instances().returning(|| {
            Ok(InstanceList {
                metadata: ListMeta::default(),
                items: Default::default(),
                types: TypeMeta::default(),
            })
        });
        mki.expect_remove_spore_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        let context = Arc::new(Context {
            watched_configurations: Default::default(),
            kube_interface: Arc::new(mki),
            type_watcher,
        });
        let spore_json = file::read_file_to_string("../test/json/spore-a.json");
        let mut spore: Spore = serde_json::from_str(&spore_json).unwrap();
        spore.metadata.deletion_timestamp = Some(Time(Default::default()));
        let spore = Arc::new(spore);

        context.watched_configurations.write().await.insert(
            spore.spec.discovery_selector.name.clone(),
            HashSet::from([ObjectRef::from_obj(spore.as_ref())]),
        );

        assert_eq!(
            reconcile(spore.clone(), context.clone()).await.unwrap(),
            Action::await_change()
        );

        assert!(context
            .watched_configurations
            .read()
            .await
            .get(&spore.spec.discovery_selector.name)
            .is_none());
    }

    fn setup_get_instances(mki: &mut MockKubeInterface, deleted: bool) {
        let deletion_timestamp = match deleted {
            true => Some(Time(Default::default())),
            false => None,
        };
        mki.expect_get_instances().returning(move || {
            Ok(InstanceList {
                metadata: ListMeta::default(),
                types: TypeMeta::default(),
                items: vec![Instance {
                    metadata: kube::core::ObjectMeta {
                        name: Some("instance-a".to_string()),
                        uid: Some("5432".to_string()),
                        owner_references: Some(vec![OwnerReference {
                            api_version: "akri.sh/v1alpha1".to_string(),
                            kind: "DiscoveryConfiguration".to_string(),
                            name: "config-a".to_string(),
                            uid: "6789".to_string(),
                            ..Default::default()
                        }]),
                        deletion_timestamp: deletion_timestamp.clone(),
                        ..Default::default()
                    },
                    spec: InstanceSpec {
                        configuration_name: "config-a".to_string(),
                        broker_properties: Default::default(),
                        shared: false,
                        nodes: Default::default(),
                        device_usage: Default::default(),
                        capacity: 1,
                        cdi_name: "akri.sh/config-a=instance-a".to_owned(),
                    },
                }],
            })
        });
    }

    #[tokio::test]
    async fn test_reconcile_with_instances() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (type_watcher, _rec) = create_type_watcher();
        let mut mki = MockKubeInterface::new();
        setup_get_instances(&mut mki, false);
        mki.expect_add_spore_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        mki.expect_apply_resource()
            .times(3)
            .returning(|_, _| Ok(()));
        mki.expect_add_instance_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        setup_mock_register_type(&mut mki);
        let context = Arc::new(Context {
            watched_configurations: Default::default(),
            kube_interface: Arc::new(mki),
            type_watcher,
        });
        let spore_json = file::read_file_to_string("../test/json/spore-a.json");
        let spore: Arc<Spore> = Arc::new(serde_json::from_str(&spore_json).unwrap());
        assert_eq!(
            reconcile(spore.clone(), context.clone()).await.unwrap(),
            Action::requeue(Duration::from_secs(300))
        );
    }

    #[tokio::test]
    async fn test_reconcile_with_deleted_instances() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (type_watcher, _rec) = create_type_watcher();
        let mut mki = MockKubeInterface::new();
        setup_get_instances(&mut mki, true);
        mki.expect_add_spore_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        mki.expect_delete_resource().times(3).returning(|_| Ok(()));
        mki.expect_remove_instance_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        let context = Arc::new(Context {
            watched_configurations: Default::default(),
            kube_interface: Arc::new(mki),
            type_watcher,
        });
        let spore_json = file::read_file_to_string("../test/json/spore-a.json");
        let spore: Arc<Spore> = Arc::new(serde_json::from_str(&spore_json).unwrap());
        assert_eq!(
            reconcile(spore.clone(), context.clone()).await.unwrap(),
            Action::requeue(Duration::from_secs(300))
        );
    }

    #[tokio::test]
    async fn test_reconcile_with_instances_deleted() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (type_watcher, _rec) = create_type_watcher();
        let mut mki = MockKubeInterface::new();
        setup_get_instances(&mut mki, false);
        mki.expect_remove_spore_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        mki.expect_remove_instance_finalizer()
            .once()
            .returning(|_, _| Ok(()));
        let context = Arc::new(Context {
            watched_configurations: Default::default(),
            kube_interface: Arc::new(mki),
            type_watcher,
        });
        let spore_json = file::read_file_to_string("../test/json/spore-a.json");
        let mut spore: Spore = serde_json::from_str(&spore_json).unwrap();
        spore.metadata.deletion_timestamp = Some(Time(Default::default()));
        let spore = Arc::new(spore);
        assert_eq!(
            reconcile(spore.clone(), context.clone()).await.unwrap(),
            Action::await_change(),
        );
    }
}
