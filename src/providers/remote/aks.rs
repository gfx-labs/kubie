//! Azure AKS provider.
//!
//! **Status: UNTESTED** -- implemented from API documentation, not validated against a real cluster.
//!
//! Discovers AKS clusters using the Azure CLI and retrieves kubeconfigs via
//! `az aks get-credentials`. Uses `kubelogin` for exec-based auth if available.
//!
//! ```yaml
//! my-aks:
//!   type: aks
//!   config:
//!     subscription: 00000000-0000-0000-0000-000000000000
//!     # Optional: only discover clusters in this resource group.
//!     # resource_group: my-rg
//! ```

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::providers::{ClusterInfo, PreviewField, Provider};

/// AKS provider configuration.
#[derive(Debug, Deserialize)]
pub struct AksConfig {
    /// Azure subscription ID.
    pub subscription: String,
    /// Optional: limit discovery to a specific resource group.
    #[serde(default)]
    pub resource_group: Option<String>,
}

/// AKS provider: discovers clusters via the Azure CLI.
pub struct Aks {
    config: AksConfig,
}

impl Aks {
    pub fn new(config: AksConfig) -> Self {
        Self { config }
    }
}

// -- Azure CLI response structures --

#[derive(Debug, Deserialize)]
struct AksCluster {
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    location: String,
    #[serde(default, rename = "resourceGroup")]
    resource_group: String,
    #[serde(default, rename = "kubernetesVersion")]
    kubernetes_version: String,
    #[serde(default, rename = "provisioningState")]
    provisioning_state: String,
    #[serde(default, rename = "powerState")]
    power_state: Option<AksPowerState>,
    #[serde(default, rename = "agentPoolProfiles")]
    agent_pool_profiles: Vec<AksAgentPool>,
    #[serde(default)]
    fqdn: String,
}

#[derive(Debug, Deserialize)]
struct AksPowerState {
    #[serde(default)]
    code: String,
}

#[derive(Debug, Deserialize)]
struct AksAgentPool {
    #[serde(default)]
    name: String,
    #[serde(default)]
    count: u32,
    #[serde(default, rename = "vmSize")]
    vm_size: String,
    #[serde(default, rename = "enableAutoScaling")]
    enable_auto_scaling: bool,
    #[serde(default, rename = "minCount")]
    min_count: Option<u32>,
    #[serde(default, rename = "maxCount")]
    max_count: Option<u32>,
}

fn convert_cluster(account: &str, c: &AksCluster) -> ClusterInfo {
    let context_name = c.name.clone();

    let mut metadata = vec![
        PreviewField {
            label: "Location".into(),
            value: c.location.clone(),
        },
        PreviewField {
            label: "Resource Group".into(),
            value: c.resource_group.clone(),
        },
    ];

    if !c.kubernetes_version.is_empty() {
        metadata.push(PreviewField {
            label: "Version".into(),
            value: c.kubernetes_version.clone(),
        });
    }
    if !c.provisioning_state.is_empty() {
        metadata.push(PreviewField {
            label: "Status".into(),
            value: c.provisioning_state.clone(),
        });
    }
    if let Some(ps) = &c.power_state {
        if !ps.code.is_empty() {
            metadata.push(PreviewField {
                label: "Power".into(),
                value: ps.code.clone(),
            });
        }
    }

    for pool in &c.agent_pool_profiles {
        let scaling = if pool.enable_auto_scaling {
            match (pool.min_count, pool.max_count) {
                (Some(min), Some(max)) => format!("{min}-{max} nodes (autoscale)"),
                _ => format!("{} nodes", pool.count),
            }
        } else {
            format!("{} nodes", pool.count)
        };
        metadata.push(PreviewField {
            label: "Pool".into(),
            value: format!("{} ({}, {})", pool.name, pool.vm_size, scaling),
        });
    }

    if !c.fqdn.is_empty() {
        metadata.push(PreviewField {
            label: "FQDN".into(),
            value: c.fqdn.clone(),
        });
    }

    ClusterInfo {
        id: c.id.clone(),
        name: c.name.clone(),
        context_name,
        provider: "aks".into(),
        account: account.to_string(),
        metadata,
    }
}

impl Provider for Aks {
    fn provider_type(&self) -> &'static str {
        "aks"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let mut cmd = std::process::Command::new("az");
        cmd.args(["aks", "list"]);
        cmd.args(["--subscription", &self.config.subscription]);
        if let Some(ref rg) = self.config.resource_group {
            cmd.args(["--resource-group", rg]);
        }
        cmd.args(["--output", "json"]);

        let output = cmd
            .output()
            .context("Failed to run 'az aks list'. Is the Azure CLI installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("az aks list failed: {}", stderr.trim());
        }

        let clusters: Vec<AksCluster> =
            serde_json::from_slice(&output.stdout).context("Failed to parse az aks list output")?;

        Ok(clusters.iter().map(|c| convert_cluster(account, c)).collect())
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        // Extract resource group from the cluster metadata.
        let resource_group = cluster
            .metadata
            .iter()
            .find(|f| f.label == "Resource Group")
            .map(|f| f.value.as_str())
            .unwrap_or("");

        if resource_group.is_empty() {
            bail!("Cannot determine resource group for AKS cluster {}", cluster.name);
        }

        // Use `az aks get-credentials` to a temp file, then read it.
        let tmp = tempfile::Builder::new()
            .prefix("kubie-aks-")
            .suffix(".yaml")
            .tempfile()?;
        let tmp_path = tmp.path().to_string_lossy().to_string();

        let output = std::process::Command::new("az")
            .args(["aks", "get-credentials"])
            .args(["--subscription", &self.config.subscription])
            .args(["--resource-group", resource_group])
            .args(["--name", &cluster.name])
            .args(["--file", &tmp_path])
            .args(["--overwrite-existing"])
            .output()
            .with_context(|| format!("Failed to run 'az aks get-credentials' for {}", cluster.name))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("az aks get-credentials failed for {}: {}", cluster.name, stderr.trim());
        }

        // Try to convert to kubelogin format for non-interactive auth.
        let _ = std::process::Command::new("kubelogin")
            .args(["convert-kubeconfig", "-l", "azurecli", "--kubeconfig", &tmp_path])
            .output();

        let content = std::fs::read_to_string(tmp.path())
            .with_context(|| format!("Failed to read kubeconfig for {}", cluster.name))?;

        Ok(content)
    }
}
