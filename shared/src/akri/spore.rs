use std::fmt::Display;

use kube::{core::DynamicObject, CustomResource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn arbitrary_json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    serde_json::from_value(serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "x-kubernetes-preserve-unknown-fields": true,
            "x-kubernetes-embedded-resource": true,
        }
    }))
    .unwrap()
}

/// # Spore: Resource Deployment Configuration
/// This object describe what kubernetes objects to deploy for a given device discovery configuration.
/// It is capable of deploying one-off objects (one set of object that get deployed if there is
/// at least 1 discovered device), or per device instances objects.
/// All deployed objects will be deployed to the Spore's namespace.
/// Objects definition can make use of Liquid templating, for more information about the available
/// variables see **TODO: Insert link to documentation**
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, Default)]
#[kube(
    group = "akri.sh",
    version = "v1",
    kind = "Spore",
    plural = "spores",
    root = "Spore",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct SporeSpec {
    /// List of full kubernetes objects to deploy for every discovered devices.
    /// If a device disappears, the associated objects will be deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "arbitrary_json_schema")]
    pub device_spore: Option<Vec<DynamicObject>>,
    /// Identifies the linked Discovery Configuration, currently it is only possible
    /// to link to a single configuration by name.
    pub discovery_selector: SporeDiscoverySelector,
    /// List of full kubernetes objects to deploy only once when at least a device
    /// is discovered, these will get deleted if there are no discovered device left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "arbitrary_json_schema")]
    pub once_spore: Option<Vec<DynamicObject>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, Default)]
pub struct SporeDiscoverySelector {
    pub name: String,
}

impl Display for Spore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Spore<{}::{} ({})>",
            self.namespace().unwrap_or_default(),
            self.name_any(),
            self.uid().unwrap_or_default()
        )
    }
}
