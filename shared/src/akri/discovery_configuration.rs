use std::collections::HashMap;

use kube::CustomResource;
use schemars::JsonSchema;

/// Selects a key from a ConfigMap or Secret
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug, JsonSchema)]
pub struct DiscoveryPropertyKeySelector {
    /// The key to select.
    pub key: String,

    /// Name of the referent.
    pub name: String,

    /// Namespace of the referent
    #[serde(default = "default_namespace")]
    pub namespace: String,

    /// Specify whether the referent or its key must be defined
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional: Option<bool>,
}

fn default_namespace() -> String {
    String::from("default")
}

/// This defines kinds of discovery property source
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DiscoveryPropertySource {
    /// Source is a key of a ConfigMap.
    ConfigMapKeyRef(DiscoveryPropertyKeySelector),
    /// Source is a key of a Secret.
    SecretKeyRef(DiscoveryPropertyKeySelector),
}

/// DiscoveryProperty represents a property for discovery devices
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryProperty {
    /// Name of the discovery property
    pub name: String,

    /// value of the discovery property
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// Source for the discovery property value. Ignored if value is not empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_from: Option<DiscoveryPropertySource>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[kube(
    group = "akri.sh",
    version = "v1alpha1",
    kind = "DiscoveryConfiguration"
)]
pub struct DiscoveryConfigurationSpec {
    /// The name of the Discovery Handler
    pub discovery_handler_name: String,

    /// A set of extra properties the Discovery Handler may need, these can be pulled from ConfigMaps or Secrets
    pub discovery_properties: Option<Vec<DiscoveryProperty>>,

    /// A string that a Discovery Handler knows how to parse to obtain necessary discovery details
    #[serde(default)]
    pub discovery_details: String,

    /// The number of slots for a device instance
    #[serde(default = "default_capacity")]
    #[schemars(range(min = 1))]
    pub instances_capacity: usize,

    /// A set of extra properties that will get added to the Instance properties and forwarded to workloads
    /// using the device
    #[serde(default)]
    pub extra_instances_properties: HashMap<String, String>,
}

fn default_capacity() -> usize {
    1
}
