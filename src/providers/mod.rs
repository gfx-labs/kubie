pub mod config;
pub mod kubeconfig;
#[cfg(feature = "remote")]
pub mod remote;

use serde::{Deserialize, Serialize};

use crate::kubeconfig::Installed;

/// A named provider: (config key, provider implementation).
pub type NamedProvider = (String, Box<dyn Provider>);

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Metadata key-value pairs for the preview pane.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreviewField {
    pub label: String,
    pub value: String,
}

/// A cluster/context discovered by a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterInfo {
    /// Unique cluster ID within the provider (e.g. DO cluster UUID, rancher cluster ID, or context name for kubeconfig).
    pub id: String,
    /// Human-readable cluster name.
    pub name: String,
    /// The kubernetes context name as it would appear in a kubeconfig.
    pub context_name: String,
    /// Which provider type this cluster came from (e.g. "kubeconfig", "digitalocean", "rancher").
    pub provider: String,
    /// The config key / account label for this provider instance.
    pub account: String,
    /// Structured metadata fields for the preview pane.
    #[serde(default)]
    pub metadata: Vec<PreviewField>,
}

// ---------------------------------------------------------------------------
// Provider trait -- implemented by all sources
// ---------------------------------------------------------------------------

/// Trait that all context sources must implement.
///
/// Each method is synchronous and may block (file I/O, HTTP calls, etc.).
/// The caller handles parallelism by running providers on separate threads.
pub trait Provider: Send + Sync {
    /// Provider type name (e.g. "kubeconfig", "digitalocean", "rancher").
    fn provider_type(&self) -> &'static str;

    /// Discover all clusters/contexts this provider knows about.
    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>>;

    /// Obtain the kubeconfig YAML for a specific cluster.
    /// For local kubeconfigs this reads from disk; for remote providers it downloads.
    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String>;
}

// ---------------------------------------------------------------------------
// EditableProvider trait -- only for file-backed sources
// ---------------------------------------------------------------------------

/// Extended trait for providers whose contexts can be edited, deleted, and linted.
/// Only the kubeconfig provider implements this.
pub trait EditableProvider: Provider {
    /// Load the full installed contexts/clusters/users from this provider's files.
    fn get_installed(&self) -> anyhow::Result<Installed>;
}
