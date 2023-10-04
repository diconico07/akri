use akri_shared::akri::spore::Spore;
use kube::CustomResourceExt;
fn main() {
    print!("{}", serde_yaml::to_string(&Spore::crd()).unwrap())
}
