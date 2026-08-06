use anyhow::{bail, Context};
use serde::Deserialize;

use crate::providers::config::Secret;
use crate::providers::{ClusterInfo, PreviewField, Provider};

const DO_API_BASE: &str = "https://api.digitalocean.com";

/// DigitalOcean provider configuration.
///
/// ```yaml
/// digitalocean:
///   type: digitalocean
///   config:
///     token: $(doctl auth token)
/// ```
#[derive(Debug, Default, Deserialize)]
pub struct DigitalOceanConfig {
    /// API token. Supports env var and command expansion via the Secret type.
    /// Example: `$(doctl auth token)` or `${DIGITALOCEAN_TOKEN}`
    #[serde(default)]
    pub token: Secret,
}

/// DigitalOcean provider: discovers DOKS clusters via the DO v2 API.
pub struct DigitalOcean {
    config: DigitalOceanConfig,
}

impl DigitalOcean {
    pub fn new(config: DigitalOceanConfig) -> Self {
        Self { config }
    }

    fn token(&self) -> anyhow::Result<&str> {
        let t = self.config.token.value()?;
        if t.is_empty() {
            bail!("DigitalOcean token is empty. Set `token` in provider config (e.g. token: $(doctl auth token))");
        }
        Ok(t)
    }
}

// -- DO API response structures --

#[derive(Debug, Deserialize)]
struct ClustersResponse {
    kubernetes_clusters: Vec<DoCluster>,
}

#[derive(Debug, Deserialize)]
struct DoCluster {
    id: String,
    name: String,
    region: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    status: DoClusterStatus,
    #[serde(default)]
    ha: bool,
    #[serde(default)]
    node_pools: Vec<DoNodePool>,
    #[serde(default)]
    created_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct DoClusterStatus {
    #[serde(default)]
    state: String,
}

#[derive(Debug, Deserialize)]
struct DoNodePool {
    name: String,
    size: String,
    #[serde(default)]
    count: u32,
    #[serde(default)]
    auto_scale: bool,
    #[serde(default)]
    min_nodes: Option<u32>,
    #[serde(default)]
    max_nodes: Option<u32>,
}

fn do_context_name(region: &str, name: &str) -> String {
    format!("do-{region}-{name}")
}

/// Parse a DO droplet size slug like "s-2vcpu-4gb" into (vcpus, `gb_ram`).
fn parse_size_slug(slug: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = slug.split('-').collect();
    let mut vcpus = None;
    let mut gb = None;
    for part in &parts {
        if let Some(v) = part.strip_suffix("vcpu") {
            vcpus = v.parse().ok();
        } else if let Some(g) = part.strip_suffix("gb") {
            gb = g.parse().ok();
        }
    }
    vcpus.zip(gb)
}

fn convert_cluster(account: &str, c: DoCluster) -> ClusterInfo {
    let context_name = do_context_name(&c.region, &c.name);

    let mut metadata = vec![PreviewField {
        label: "Region".into(),
        value: c.region.clone(),
    }];

    if !c.version.is_empty() {
        metadata.push(PreviewField {
            label: "Version".into(),
            value: c.version,
        });
    }
    if !c.status.state.is_empty() {
        metadata.push(PreviewField {
            label: "Status".into(),
            value: c.status.state,
        });
    }
    if c.ha {
        metadata.push(PreviewField {
            label: "HA".into(),
            value: "yes".into(),
        });
    }

    let mut total_vcpus: u32 = 0;
    let mut total_gb: u32 = 0;
    let mut total_nodes: u32 = 0;
    let mut pool_lines: Vec<String> = Vec::new();

    for pool in &c.node_pools {
        let node_count = pool.count;
        total_nodes += node_count;

        if let Some((vcpus, gb)) = parse_size_slug(&pool.size) {
            total_vcpus += vcpus * node_count;
            total_gb += gb * node_count;
        }

        let scaling = if pool.auto_scale {
            match (pool.min_nodes, pool.max_nodes) {
                (Some(min), Some(max)) => format!("{min}-{max} nodes, autoscale"),
                _ => format!("{node_count} nodes"),
            }
        } else if node_count > 0 {
            format!("{node_count} nodes")
        } else {
            "unknown".to_string()
        };
        pool_lines.push(format!("{}: {} ({})", pool.name, pool.size, scaling));
    }

    if !pool_lines.is_empty() {
        metadata.push(PreviewField {
            label: "Topology".into(),
            value: pool_lines.join("\n"),
        });
    }

    if total_nodes > 0 {
        metadata.push(PreviewField {
            label: "Nodes".into(),
            value: total_nodes.to_string(),
        });
    }
    if total_vcpus > 0 {
        metadata.push(PreviewField {
            label: "CPU".into(),
            value: format!("{total_vcpus} cores"),
        });
    }
    if total_gb > 0 {
        metadata.push(PreviewField {
            label: "Memory".into(),
            value: format!("{total_gb} GiB"),
        });
    }

    if !c.created_at.is_empty() {
        metadata.push(PreviewField {
            label: "Created".into(),
            value: c.created_at,
        });
    }

    ClusterInfo {
        id: c.id,
        name: c.name,
        context_name,
        provider: "digitalocean".into(),
        account: account.to_string(),
        metadata,
    }
}

impl Provider for DigitalOcean {
    fn provider_type(&self) -> &'static str {
        "digitalocean"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let token = self.token()?;
        let url = format!("{DO_API_BASE}/v2/kubernetes/clusters");

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .context("Failed to call DigitalOcean clusters API")?
            .body_mut()
            .read_to_string()
            .context("Failed to read DigitalOcean clusters response")?;

        let response: ClustersResponse =
            serde_json::from_str(&body).context("Failed to parse DigitalOcean clusters response")?;

        let clusters = response
            .kubernetes_clusters
            .into_iter()
            .map(|c| convert_cluster(account, c))
            .collect();

        Ok(clusters)
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        let token = self.token()?;
        let url = format!("{DO_API_BASE}/v2/kubernetes/clusters/{}/kubeconfig", cluster.id);

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .with_context(|| format!("Failed to download kubeconfig for cluster {}", cluster.name))?
            .body_mut()
            .read_to_string()
            .with_context(|| format!("Failed to read kubeconfig for cluster {}", cluster.name))?;

        if body.is_empty() {
            bail!("DigitalOcean returned empty kubeconfig for cluster {}", cluster.name);
        }

        Ok(body)
    }
}
