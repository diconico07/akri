use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use akri_shared::{
    akri::{instance::Instance, instance_filter::InstanceFilter},
    k8s::crud::{Api, IntoApi},
};
use futures::StreamExt;
use k8s_openapi::{
    api::{
        core::v1::{
            NodeSelector, NodeSelectorRequirement, NodeSelectorTerm, Pod, PodResourceClaim,
        },
        resource::v1alpha2::{
            AllocationResult, PodSchedulingContext, PodSchedulingContextStatus, ResourceClaim,
            ResourceClaimConsumerReference, ResourceClaimSchedulingStatus, ResourceClass,
            ResourceHandle,
        },
    },
    Resource,
};
use kube::{
    api::{Patch, PatchParams},
    ResourceExt,
};
use kube_runtime::{
    controller::Action,
    reflector::{ObjectRef, Store},
    watcher::Config,
    Controller,
};
use log::{debug, trace};
use serde_json::json;
use thiserror;

use crate::common::ClaimHandle;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("Invalid claim object")]
    InvalidObject,
    #[error("No Instance matches the claim")]
    NoMatchingInstance,
    #[error("Matching pod not found")]
    PodNotFound,
    #[error(transparent)]
    KubeError(#[from] kube::Error),
}

struct PscCtx {
    classes: Store<ResourceClass>,
    filters: Store<InstanceFilter>,
    instances: Store<Instance>,
    claims: Store<ResourceClaim>,
    client: Arc<dyn PscDriverClient>,
}

pub trait PscDriverClient:
    IntoApi<ResourceClaim> + IntoApi<Instance> + IntoApi<PodSchedulingContext> + IntoApi<Pod>
{
}
impl<
        T: IntoApi<ResourceClaim> + IntoApi<Instance> + IntoApi<PodSchedulingContext> + IntoApi<Pod>,
    > PscDriverClient for T
{
}

pub async fn start_controller(
    client: Arc<dyn PscDriverClient>,
    classes: Store<ResourceClass>,
    filters: Store<InstanceFilter>,
    instances: Store<Instance>,
    claims: Store<ResourceClaim>,
) {
    let context = Arc::new(PscCtx {
        classes,
        filters,
        instances,
        claims,
        client: client.clone(),
    });
    Controller::new(client.all().as_inner(), Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|_| async {})
        .await;
}

fn claim_is_ours(
    ctx: &PscCtx,
    pod_resource_claim: &PodResourceClaim,
    pod: &Pod,
) -> Option<(Arc<ResourceClaim>, String)> {
    let Some(source) = pod_resource_claim.source.clone() else {
        return None;
    };
    let Some(claim) = (if let Some(claim) = source.resource_claim_name {
        ctx.claims
            .get(&ObjectRef::new(&claim).within(&pod.namespace().unwrap_or_default()))
    } else if let Some(template) = source.resource_claim_template_name {
        trace!("Got template to handle: {}", template);
        let statuses = pod
            .status
            .as_ref()
            .cloned()
            .unwrap_or_default()
            .resource_claim_statuses
            .unwrap_or_default();
        trace!("Retrieved status : {:?}", statuses);
        let Some(prcs) = statuses
            .iter()
            .find(|st| st.name == pod_resource_claim.name)
        else {
            return None;
        };
        let Some(rc_name) = prcs.resource_claim_name.clone() else {
            return None;
        };
        ctx.claims
            .get(&ObjectRef::new(&rc_name).within(&pod.namespace().unwrap_or_default()))
    } else {
        None
    }) else {
        return None;
    };
    if claim.spec.allocation_mode == Some("Immediate".to_owned()) {
        // Not in WaitForConsumer mode
        return None;
    }
    if claim
        .status
        .as_ref()
        .is_some_and(|st| st.allocation.is_some())
    {
        // Already allocated
        return None;
    }
    let Some(class) = ctx
        .classes
        .get(&ObjectRef::new(&claim.spec.resource_class_name))
    else {
        return None;
    };
    if class.driver_name == "akri.sh" {
        let Some(params) = class.parameters_ref.clone() else {
            return None;
        };
        Some((claim, params.name))
    } else {
        None
    }
}

async fn reconcile(obj: Arc<PodSchedulingContext>, ctx: Arc<PscCtx>) -> Result<Action, Error> {
    debug!("Reconciling {:?}::{}", obj.namespace(), obj.name_any());
    let pod_api: Box<dyn Api<Pod>> = ctx
        .client
        .namespaced(&obj.namespace().as_ref().cloned().unwrap_or_default());
    let pod = pod_api
        .get(&obj.name_any())
        .await?
        .ok_or(Error::PodNotFound)?;
    let claims = pod
        .spec
        .clone()
        .unwrap_or_default()
        .resource_claims
        .unwrap_or_default()
        .into_iter()
        .filter_map(|prc| claim_is_ours(&ctx, &prc, &pod))
        .map(|(claim, dc)| {
            let filter = ctx.filters.get(
                &ObjectRef::new(
                    &claim
                        .spec
                        .parameters_ref
                        .as_ref()
                        .cloned()
                        .unwrap_or_default()
                        .name,
                )
                .within(&claim.namespace().unwrap_or_default()),
            );
            let instances: Vec<Arc<Instance>> = ctx
                .instances
                .state()
                .iter()
                .filter(|i| i.spec.configuration_name == dc)
                .filter(|i| match &filter {
                    Some(f) => f.matches(i),
                    None => true,
                })
                .filter(|i| i.spec.capacity > i.spec.active_claims.len())
                .cloned()
                .collect();
            (claim, instances)
        });

    let mut potential_nodes: HashSet<String> = HashSet::from_iter(
        obj.spec
            .potential_nodes
            .as_ref()
            .cloned()
            .unwrap_or_default(),
    );
    let suitable_nodes: HashMap<String, HashSet<String>> = claims
        .clone()
        .map(|(claim, instances)| {
            let nodes: HashSet<String> =
                HashSet::from_iter(instances.iter().flat_map(|i| i.spec.nodes.clone()));
            (claim.name_any(), nodes)
        })
        .collect();
    if let Some(selected_node) = obj.spec.selected_node.as_ref() {
        potential_nodes.insert(selected_node.clone());
        if suitable_nodes
            .iter()
            .all(|(_, nodes)| nodes.contains(selected_node))
        {
            let claim_api = ctx.client.namespaced(&obj.namespace().unwrap_or_default());
            let instance_api: Box<dyn Api<Instance>> = ctx.client.all();
            for (claim, instances) in claims {
                claim_api
                    .add_finalizer(claim.as_ref(), "akri.sh/driver-controller")
                    .await?;

                let suitable_instances = instances
                    .into_iter()
                    .filter(|i| i.spec.nodes.contains(selected_node));
                let best_instance = suitable_instances
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

                let allocation_result = AllocationResult {
                    available_on_nodes: Some(NodeSelector {
                        node_selector_terms: vec![NodeSelectorTerm {
                            match_fields: Some(vec![NodeSelectorRequirement {
                                key: "metadata.name".to_string(),
                                values: Some(vec![selected_node.to_string()]),
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
                let reserved_for = vec![ResourceClaimConsumerReference {
                    api_group: Some(Pod::GROUP.to_string()),
                    name: pod.name_any(),
                    resource: Pod::KIND.to_string(),
                    uid: pod.uid().ok_or(Error::InvalidObject)?,
                }];

                let patch = Patch::Apply(json!({
                    "status": {
                        "allocation": allocation_result,
                        "driverName": "akri.sh".to_string(),
                        "reservedFor": reserved_for,
                    }
                }));
                if let Err(e) = claim_api
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
            }
        }
    }

    let status = PodSchedulingContextStatus {
        resource_claims: Some(
            suitable_nodes
                .iter()
                .map(|(claim, nodes)| ResourceClaimSchedulingStatus {
                    name: Some(claim.clone()),
                    unsuitable_nodes: Some(potential_nodes.difference(nodes).cloned().collect()),
                })
                .collect(),
        ),
    };

    let psc_api: Box<dyn Api<PodSchedulingContext>> =
        ctx.client.namespaced(&obj.namespace().unwrap_or_default());
    psc_api
        .raw_patch_status(
            &obj.name_any(),
            &Patch::Apply(json!({"status": status})),
            &PatchParams::apply("akri.sh/driver-controller"),
        )
        .await?;

    Ok(Action::requeue(Duration::from_secs(3600)))
}

fn error_policy(claim: Arc<PodSchedulingContext>, error: &Error, _ctx: Arc<PscCtx>) -> Action {
    debug!(
        "Error with reconciliation of {:?}::{} : {}",
        claim.namespace(),
        claim.name_any(),
        error
    );
    Action::requeue(Duration::from_secs(60))
}
