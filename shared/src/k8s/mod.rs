use std::convert::TryFrom;

use crate::akri::spore::Spore;

use super::akri::{
    instance,
    instance::{Instance, InstanceList, InstanceSpec},
    retry::{random_delay, MAX_INSTANCE_UPDATE_TRIES},
    API_NAMESPACE, API_VERSION,
};
use anyhow::anyhow;
use async_trait::async_trait;
use futures::StreamExt;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{Node, Pod, Service};
use kube::{
    api::{ObjectList, Patch, PatchParams},
    client::Client,
    core::{DynamicObject, GroupVersionKind, ObjectMeta, PartialObjectMetaExt, TypeMeta},
    discovery::{self, ApiResource},
    Api, Resource, ResourceExt,
};
use mockall::{automock, predicate::*};

pub mod crud;
pub mod job;
pub mod node;
pub mod pod;
pub mod service;

pub const NODE_SELECTOR_OP_IN: &str = "In";
pub const OBJECT_NAME_FIELD: &str = "metadata.name";
pub const RESOURCE_REQUIREMENTS_KEY: &str = "{{PLACEHOLDER}}";
pub const ERROR_NOT_FOUND: u16 = 404;
pub const ERROR_CONFLICT: u16 = 409;

/// OwnershipType defines what type of Kubernetes object
/// an object is dependent on
#[derive(Clone, Debug)]
pub enum OwnershipType {
    Configuration,
    Instance,
    Pod,
    Service,
}

/// OwnershipInfo provides enough information to identify
/// the Kubernetes object an object depends on
#[derive(Clone, Debug)]
pub struct OwnershipInfo {
    object_type: OwnershipType,
    object_uid: String,
    object_name: String,
}

impl OwnershipInfo {
    pub fn new(object_type: OwnershipType, object_name: String, object_uid: String) -> Self {
        OwnershipInfo {
            object_type,
            object_uid,
            object_name,
        }
    }

    pub fn get_api_version(&self) -> String {
        match self.object_type {
            OwnershipType::Instance | OwnershipType::Configuration => {
                format!("{}/{}", API_NAMESPACE, API_VERSION)
            }
            OwnershipType::Pod | OwnershipType::Service => "core/v1".to_string(),
        }
    }

    pub fn get_kind(&self) -> String {
        match self.object_type {
            OwnershipType::Instance => "Instance",
            OwnershipType::Configuration => "Configuration",
            OwnershipType::Pod => "Pod",
            OwnershipType::Service => "Service",
        }
        .to_string()
    }

    pub fn get_controller(&self) -> Option<bool> {
        Some(true)
    }

    pub fn get_block_owner_deletion(&self) -> Option<bool> {
        Some(true)
    }

    pub fn get_name(&self) -> String {
        self.object_name.clone()
    }

    pub fn get_uid(&self) -> String {
        self.object_uid.clone()
    }
}

#[automock]
#[async_trait]
pub trait KubeInterface: Send + Sync {
    fn get_kube_client(&self) -> Client;

    async fn find_node(&self, name: &str) -> Result<Node, anyhow::Error>;

    async fn find_pods_with_label(&self, selector: &str) -> Result<ObjectList<Pod>, anyhow::Error>;
    async fn find_pods_with_field(&self, selector: &str) -> Result<ObjectList<Pod>, anyhow::Error>;
    async fn create_pod(&self, pod_to_create: &Pod, namespace: &str) -> Result<(), anyhow::Error>;
    async fn remove_pod(&self, pod_to_remove: &str, namespace: &str) -> Result<(), anyhow::Error>;

    async fn find_jobs_with_label(&self, selector: &str) -> Result<ObjectList<Job>, anyhow::Error>;
    async fn find_jobs_with_field(&self, selector: &str) -> Result<ObjectList<Job>, anyhow::Error>;
    async fn create_job(&self, job_to_create: &Job, namespace: &str) -> Result<(), anyhow::Error>;
    async fn remove_job(&self, job_to_remove: &str, namespace: &str) -> Result<(), anyhow::Error>;

    async fn find_services(&self, selector: &str) -> Result<ObjectList<Service>, anyhow::Error>;
    async fn create_service(
        &self,
        svc_to_create: &Service,
        namespace: &str,
    ) -> Result<(), anyhow::Error>;
    async fn remove_service(
        &self,
        svc_to_remove: &str,
        namespace: &str,
    ) -> Result<(), anyhow::Error>;
    async fn update_service(
        &self,
        svc_to_update: &Service,
        name: &str,
        namespace: &str,
    ) -> Result<(), anyhow::Error>;

    async fn find_instance(&self, name: &str) -> Result<Instance, anyhow::Error>;
    async fn get_instances(&self) -> Result<InstanceList, anyhow::Error>;
    async fn create_instance(
        &self,
        instance_to_create: &InstanceSpec,
        name: &str,
        owner_config_name: &str,
        owner_config_uid: &str,
    ) -> Result<(), anyhow::Error>;
    async fn delete_instance(&self, name: &str) -> Result<(), anyhow::Error>;
    async fn update_instance(
        &self,
        instance_to_update: &InstanceSpec,
        name: &str,
    ) -> Result<(), anyhow::Error>;

    async fn apply_resource(
        &self,
        resource: &DynamicObject,
        field_manager: &str,
    ) -> anyhow::Result<()>;
    async fn delete_resource(&self, resource: &DynamicObject) -> anyhow::Result<()>;
    async fn add_spore_finalizer(&self, spore: &Spore, finalizer: &str) -> anyhow::Result<()>;
    async fn remove_spore_finalizer(&self, spore: &Spore, finalizer: &str) -> anyhow::Result<()>;
    async fn add_instance_finalizer(
        &self,
        instance: &Instance,
        finalizer: &str,
    ) -> anyhow::Result<()>;
    async fn remove_instance_finalizer(
        &self,
        instance: &Instance,
        finalizer: &str,
    ) -> anyhow::Result<()>;

    async fn get_api_resource(&self, type_meta: &TypeMeta) -> anyhow::Result<ApiResource>;
    fn get_metadata_watcher_resource(
        &self,
        dyntype: &ApiResource,
        watcher_config: kube_runtime::watcher::Config,
    ) -> futures::stream::BoxStream<
        'static,
        Result<
            kube_runtime::watcher::Event<kube::api::PartialObjectMeta<DynamicObject>>,
            kube_runtime::watcher::Error,
        >,
    >;
}

#[async_trait]
pub trait ResourceWithApi: Sized {
    async fn get_api(&self, kube_client: Client) -> anyhow::Result<Api<Self>>;
}

#[async_trait]
impl ResourceWithApi for DynamicObject {
    async fn get_api(&self, kube_client: Client) -> anyhow::Result<Api<Self>> {
        let gvk = GroupVersionKind::try_from(self.types.as_ref().unwrap())?;
        let (ar, _caps) = discovery::pinned_kind(&kube_client, &gvk).await?;
        Ok(match &self.meta().namespace {
            None => Api::all_with(kube_client, &ar),
            Some(ns) => Api::namespaced_with(kube_client, ns.as_str(), &ar),
        })
    }
}

#[async_trait]
impl ResourceWithApi for Spore {
    async fn get_api(&self, kube_client: Client) -> anyhow::Result<Api<Self>> {
        Ok(Api::namespaced(
            kube_client,
            self.meta()
                .namespace
                .as_ref()
                .ok_or(anyhow!("No namespace provided for Spore"))?,
        ))
    }
}

#[async_trait]
impl ResourceWithApi for Instance {
    async fn get_api(&self, kube_client: Client) -> anyhow::Result<Api<Self>> {
        Ok(Api::all(kube_client))
    }
}

#[derive(Clone)]
pub struct KubeImpl {
    client: kube::Client,
}

impl KubeImpl {
    /// Create new instance of KubeImpl
    pub async fn new() -> Result<Self, anyhow::Error> {
        Ok(KubeImpl {
            client: Client::try_default().await?,
        })
    }
}

#[async_trait]
impl KubeInterface for KubeImpl {
    /// Return of clone of KubeImpl's client
    fn get_kube_client(&self) -> Client {
        self.client.clone()
    }

    /// Get Kuberenetes node for specified name
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let node = kube.find_node("node-a").await.unwrap();
    /// # }
    /// ```
    async fn find_node(&self, name: &str) -> Result<Node, anyhow::Error> {
        node::find_node(name, self.get_kube_client()).await
    }

    /// Get Kuberenetes pods with specified label selector
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let interesting_pods = kube.find_pods_with_label("label=interesting").await.unwrap();
    /// # }
    /// ```
    async fn find_pods_with_label(&self, selector: &str) -> Result<ObjectList<Pod>, anyhow::Error> {
        pod::find_pods_with_selector(Some(selector.to_string()), None, self.get_kube_client()).await
    }
    /// Get Kuberenetes pods with specified field selector
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let pods_on_node_a = kube.find_pods_with_field("spec.nodeName=node-a").await.unwrap();
    /// # }
    /// ```
    async fn find_pods_with_field(&self, selector: &str) -> Result<ObjectList<Pod>, anyhow::Error> {
        pod::find_pods_with_selector(None, Some(selector.to_string()), self.get_kube_client()).await
    }
    /// Create Kuberenetes pod
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    /// use k8s_openapi::api::core::v1::Pod;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.create_pod(&Pod::default(), "pod_namespace").await.unwrap();
    /// # }
    /// ```
    async fn create_pod(&self, pod_to_create: &Pod, namespace: &str) -> Result<(), anyhow::Error> {
        pod::create_pod(pod_to_create, namespace, self.get_kube_client()).await
    }
    /// Remove Kubernetes pod
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.remove_pod("pod_to_remove", "pod_namespace").await.unwrap();
    /// # }
    /// ```
    async fn remove_pod(&self, pod_to_remove: &str, namespace: &str) -> Result<(), anyhow::Error> {
        pod::remove_pod(pod_to_remove, namespace, self.get_kube_client()).await
    }

    /// Find Kuberenetes Jobs with specified label selector
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let interesting_jobs = kube.find_jobs_with_label("label=interesting").await.unwrap();
    /// # }
    /// ```
    async fn find_jobs_with_label(&self, selector: &str) -> Result<ObjectList<Job>, anyhow::Error> {
        job::find_jobs_with_selector(Some(selector.to_string()), None, self.get_kube_client()).await
    }
    /// Find Kuberenetes Jobs with specified field selector
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let jobs_on_node_a = kube.find_jobs_with_field("spec.nodeName=node-a").await.unwrap();
    /// # }
    /// ```
    async fn find_jobs_with_field(&self, selector: &str) -> Result<ObjectList<Job>, anyhow::Error> {
        job::find_jobs_with_selector(None, Some(selector.to_string()), self.get_kube_client()).await
    }

    /// Create Kuberenetes job
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    /// use k8s_openapi::api::batch::v1::Job;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.create_job(&Job::default(), "job_namespace").await.unwrap();
    /// # }
    /// ```
    async fn create_job(&self, job_to_create: &Job, namespace: &str) -> Result<(), anyhow::Error> {
        job::create_job(job_to_create, namespace, self.get_kube_client()).await
    }
    /// Remove Kubernetes job
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.remove_job("job_to_remove", "job_namespace").await.unwrap();
    /// # }
    /// ```
    async fn remove_job(&self, job_to_remove: &str, namespace: &str) -> Result<(), anyhow::Error> {
        job::remove_job(job_to_remove, namespace, self.get_kube_client()).await
    }

    /// Get Kuberenetes services with specified label selector
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let interesting_services = kube.find_services("label=interesting").await.unwrap();
    /// # }
    /// ```
    async fn find_services(&self, selector: &str) -> Result<ObjectList<Service>, anyhow::Error> {
        service::find_services_with_selector(selector, self.get_kube_client()).await
    }
    /// Create Kubernetes service
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    /// use k8s_openapi::api::core::v1::Service;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.create_service(&Service::default(), "service_namespace").await.unwrap();
    /// # }
    /// ```
    async fn create_service(
        &self,
        svc_to_create: &Service,
        namespace: &str,
    ) -> Result<(), anyhow::Error> {
        service::create_service(svc_to_create, namespace, self.get_kube_client()).await
    }
    /// Remove Kubernetes service
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.remove_service("service_to_remove", "service_namespace").await.unwrap();
    /// # }
    /// ```
    async fn remove_service(
        &self,
        svc_to_remove: &str,
        namespace: &str,
    ) -> Result<(), anyhow::Error> {
        service::remove_service(svc_to_remove, namespace, self.get_kube_client()).await
    }
    /// Update Kubernetes service
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    /// use k8s_openapi::api::core::v1::Service;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let selector = "environment=production,app=nginx";
    /// for svc in kube.find_services(&selector).await.unwrap() {
    ///     let svc_name = &svc.metadata.name.clone().unwrap();
    ///     let svc_namespace = &svc.metadata.namespace.as_ref().unwrap().clone();
    ///     let updated_svc = kube.update_service(
    ///         &svc,
    ///         &svc_name,
    ///         &svc_namespace).await.unwrap();
    /// }
    /// # }
    /// ```
    async fn update_service(
        &self,
        svc_to_update: &Service,
        name: &str,
        namespace: &str,
    ) -> Result<(), anyhow::Error> {
        service::update_service(svc_to_update, name, namespace, self.get_kube_client()).await
    }

    // Get Akri Instance with given name and namespace
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let instance = kube.find_instance("instance-1", "instance-namespace").await.unwrap();
    /// # }
    /// ```
    async fn find_instance(&self, name: &str) -> Result<Instance, anyhow::Error> {
        instance::find_instance(name, &self.get_kube_client()).await
    }
    // Get Akri Instances with given namespace
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// let instances = kube.get_instances().await.unwrap();
    /// # }
    /// ```
    async fn get_instances(&self) -> Result<InstanceList, anyhow::Error> {
        instance::get_instances(&self.get_kube_client()).await
    }

    /// Create Akri Instance
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    /// use akri_shared::akri::instance::InstanceSpec;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.create_instance(
    ///     &InstanceSpec{
    ///         configuration_name: "capability_configuration_name".to_string(),
    ///         cdi_name: "akri.sh/config-1=instance-1".to_string(),
    ///         capacity: 1,
    ///         shared: true,
    ///         nodes: Vec::new(),
    ///         device_usage: std::collections::HashMap::new(),
    ///         broker_properties: std::collections::HashMap::new(),
    ///     },
    ///     "instance-1",
    ///     "config-1",
    ///     "abcdefgh-ijkl-mnop-qrst-uvwxyz012345"
    /// ).await.unwrap();
    /// # }
    /// ```
    async fn create_instance(
        &self,
        instance_to_create: &InstanceSpec,
        name: &str,
        owner_config_name: &str,
        owner_config_uid: &str,
    ) -> Result<(), anyhow::Error> {
        instance::create_instance(
            instance_to_create,
            name,
            owner_config_name,
            owner_config_uid,
            &self.get_kube_client(),
        )
        .await
    }
    // Delete Akri Instance
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.delete_instance(
    ///     "instance-1",
    /// ).await.unwrap();
    /// # }
    /// ```
    async fn delete_instance(&self, name: &str) -> Result<(), anyhow::Error> {
        instance::delete_instance(name, &self.get_kube_client()).await
    }
    /// Update Akri Instance
    ///
    /// Example:
    ///
    /// ```no_run
    /// use akri_shared::k8s;
    /// use akri_shared::k8s::KubeInterface;
    /// use akri_shared::akri::instance::InstanceSpec;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let kube = k8s::KubeImpl::new().await.unwrap();
    /// kube.update_instance(
    ///     &InstanceSpec{
    ///         configuration_name: "capability_configuration_name".to_string(),
    ///         cdi_name: "akri.sh/capability_configuration_name=instance-1".to_string(),
    ///         capacity: 1,
    ///         shared: true,
    ///         nodes: Vec::new(),
    ///         device_usage: std::collections::HashMap::new(),
    ///         broker_properties: std::collections::HashMap::new(),
    ///     },
    ///     "instance-1",
    /// ).await.unwrap();
    /// # }
    /// ```
    async fn update_instance(
        &self,
        instance_to_update: &InstanceSpec,
        name: &str,
    ) -> Result<(), anyhow::Error> {
        instance::update_instance(instance_to_update, name, &self.get_kube_client()).await
    }

    async fn apply_resource(
        &self,
        resource: &DynamicObject,
        field_manager: &str,
    ) -> Result<(), anyhow::Error> {
        let api = resource.get_api(self.get_kube_client()).await?;
        let patch = Patch::Apply(resource);
        api.patch(
            resource.name_any().as_str(),
            &PatchParams::apply(field_manager),
            &patch,
        )
        .await?;
        Ok(())
    }

    async fn delete_resource(&self, resource: &DynamicObject) -> Result<(), anyhow::Error> {
        let api = resource.get_api(self.get_kube_client()).await?;
        api.delete(resource.name_any().as_str(), &Default::default())
            .await?;
        Ok(())
    }

    async fn add_spore_finalizer(&self, spore: &Spore, finalizer: &str) -> anyhow::Result<()> {
        let api = spore.get_api(self.get_kube_client()).await?;
        set_finalizers(
            &api,
            &spore.name_any(),
            Some(vec![finalizer.to_string()]),
            finalizer,
        )
        .await
    }
    async fn remove_spore_finalizer(&self, spore: &Spore, finalizer: &str) -> anyhow::Result<()> {
        let api = spore.get_api(self.get_kube_client()).await?;
        set_finalizers(&api, &spore.name_any(), None, finalizer).await
    }
    async fn add_instance_finalizer(
        &self,
        instance: &Instance,
        finalizer: &str,
    ) -> anyhow::Result<()> {
        let api = instance.get_api(self.get_kube_client()).await?;
        set_finalizers(
            &api,
            &instance.name_any(),
            Some(vec![finalizer.to_string()]),
            finalizer,
        )
        .await
    }
    async fn remove_instance_finalizer(
        &self,
        instance: &Instance,
        finalizer: &str,
    ) -> anyhow::Result<()> {
        let api = instance.get_api(self.get_kube_client()).await?;
        set_finalizers(&api, &instance.name_any(), None, finalizer).await
    }
    async fn get_api_resource(&self, type_meta: &TypeMeta) -> anyhow::Result<ApiResource> {
        let gvk = GroupVersionKind::try_from(type_meta)?;
        let (ar, _caps) = discovery::pinned_kind(&self.get_kube_client(), &gvk).await?;
        Ok(ar)
    }
    fn get_metadata_watcher_resource(
        &self,
        dyntype: &ApiResource,
        watcher_config: kube_runtime::watcher::Config,
    ) -> futures::stream::BoxStream<
        'static,
        Result<
            kube_runtime::watcher::Event<kube::api::PartialObjectMeta<DynamicObject>>,
            kube_runtime::watcher::Error,
        >,
    > {
        let dyn_api = Api::<DynamicObject>::all_with(self.get_kube_client(), dyntype);
        kube_runtime::metadata_watcher(dyn_api, watcher_config).boxed()
    }
}

async fn set_finalizers<
    T: Resource<DynamicType = ()> + Clone + serde::de::DeserializeOwned + std::fmt::Debug,
>(
    api: &Api<T>,
    name: &str,
    finalizers: Option<Vec<String>>,
    field_manager: &str,
) -> anyhow::Result<()> {
    let metadata = ObjectMeta {
        finalizers,
        ..Default::default()
    }
    .into_request_partial::<T>();
    api.patch_metadata(
        name,
        &PatchParams::apply(field_manager),
        &Patch::Apply(&metadata),
    )
    .await?;
    Ok(())
}

/// This deletes an Instance unless it has already been deleted by another node
/// or fails after multiple retries.
pub async fn try_delete_instance(
    kube_interface: &dyn KubeInterface,
    instance_name: &str,
) -> Result<(), anyhow::Error> {
    for x in 0..MAX_INSTANCE_UPDATE_TRIES {
        match kube_interface.delete_instance(instance_name).await {
            Ok(()) => {
                log::trace!("try_delete_instance - deleted Instance {}", instance_name);
                break;
            }
            Err(e) => {
                if let Some(ae) = e.downcast_ref::<kube::error::ErrorResponse>() {
                    if ae.code == ERROR_NOT_FOUND {
                        log::trace!(
                            "try_delete_instance - discovered Instance {} already deleted",
                            instance_name
                        );
                        break;
                    }
                }
                log::error!(
                    "try_delete_instance - tried to delete Instance {} but still exists. {} retries left.",
                    instance_name, MAX_INSTANCE_UPDATE_TRIES - x - 1
                );
                if x == MAX_INSTANCE_UPDATE_TRIES - 1 {
                    return Err(e);
                }
            }
        }
        random_delay().await;
    }
    Ok(())
}

#[cfg(test)]
pub mod test_ownership {
    use super::*;

    #[tokio::test]
    async fn test_try_delete_instance() {
        let mut mock_kube_interface = MockKubeInterface::new();
        mock_kube_interface
            .expect_delete_instance()
            .times(1)
            .returning(move |_| {
                let error_response = kube::error::ErrorResponse {
                    status: "random".to_string(),
                    message: "blah".to_string(),
                    reason: "NotFound".to_string(),
                    code: 404,
                };
                Err(error_response.into())
            });
        try_delete_instance(&mock_kube_interface, "instance_name")
            .await
            .unwrap();
    }

    // Test that succeeds on second try
    #[tokio::test]
    async fn test_try_delete_instance_sequence() {
        let mut seq = mockall::Sequence::new();
        let mut mock_kube_interface = MockKubeInterface::new();
        mock_kube_interface
            .expect_delete_instance()
            .times(1)
            .returning(move |_| {
                let error_response = kube::error::ErrorResponse {
                    status: "random".to_string(),
                    message: "blah".to_string(),
                    reason: "SomeError".to_string(),
                    code: 401,
                };
                Err(error_response.into())
            })
            .in_sequence(&mut seq);
        mock_kube_interface
            .expect_delete_instance()
            .times(1)
            .returning(move |_| Ok(()))
            .in_sequence(&mut seq);
        try_delete_instance(&mock_kube_interface, "instance_name")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_ownership_from_config() {
        let name = "asdf";
        let uid = "zxcv";
        let ownership = OwnershipInfo::new(
            OwnershipType::Configuration,
            name.to_string(),
            uid.to_string(),
        );
        assert_eq!(
            format!("{}/{}", API_NAMESPACE, API_VERSION),
            ownership.get_api_version()
        );
        assert_eq!("Configuration", &ownership.get_kind());
        assert!(ownership.get_controller().unwrap());
        assert!(ownership.get_block_owner_deletion().unwrap());
        assert_eq!(name, &ownership.get_name());
        assert_eq!(uid, &ownership.get_uid());
    }
    #[tokio::test]
    async fn test_ownership_from_instance() {
        let name = "asdf";
        let uid = "zxcv";
        let ownership =
            OwnershipInfo::new(OwnershipType::Instance, name.to_string(), uid.to_string());
        assert_eq!(
            format!("{}/{}", API_NAMESPACE, API_VERSION),
            ownership.get_api_version()
        );
        assert_eq!("Instance", &ownership.get_kind());
        assert!(ownership.get_controller().unwrap());
        assert!(ownership.get_block_owner_deletion().unwrap());
        assert_eq!(name, &ownership.get_name());
        assert_eq!(uid, &ownership.get_uid());
    }
    #[tokio::test]
    async fn test_ownership_from_pod() {
        let name = "asdf";
        let uid = "zxcv";
        let ownership = OwnershipInfo::new(OwnershipType::Pod, name.to_string(), uid.to_string());
        assert_eq!("core/v1", ownership.get_api_version());
        assert_eq!("Pod", &ownership.get_kind());
        assert!(ownership.get_controller().unwrap());
        assert!(ownership.get_block_owner_deletion().unwrap());
        assert_eq!(name, &ownership.get_name());
        assert_eq!(uid, &ownership.get_uid());
    }
    #[tokio::test]
    async fn test_ownership_from_service() {
        let name = "asdf";
        let uid = "zxcv";
        let ownership =
            OwnershipInfo::new(OwnershipType::Service, name.to_string(), uid.to_string());
        assert_eq!("core/v1", ownership.get_api_version());
        assert_eq!("Service", &ownership.get_kind());
        assert!(ownership.get_controller().unwrap());
        assert!(ownership.get_block_owner_deletion().unwrap());
        assert_eq!(name, &ownership.get_name());
        assert_eq!(uid, &ownership.get_uid());
    }
}
