use anyhow::{bail, Context};
use serde::Deserialize;

use crate::providers::{ClusterInfo, PreviewField, Provider};
use crate::providers::config::Secret;
use super::resources;

/// Rancher provider configuration.
///
/// ```yaml
/// my-rancher:
///   type: rancher
///   config:
///     url: https://rancher.example.com
///     token: $(vault kv get -field=token secret/rancher)
/// ```
#[derive(Debug, Deserialize)]
pub struct RancherConfig {
    /// Base URL of the Rancher server (e.g. `https://rancher.example.com`).
    /// Plain string -- not expanded.
    pub url: String,

    /// API bearer token (format: `token-xxxxx:secret`).
    /// Supports env var and command expansion via the Secret type.
    pub token: Secret,
}

/// Rancher provider: discovers clusters via the Rancher v3 API.
pub struct Rancher {
    config: RancherConfig,
}

impl Rancher {
    pub fn new(config: RancherConfig) -> Self {
        Self { config }
    }

    fn base_url(&self) -> &str {
        self.config.url.trim_end_matches('/')
    }

    fn token(&self) -> &str {
        self.config.token.value()
    }
}

// -- Rancher v3 API response structures --

#[derive(Debug, Deserialize)]
struct ClustersResponse {
    data: Vec<RancherCluster>,
}

#[derive(Debug, Deserialize)]
struct RancherCluster {
    id: String,
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    driver: String,
    #[serde(default, rename = "nodeCount")]
    node_count: Option<u32>,
    #[serde(default)]
    version: Option<RancherVersion>,
    #[serde(default)]
    created: String,
    #[serde(default)]
    allocatable: Option<RancherResources>,
}

#[derive(Debug, Deserialize)]
struct RancherVersion {
    #[serde(default, rename = "gitVersion")]
    git_version: String,
}

#[derive(Debug, Deserialize)]
struct RancherResources {
    #[serde(default)]
    cpu: String,
    #[serde(default)]
    memory: String,
    #[serde(default)]
    #[allow(dead_code)]
    pods: String,
}

#[derive(Debug, Deserialize)]
struct GenerateKubeconfigResponse {
    config: String,
}

fn convert_cluster(account: &str, c: RancherCluster) -> ClusterInfo {
    let context_name = c.name.clone();

    let mut metadata = Vec::new();

    if !c.state.is_empty() {
        metadata.push(PreviewField {
            label: "Status".into(),
            value: c.state,
        });
    }
    if !c.provider.is_empty() {
        metadata.push(PreviewField {
            label: "Provider".into(),
            value: c.provider,
        });
    }
    if !c.driver.is_empty() {
        metadata.push(PreviewField {
            label: "Driver".into(),
            value: c.driver,
        });
    }
    if let Some(v) = &c.version {
        if !v.git_version.is_empty() {
            metadata.push(PreviewField {
                label: "Version".into(),
                value: v.git_version.clone(),
            });
        }
    }
    if let Some(count) = c.node_count {
        metadata.push(PreviewField {
            label: "Nodes".into(),
            value: count.to_string(),
        });
    }
    if let Some(res) = &c.allocatable {
        if !res.cpu.is_empty() {
            metadata.push(PreviewField {
                label: "CPU".into(),
                value: resources::humanize_cpu(&res.cpu),
            });
        }
        if !res.memory.is_empty() {
            metadata.push(PreviewField {
                label: "Memory".into(),
                value: resources::humanize_memory(&res.memory),
            });
        }
    }
    if !c.created.is_empty() {
        metadata.push(PreviewField {
            label: "Created".into(),
            value: c.created,
        });
    }

    ClusterInfo {
        id: c.id,
        name: c.name,
        context_name,
        provider: "rancher".into(),
        account: account.to_string(),
        metadata,
    }
}

impl Provider for Rancher {
    fn provider_type(&self) -> &'static str {
        "rancher"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let url = format!("{}/v3/clusters", self.base_url());

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {}", self.token()))
            .call()
            .context("Failed to call Rancher clusters API")?
            .body_mut()
            .read_to_string()
            .context("Failed to read Rancher clusters response")?;

        let response: ClustersResponse =
            serde_json::from_str(&body).context("Failed to parse Rancher clusters response")?;

        let clusters = response
            .data
            .into_iter()
            .map(|c| convert_cluster(account, c))
            .collect();

        Ok(clusters)
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        let url = format!(
            "{}/v3/clusters/{}?action=generateKubeconfig",
            self.base_url(),
            cluster.id
        );

        let body: String = ureq::post(&url)
            .header("Authorization", &format!("Bearer {}", self.token()))
            .header("Content-Type", "application/json")
            .send_empty()
            .with_context(|| {
                format!("Failed to generate kubeconfig for cluster {}", cluster.name)
            })?
            .body_mut()
            .read_to_string()
            .with_context(|| {
                format!(
                    "Failed to read kubeconfig response for cluster {}",
                    cluster.name
                )
            })?;

        let response: GenerateKubeconfigResponse = serde_json::from_str(&body).with_context(|| {
            format!(
                "Failed to parse kubeconfig response for cluster {}",
                cluster.name
            )
        })?;

        if response.config.is_empty() {
            bail!(
                "Rancher returned empty kubeconfig for cluster {}",
                cluster.name
            );
        }

        Ok(response.config)
    }
}
