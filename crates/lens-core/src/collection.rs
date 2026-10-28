//! Collection scope records observed capability, not system health.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Collected,
    Unavailable,
    Denied,
    Failed,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub state: CapabilityState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
impl Capability {
    pub fn collected() -> Self {
        Self {
            state: CapabilityState::Collected,
            detail: None,
        }
    }
    pub fn from_error(error: &std::io::Error) -> Self {
        Self {
            state: match error.kind() {
                std::io::ErrorKind::NotFound => CapabilityState::Unavailable,
                std::io::ErrorKind::PermissionDenied => CapabilityState::Denied,
                _ => CapabilityState::Failed,
            },
            detail: Some(error.to_string()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionScope {
    /// Origin of the inspected evidence; not necessarily the collector host.
    pub source: String,
    pub capabilities: BTreeMap<String, Capability>,
}
impl CollectionScope {
    pub fn complete(&self) -> bool {
        self.capabilities
            .values()
            .all(|c| !matches!(c.state, CapabilityState::Denied | CapabilityState::Failed))
    }
}
