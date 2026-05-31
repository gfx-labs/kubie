//! Linode LKE (Linode Kubernetes Engine) provider.
//!
//! **Status: UNTESTED** -- implemented from API documentation, not validated against a real cluster.
//!
//! Discovers LKE clusters via the Linode API and downloads kubeconfigs on demand.
//! Uses a personal access token for authentication.
//!
//! ```yaml
//! my-linode:
//!   type: linode
//!   config:
//!     token: $(linode-cli --text --no-headers profile view | awk '{print $1}')
//!     # Or use an explicit token:
//!     # token: ${LINODE_TOKEN}
//! ```

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::providers::config::Secret;
use crate::providers::{ClusterInfo, PreviewField, Provider};

const LINODE_API_BASE: &str = "https://api.linode.com/v4";

/// Linode LKE provider configuration.
#[derive(Debug, Deserialize)]
pub struct LinodeConfig {
    /// Linode personal access token. Supports Secret expansion.
    pub token: Secret,
}

/// Linode LKE provider.
pub struct Linode {
    config: LinodeConfig,
}

impl Linode {
    pub fn new(config: LinodeConfig) -> Self {
        Self { config }
    }

    fn token(&self) -> anyhow::Result<&str> {
        let t = self.config.token.value();
        if t.is_empty() {
            bail!("Linode token is empty. Set `token` in provider config.");
        }
        Ok(t)
    }
}

// -- Linode API response structures --

#[derive(Debug, Deserialize)]
struct ListClustersResponse {
    data: Vec<LkeCluster>,
}

#[derive(Debug, Deserialize)]
struct LkeCluster {
    id: u64,
    label: String,
    region: String,
    #[serde(default)]
    k8s_version: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    control_plane: Option<LkeControlPlane>,
}

#[derive(Debug, Deserialize)]
struct LkeControlPlane {
    #[serde(default)]
    high_availability: bool,
}

#[derive(Debug, Deserialize)]
struct KubeconfigResponse {
    kubeconfig: String,
}

fn convert_cluster(account: &str, c: &LkeCluster) -> ClusterInfo {
    let context_name = format!("lke-{}-{}", c.region, c.label);

    let mut metadata = vec![PreviewField {
        label: "Region".into(),
        value: c.region.clone(),
    }];

    if !c.k8s_version.is_empty() {
        metadata.push(PreviewField {
            label: "Version".into(),
            value: c.k8s_version.clone(),
        });
    }
    if !c.status.is_empty() {
        metadata.push(PreviewField {
            label: "Status".into(),
            value: c.status.clone(),
        });
    }
    if let Some(cp) = &c.control_plane {
        if cp.high_availability {
            metadata.push(PreviewField {
                label: "HA".into(),
                value: "yes".into(),
            });
        }
    }
    if !c.created.is_empty() {
        metadata.push(PreviewField {
            label: "Created".into(),
            value: c.created.clone(),
        });
    }

    ClusterInfo {
        id: c.id.to_string(),
        name: c.label.clone(),
        context_name,
        provider: "linode".into(),
        account: account.to_string(),
        metadata,
    }
}

impl Provider for Linode {
    fn provider_type(&self) -> &'static str {
        "linode"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let token = self.token()?;
        let url = format!("{LINODE_API_BASE}/lke/clusters");

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .context("Failed to call Linode LKE clusters API")?
            .body_mut()
            .read_to_string()
            .context("Failed to read Linode LKE clusters response")?;

        let response: ListClustersResponse =
            serde_json::from_str(&body).context("Failed to parse Linode LKE clusters response")?;

        Ok(response.data.iter().map(|c| convert_cluster(account, c)).collect())
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        let token = self.token()?;
        let url = format!("{LINODE_API_BASE}/lke/clusters/{}/kubeconfig", cluster.id);

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .with_context(|| format!("Failed to get kubeconfig for LKE cluster {}", cluster.name))?
            .body_mut()
            .read_to_string()
            .with_context(|| format!("Failed to read kubeconfig for LKE cluster {}", cluster.name))?;

        let response: KubeconfigResponse = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse kubeconfig response for {}", cluster.name))?;

        // Linode returns the kubeconfig base64-encoded.
        let decoded = base64_decode(&response.kubeconfig)
            .with_context(|| format!("Failed to decode kubeconfig for {}", cluster.name))?;

        Ok(decoded)
    }
}

fn base64_decode(input: &str) -> anyhow::Result<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(input.trim())?;
    String::from_utf8(bytes).context("Decoded kubeconfig is not valid UTF-8")
}
