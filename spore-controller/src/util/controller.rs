use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use akri_shared::{
    akri::{configuration::Configuration, instance::Instance, spore::Spore},
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
                .get_metadata_watcher_resource(&ar, Default::default())
                .touched_objects(),
            Default::default(),
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
            Default::default(),
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
                ObjectRef::<Configuration>::from_owner_ref(None, &o, Default::default())
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
    namespace: &String,
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
        new_resource.meta_mut().namespace = Some(namespace.clone());
        ki.apply_resource(&new_resource, "akri-spore-controller")
            .await?;
    }
    Ok(())
}

async fn delete_resources(
    ctx: &Context,
    data: &serde_json::Value,
    resources: impl Iterator<Item = &DynamicObject>,
    namespace: &String,
) -> Result<(), Error> {
    let ki = ctx.kube_interface.clone();
    for resource in resources {
        let mut new_resource = render_object(resource, data)?;
        new_resource.meta_mut().namespace = Some(namespace.clone());
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
