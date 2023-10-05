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

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, Default)]
#[kube(
    group = "akri.sh",
    version = "v0",
    kind = "Spore",
    plural = "spores",
    struct = "Spore",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct SporeSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "arbitrary_json_schema")]
    pub device_spore: Option<Vec<DynamicObject>>,
    pub discovery_selector: SporeDiscoverySelector,
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
