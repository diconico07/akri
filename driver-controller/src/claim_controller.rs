use std::{ops::Deref, sync::Arc, time::Duration};

use akri_shared::{
    akri::{instance::Instance, instance_filter::InstanceFilter},
    k8s::crud::IntoApi,
};
use futures::StreamExt;
use k8s_openapi::api::{
    core::v1::{NodeSelector, NodeSelectorRequirement, NodeSelectorTerm},
    resource::v1alpha2::{AllocationResult, ResourceClaim, ResourceClass, ResourceHandle},
};
use kube::{
    api::{Patch, PatchParams},
    ResourceExt,
};
use kube_runtime::{
    controller::Action,
    reflector::{ObjectRef, Store},
    Controller,
};
use log::{debug, trace};
use serde_json::json;
use thiserror;
use tokio::sync::mpsc::Sender;

use crate::common::ClaimHandle;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("Invalid claim object")]
    InvalidObject,
    #[error("Matching class not found")]
    ClassNotFound,
    #[error("Specified InstanceFilter not found")]
    FilterNotFound,
    #[error("No Instance matches the claim")]
    NoMatchingInstance,
    #[error(transparent)]
    KubeError(#[from] kube::Error),
}

pub trait ClaimDriverClient: IntoApi<ResourceClaim> + IntoApi<Instance> {}
impl<T: IntoApi<ResourceClaim> + IntoApi<Instance>> ClaimDriverClient for T {}

struct RcCtx {
    classes: Store<ResourceClass>,
    filters: Store<InstanceFilter>,
    instances: Store<Instance>,
    client: Arc<dyn ClaimDriverClient>,
    release_trigger: Sender<()>,
}

pub async fn start_controller(
    controller: Controller<ResourceClaim>,
    client: Arc<dyn ClaimDriverClient>,
    classes: Store<ResourceClass>,
    filters: Store<InstanceFilter>,
    instances: Store<Instance>,
    release_trigger: Sender<()>,
) {
    let context = Arc::new(RcCtx {
        classes,
        filters,
        instances,
        client: client.clone(),
        release_trigger,
    });
    controller
        .run(reconcile, error_policy, context)
        .for_each(|_| async {})
        .await;
}

async fn reconcile(claim: Arc<ResourceClaim>, ctx: Arc<RcCtx>) -> Result<Action, Error> {
    trace!("Reconciling {:?}::{}", claim.namespace(), claim.name_any());
    let status = claim.status.as_ref().cloned().unwrap_or_default();
    if claim.metadata.deletion_timestamp.is_some()
        || status.deallocation_requested.is_some_and(|r| r)
    {
        // Let's handle de-allocation if its ours
        if claim
            .metadata
            .finalizers
            .as_ref()
            .is_some_and(|f| f.contains(&"akri.sh/driver-controller".to_string()))
        {
            let handle: ClaimHandle = serde_json::from_str(
                &claim
                    .status
                    .as_ref()
                    .ok_or(Error::InvalidObject)?
                    .allocation
                    .as_ref()
                    .ok_or(Error::InvalidObject)?
                    .resource_handles
                    .as_ref()
                    .ok_or(Error::InvalidObject)?
                    .first()
                    .ok_or(Error::InvalidObject)?
                    .data
                    .as_ref()
                    .ok_or(Error::InvalidObject)?,
            )
            .map_err(|_| Error::InvalidObject)?;
            let instance_api: Box<dyn akri_shared::k8s::crud::Api<Instance>> = ctx.client.all();
            let patch = Patch::Apply(json!({
                    "spec": {
                        "activeClaims": [],
                    },
            }));
            instance_api
                .raw_patch(
                    &handle.instance_name,
                    &patch,
                    &PatchParams::apply(&format!(
                        "akri.sh/driver-controller={}",
                        claim.uid().unwrap()
                    )),
                )
                .await?;
            let api = ctx
                .client
                .namespaced(&claim.namespace().unwrap_or_default());
            api.remove_finalizer(claim.deref(), "akri.sh/driver-controller")
                .await?;
            let _ = ctx.release_trigger.try_send(());
        }
    }

    if status.allocation.is_some() {
        return Ok(Action::requeue(Duration::from_secs(300)));
    }
    if claim.spec.allocation_mode != Some("Immediate".to_owned()) {
        return Ok(Action::await_change());
    }

    let Some(class) = ctx
        .classes
        .get(&ObjectRef::new(&claim.spec.resource_class_name))
    else {
        return Err(Error::ClassNotFound);
    };

    if class.driver_name != "akri.sh" {
        return Ok(Action::await_change());
    }

    let config_name = &class
        .parameters_ref
        .as_ref()
        .ok_or(Error::ClassNotFound)?
        .name;
    let filter = match claim.spec.parameters_ref.as_ref().cloned() {
        Some(pr) => ctx
            .filters
            .get(&ObjectRef::new(&pr.name).within(&claim.namespace().unwrap_or_default()))
            .ok_or(Error::FilterNotFound)?,
        None => Arc::new(Default::default()),
    };

    let best_instance = ctx
        .instances
        .state()
        .into_iter()
        .filter(|instance| &instance.spec.configuration_name == config_name)
        .filter(|instance| filter.matches(&instance))
        .fold(None, |best: Option<Arc<Instance>>, instance| {
            if let Some(best) = best {
                if (instance.spec.capacity - instance.spec.active_claims.len())
                    > (best.spec.capacity - best.spec.active_claims.len())
                {
                    Some(instance)
                } else {
                    Some(best)
                }
            } else {
                if (instance.spec.capacity - instance.spec.active_claims.len()) > 0 {
                    Some(instance)
                } else {
                    None
                }
            }
        })
        .ok_or(Error::NoMatchingInstance)?;

    let allocation_result = AllocationResult {
        available_on_nodes: Some(NodeSelector {
            node_selector_terms: vec![NodeSelectorTerm {
                match_fields: Some(vec![NodeSelectorRequirement {
                    key: "metadata.name".to_string(),
                    values: Some(best_instance.spec.nodes.clone()),
                    operator: "In".to_string(),
                }]),
                match_expressions: None,
            }],
        }),
        resource_handles: Some(vec![ResourceHandle {
            data: Some(
                serde_json::to_string(&ClaimHandle {
                    cdi_name: best_instance.spec.cdi_name.clone(),
                    instance_name: best_instance.name_any(),
                })
                .unwrap(),
            ),
            driver_name: Some("akri.sh".to_owned()),
        }]),
        shareable: Some(false),
    };
    let api = ctx.client.namespaced(&claim.namespace().unwrap());
    api.add_finalizer(claim.deref(), "akri.sh/driver-controller")
        .await?;

    let instance_api: Box<dyn akri_shared::k8s::crud::Api<Instance>> = ctx.client.all();
    let patch = Patch::Apply(json!({
            "metadata": {
                "resourceVersion": best_instance.resource_version().unwrap(), // We want to be sure to not exceed capacity
            },
            "spec": {
                "activeClaims": [claim.uid().unwrap()],
            },
    }));
    instance_api
        .raw_patch(
            &best_instance.name_any(),
            &patch,
            &PatchParams::apply(&format!(
                "akri.sh/driver-controller={}",
                claim.uid().unwrap()
            )),
        )
        .await?;

    let patch = Patch::Apply(json!({
        "status": {
            "allocation": allocation_result,
            "driverName": "akri.sh".to_string()
        },
    }));
    if let Err(e) = api
        .raw_patch_status(
            &claim.name_any(),
            &patch,
            &PatchParams::apply("akri.sh/driver-controller"),
        )
        .await
    {
        // We must rollback the claim allocation here
        let patch = Patch::Apply(json!({
                "spec": {
                    "activeClaims": [],
                },
        }));
        instance_api
            .raw_patch(
                &best_instance.name_any(),
                &patch,
                &PatchParams::apply(&format!(
                    "akri.sh/driver-controller={}",
                    claim.uid().unwrap()
                )),
            )
            .await?;
        return Err(e.into());
    }

    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(claim: Arc<ResourceClaim>, error: &Error, _ctx: Arc<RcCtx>) -> Action {
    debug!(
        "Error with reconciliation of {:?}::{} : {}",
        claim.namespace(),
        claim.name_any(),
        error
    );
    Action::requeue(Duration::from_secs(60))
}
