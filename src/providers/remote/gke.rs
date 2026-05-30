use anyhow::{bail, Context};
use serde::Deserialize;

use crate::providers::config::Secret;
use crate::providers::{ClusterInfo, PreviewField, Provider};

const GKE_API_BASE: &str = "https://container.googleapis.com";

/// GKE (Google Kubernetes Engine) provider configuration.
///
/// ```yaml
/// my-gke:
///   type: gke
///   config:
///     project: my-gcp-project
///     token: $(gcloud auth print-access-token)
///     # location: us-central1  # optional, defaults to "-" (all locations)
/// ```
#[derive(Debug, Deserialize)]
pub struct GkeConfig {
    /// GCP project ID.
    pub project: String,

    /// OAuth2 access token. Supports command expansion via Secret.
    /// Example: `$(gcloud auth print-access-token)`
    pub token: Secret,

    /// Location filter. Use a region (e.g. "us-central1"), zone (e.g.
    /// "us-central1-a"), or "-" for all locations. Defaults to "-".
    #[serde(default = "default_location")]
    pub location: String,
}

fn default_location() -> String {
    "-".to_string()
}

/// GKE provider: discovers GKE clusters via the Container API.
pub struct Gke {
    config: GkeConfig,
}

impl Gke {
    pub fn new(config: GkeConfig) -> Self {
        Self { config }
    }

    fn token(&self) -> anyhow::Result<&str> {
        let t = self.config.token.value();
        if t.is_empty() {
            bail!("GKE token is empty. Set `token` in provider config (e.g. token: $(gcloud auth print-access-token))");
        }
        Ok(t)
    }
}

// -- GKE API response structures --

#[derive(Debug, Deserialize)]
struct ListClustersResponse {
    #[serde(default)]
    clusters: Vec<GkeCluster>,
}

#[derive(Debug, Deserialize)]
struct GkeCluster {
    #[serde(default)]
    name: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default, rename = "currentMasterVersion")]
    current_master_version: String,
    #[serde(default)]
    status: String,
    #[serde(default, rename = "currentNodeCount")]
    current_node_count: Option<u32>,
    #[serde(default, rename = "createTime")]
    create_time: String,
    #[serde(default, rename = "autopilot")]
    autopilot: Option<GkeAutopilot>,
    #[serde(default, rename = "nodePools")]
    node_pools: Vec<GkeNodePool>,
    #[serde(default, rename = "masterAuth")]
    master_auth: Option<GkeMasterAuth>,
    #[serde(default, rename = "selfLink")]
    self_link: String,
}

#[derive(Debug, Deserialize)]
struct GkeAutopilot {
    #[serde(default)]
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct GkeNodePool {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "initialNodeCount")]
    initial_node_count: Option<u32>,
    #[serde(default)]
    config: Option<GkeNodeConfig>,
    #[serde(default)]
    autoscaling: Option<GkeNodePoolAutoscaling>,
    #[serde(default)]
    #[allow(dead_code)]
    status: String,
}

#[derive(Debug, Deserialize)]
struct GkeNodeConfig {
    #[serde(default, rename = "machineType")]
    machine_type: String,
    #[serde(default, rename = "diskSizeGb")]
    #[allow(dead_code)]
    disk_size_gb: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GkeNodePoolAutoscaling {
    #[serde(default)]
    enabled: bool,
    #[serde(default, rename = "minNodeCount")]
    min_node_count: Option<u32>,
    #[serde(default, rename = "maxNodeCount")]
    max_node_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct GkeMasterAuth {
    #[serde(default, rename = "clusterCaCertificate")]
    cluster_ca_certificate: String,
}

/// Build the context name for a GKE cluster.
/// Follows the gcloud convention: gke_{project}_{location}_{name}
fn gke_context_name(project: &str, location: &str, name: &str) -> String {
    format!("gke_{project}_{location}_{name}")
}

fn convert_cluster(account: &str, project: &str, c: &GkeCluster) -> ClusterInfo {
    let context_name = gke_context_name(project, &c.location, &c.name);

    let mut metadata = vec![PreviewField {
        label: "Location".into(),
        value: c.location.clone(),
    }];

    if !c.current_master_version.is_empty() {
        metadata.push(PreviewField {
            label: "Version".into(),
            value: c.current_master_version.clone(),
        });
    }

    if !c.status.is_empty() {
        metadata.push(PreviewField {
            label: "Status".into(),
            value: c.status.clone(),
        });
    }

    if let Some(ap) = &c.autopilot {
        if ap.enabled {
            metadata.push(PreviewField {
                label: "Mode".into(),
                value: "Autopilot".into(),
            });
        }
    }

    if let Some(count) = c.current_node_count {
        metadata.push(PreviewField {
            label: "Nodes".into(),
            value: count.to_string(),
        });
    }

    for pool in &c.node_pools {
        let machine = pool
            .config
            .as_ref()
            .map(|c| c.machine_type.as_str())
            .unwrap_or("unknown");

        let scaling = if let Some(auto) = &pool.autoscaling {
            if auto.enabled {
                match (auto.min_node_count, auto.max_node_count) {
                    (Some(min), Some(max)) => format!("{min}-{max} nodes (autoscale)"),
                    _ => pool
                        .initial_node_count
                        .map(|n| format!("{n} nodes"))
                        .unwrap_or_default(),
                }
            } else {
                pool.initial_node_count
                    .map(|n| format!("{n} nodes"))
                    .unwrap_or_default()
            }
        } else {
            pool.initial_node_count
                .map(|n| format!("{n} nodes"))
                .unwrap_or_default()
        };

        let value = if scaling.is_empty() {
            format!("{} ({})", pool.name, machine)
        } else {
            format!("{} ({}, {})", pool.name, machine, scaling)
        };

        metadata.push(PreviewField {
            label: "Pool".into(),
            value,
        });
    }

    if !c.endpoint.is_empty() {
        metadata.push(PreviewField {
            label: "Endpoint".into(),
            value: c.endpoint.clone(),
        });
    }

    if !c.create_time.is_empty() {
        metadata.push(PreviewField {
            label: "Created".into(),
            value: c.create_time.clone(),
        });
    }

    ClusterInfo {
        id: c.self_link.clone(),
        name: c.name.clone(),
        context_name,
        provider: "gke".into(),
        account: account.to_string(),
        metadata,
    }
}

/// Build a kubeconfig YAML from cluster info.
///
/// Uses the `gke-gcloud-auth-plugin` exec-based credential provider so that
/// tokens are refreshed automatically by kubectl. This is the same mechanism
/// that `gcloud container clusters get-credentials` uses.
fn build_kubeconfig(cluster: &GkeCluster, project: &str) -> String {
    let ca_cert = cluster
        .master_auth
        .as_ref()
        .map(|a| a.cluster_ca_certificate.as_str())
        .unwrap_or("");

    let context_name = gke_context_name(project, &cluster.location, &cluster.name);

    format!(
        r#"apiVersion: v1
kind: Config
current-context: {context_name}
clusters:
- name: {context_name}
  cluster:
    server: "https://{endpoint}"
    certificate-authority-data: "{ca_cert}"
contexts:
- name: {context_name}
  context:
    cluster: {context_name}
    user: {context_name}
users:
- name: {context_name}
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: gke-gcloud-auth-plugin
      installHint: Install gke-gcloud-auth-plugin for kubectl by following https://cloud.google.com/kubernetes-engine/docs/how-to/cluster-access-for-kubectl#install_plugin
      provideClusterInfo: true
"#,
        endpoint = cluster.endpoint,
    )
}

impl Provider for Gke {
    fn provider_type(&self) -> &'static str {
        "gke"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let token = self.token()?;
        let url = format!(
            "{GKE_API_BASE}/v1/projects/{}/locations/{}/clusters",
            self.config.project, self.config.location
        );

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .context("Failed to call GKE clusters API")?
            .body_mut()
            .read_to_string()
            .context("Failed to read GKE clusters response")?;

        let response: ListClustersResponse =
            serde_json::from_str(&body).context("Failed to parse GKE clusters response")?;

        let clusters = response
            .clusters
            .iter()
            .map(|c| convert_cluster(account, &self.config.project, c))
            .collect();

        Ok(clusters)
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        let token = self.token()?;

        // We need the full cluster details (endpoint + CA cert) to build the kubeconfig.
        // The ClusterInfo.id holds the selfLink, but we can also reconstruct the URL.
        // Parse the context name to extract project/location/name.
        let parts: Vec<&str> = cluster.context_name.splitn(4, '_').collect();
        if parts.len() < 4 || parts[0] != "gke" {
            bail!("Unexpected GKE context name format: {}", cluster.context_name);
        }
        let project = parts[1];
        let location = parts[2];
        let name = parts[3];

        let url = format!("{GKE_API_BASE}/v1/projects/{project}/locations/{location}/clusters/{name}");

        let body: String = ureq::get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .call()
            .with_context(|| format!("Failed to get cluster details for {}", cluster.name))?
            .body_mut()
            .read_to_string()
            .with_context(|| format!("Failed to read cluster details for {}", cluster.name))?;

        let gke_cluster: GkeCluster = serde_json::from_str(&body)
            .with_context(|| format!("Failed to parse cluster details for {}", cluster.name))?;

        if gke_cluster.endpoint.is_empty() {
            bail!("GKE cluster {} has no endpoint", cluster.name);
        }

        Ok(build_kubeconfig(&gke_cluster, project))
    }
}
