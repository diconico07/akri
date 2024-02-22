#[macro_use]
extern crate lazy_static;
mod util;

use akri_shared::{
    akri::{metrics::run_metrics_server, API_NAMESPACE},
    k8s,
};
use prometheus::IntGaugeVec;
use std::sync::Arc;
use util::{controller, node_watcher};

/// Length of time to sleep between controller system validation checks
pub const SYSTEM_CHECK_DELAY_SECS: u64 = 30;

lazy_static! {
    // Reports the number of Broker pods running, grouped by Configuration and Node
    pub static ref BROKER_POD_COUNT_METRIC: IntGaugeVec = prometheus::register_int_gauge_vec!("akri_broker_pod_count", "Akri Broker Pod Count", &["configuration", "node"]).unwrap();
}

/// This is the entry point for the controller.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
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

    let mut tasks = Vec::new();

    // Start server for prometheus metrics
    tasks.push(tokio::spawn(async move {
        run_metrics_server().await.unwrap();
    }));

    // Watch for node disappearance
    tasks.push(tokio::spawn({
        async move {
            let mut node_watcher = node_watcher::NodeWatcher::new();
            node_watcher.watch().await.unwrap();
        }
    }));

    tasks.push(tokio::spawn({
        async move {
            let client = Arc::new(k8s::KubeImpl::new().await.unwrap());
            let spore_controller = controller::Controller::new(client).await.unwrap();
            spore_controller.run().await;
        }
    }));

    futures::future::try_join_all(tasks).await?;

    log::info!("{} Controller end", API_NAMESPACE);
    Ok(())
}
