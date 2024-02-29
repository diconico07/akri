use akri_shared::akri::instance_filter;
use kube::CustomResourceExt;

pub fn main() {
    println!(
        "{}",
        serde_yaml::to_string(&instance_filter::InstanceFilter::crd()).unwrap()
    );
}
