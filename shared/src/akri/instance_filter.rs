use kube::CustomResource;
use schemars::JsonSchema;

use super::instance::Instance;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
// group = API_NAMESPACE and version = API_VERSION
#[kube(
    group = "akri.sh",
    version = "v1alpha1",
    kind = "InstanceFilter",
    shortname = "akriif",
    derive = "PartialEq",
    derive = "Default",
    namespaced
)]
pub struct InstanceFilterSpec {
    pub terms: Vec<InstanceFilterTerm>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Deserialize, JsonSchema)]
pub struct InstanceFilterTerm {
    pub match_properties: Vec<InstanceFilterRequirement>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Deserialize, JsonSchema)]
pub struct InstanceFilterRequirement {
    pub key: String,
    pub value: Option<Vec<String>>,
    pub operator: InstanceFilterOperator,
}

#[derive(Debug, Clone, Serialize, PartialEq, Deserialize, JsonSchema)]
pub enum InstanceFilterOperator {
    In,
    NotIn,
    Exists,
    NotExists,
    Gt,
    Lt,
}

impl InstanceFilter {
    pub fn matches(&self, instance: &Instance) -> bool {
        if self.spec.terms.is_empty() {
            return true;
        }
        let mut matches = false;
        for term in &self.spec.terms {
            matches |= term
                .match_properties
                .iter()
                .all(|requirement| requirement.matches(instance))
        }
        matches
    }
}

impl InstanceFilterRequirement {
    fn matches(&self, instance: &Instance) -> bool {
        let props = &instance.spec.broker_properties;
        let val = props.get(&self.key);
        match self.operator {
            InstanceFilterOperator::Exists => val.is_some(),
            InstanceFilterOperator::NotExists => val.is_none(),
            InstanceFilterOperator::In => val.is_some_and(|val| {
                self.value
                    .as_ref()
                    .cloned()
                    .unwrap_or_default()
                    .contains(val)
            }),
            InstanceFilterOperator::NotIn => val.is_some_and(|val| {
                !self
                    .value
                    .as_ref()
                    .cloned()
                    .unwrap_or_default()
                    .contains(val)
            }),
            InstanceFilterOperator::Gt => {
                let Some(val) = val else { return false };
                let Ok(integer_val) = val.parse::<i64>() else {
                    return false;
                };
                let Some(other) = self
                    .value
                    .as_ref()
                    .cloned()
                    .unwrap_or_default()
                    .first()
                    .cloned()
                else {
                    return false;
                };
                let Ok(other_integer) = other.parse() else {
                    return false;
                };
                integer_val > other_integer
            }
            InstanceFilterOperator::Lt => {
                let Some(val) = val else { return false };
                let Ok(integer_val) = val.parse::<i64>() else {
                    return false;
                };
                let Some(other) = self
                    .value
                    .as_ref()
                    .cloned()
                    .unwrap_or_default()
                    .first()
                    .cloned()
                else {
                    return false;
                };
                let Ok(other_integer) = other.parse() else {
                    return false;
                };
                integer_val < other_integer
            }
        }
    }
}
