use std::{fmt::Debug, hash::Hash, sync::Arc};

use akri_shared::{akri::API_NAMESPACE, k8s::crud::IntoApi};
use anyhow::Ok;
use futures::{
    future::{join_all, ready},
    StreamExt,
};
use kube_runtime::{
    reflector::{self, Store},
    watcher, Controller, WatchStreamExt,
};
use serde::de::DeserializeOwned;
use tokio::task::JoinHandle;

mod claim_controller;
mod common;
mod pod_scheduling_controller;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    println!("{} Controller start", API_NAMESPACE);

    println!(
        "{} KUBERNETES_PORT found ... env_logger::init",
        API_NAMESPACE
    );
    env_logger::try_init()?;
    println!(
        "{} KUBERNETES_PORT found ... env_logger::init finished",
        API_NAMESPACE
    );

    log::info!("{} Controller logging started", API_NAMESPACE);

    let client = Arc::new(akri_shared::k8s::KubeImpl::new().await?);
    let (classes, classes_task) = start_reflector(client.clone());
    let (filters, filters_task) = start_reflector(client.clone());
    let (instances, instance_task) = start_reflector(client.clone());

    instances.wait_until_ready().await.unwrap();
    filters.wait_until_ready().await.unwrap();
    classes.wait_until_ready().await.unwrap();

    let claim_controller = Controller::new(client.all().as_inner(), watcher::Config::default());
    let claims = claim_controller.store();
    let claim_task = tokio::spawn(claim_controller::start_controller(
        claim_controller,
        client.clone(),
        classes.clone(),
        filters.clone(),
        instances.clone(),
    ));
    let psc_task = tokio::spawn(pod_scheduling_controller::start_controller(
        client.clone(),
        classes.clone(),
        filters.clone(),
        instances.clone(),
        claims,
    ));
    join_all([
        classes_task,
        filters_task,
        instance_task,
        claim_task,
        psc_task,
    ])
    .await;
    Ok(())
}

fn start_reflector<T: kube::Resource + Send + Sync + Clone + Debug + DeserializeOwned>(
    client: Arc<dyn IntoApi<T>>,
) -> (Store<T>, JoinHandle<()>)
where
    T::DynamicType: Hash + Eq + Clone + Default,
{
    let (reader, writer) = reflector::store();
    let api = client.all().as_inner();
    let rf = reflector::reflector(writer, watcher(api, Default::default()));
    let task = tokio::spawn(async {
        rf.applied_objects()
            .default_backoff()
            .for_each(|_| ready(()))
            .await
    });
    (reader, task)
}
