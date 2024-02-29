use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimHandle {
    pub cdi_name: String,
    pub instance_name: String,
}
