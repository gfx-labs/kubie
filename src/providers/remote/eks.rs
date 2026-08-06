//! AWS EKS provider.
//!
//! **Status: UNTESTED** -- implemented from API documentation, not validated against a real cluster.
//!
//! Discovers EKS clusters using the AWS CLI and generates kubeconfigs with the
//! `aws eks get-token` exec-based auth plugin for automatic token refresh.
//!
//! ```yaml
//! my-eks:
//!   type: eks
//!   config:
//!     region: us-east-1
//!     # Optional: AWS CLI profile to use. Defaults to the default profile.
//!     # profile: my-profile
//! ```

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::providers::{ClusterInfo, PreviewField, Provider};

/// EKS provider configuration.
#[derive(Debug, Deserialize)]
pub struct EksConfig {
    /// AWS region (e.g. "us-east-1").
    pub region: String,
    /// Optional AWS CLI profile name.
    #[serde(default)]
    pub profile: Option<String>,
}

/// EKS provider: discovers clusters via the AWS CLI.
pub struct Eks {
    config: EksConfig,
}

impl Eks {
    pub fn new(config: EksConfig) -> Self {
        Self { config }
    }

    fn aws_cmd(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new("aws");
        cmd.args(["--region", &self.config.region]);
        cmd.arg("--output").arg("json");
        if let Some(ref profile) = self.config.profile {
            cmd.args(["--profile", profile]);
        }
        cmd
    }
}

// -- AWS CLI response structures --

#[derive(Debug, Deserialize)]
struct ListClustersResponse {
    clusters: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DescribeClusterResponse {
    cluster: EksCluster,
}

#[derive(Debug, Deserialize)]
struct EksCluster {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arn: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default, rename = "certificateAuthority")]
    certificate_authority: Option<EksCertAuthority>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    status: String,
    #[serde(default, rename = "platformVersion")]
    platform_version: String,
    #[serde(default, rename = "createdAt")]
    #[allow(dead_code)]
    created_at: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct EksCertAuthority {
    #[serde(default)]
    data: String,
}

fn convert_cluster(account: &str, region: &str, c: &EksCluster) -> ClusterInfo {
    let context_name = format!("arn:aws:eks:{}:cluster:{}", region, c.name);

    let mut metadata = vec![PreviewField {
        label: "Region".into(),
        value: region.to_string(),
    }];

    if !c.version.is_empty() {
        metadata.push(PreviewField {
            label: "Version".into(),
            value: c.version.clone(),
        });
    }
    if !c.status.is_empty() {
        metadata.push(PreviewField {
            label: "Status".into(),
            value: c.status.clone(),
        });
    }
    if !c.platform_version.is_empty() {
        metadata.push(PreviewField {
            label: "Platform".into(),
            value: c.platform_version.clone(),
        });
    }
    if !c.endpoint.is_empty() {
        metadata.push(PreviewField {
            label: "Endpoint".into(),
            value: c.endpoint.clone(),
        });
    }

    ClusterInfo {
        id: c.arn.clone(),
        name: c.name.clone(),
        context_name,
        provider: "eks".into(),
        account: account.to_string(),
        metadata,
    }
}

/// Build a kubeconfig using the `aws eks get-token` exec plugin.
fn build_kubeconfig(cluster: &EksCluster, region: &str, profile: Option<&str>) -> String {
    let ca_data = cluster
        .certificate_authority
        .as_ref()
        .map(|ca| ca.data.as_str())
        .unwrap_or("");

    let context_name = format!("arn:aws:eks:{}:cluster:{}", region, cluster.name);

    let mut exec_args = format!(
        r#"      command: aws
      args:
        - eks
        - get-token
        - --cluster-name
        - "{}"
        - --region
        - "{}""#,
        cluster.name, region
    );

    if let Some(p) = profile {
        exec_args.push_str(&format!(
            r#"
        - --profile
        - "{}""#,
            p
        ));
    }

    format!(
        r#"apiVersion: v1
kind: Config
current-context: {context_name}
clusters:
- name: {context_name}
  cluster:
    server: "{endpoint}"
    certificate-authority-data: "{ca_data}"
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
{exec_args}
      env:
        - name: AWS_STS_REGIONAL_ENDPOINTS
          value: regional
"#,
        endpoint = cluster.endpoint,
    )
}

impl Provider for Eks {
    fn provider_type(&self) -> &'static str {
        "eks"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let output = self
            .aws_cmd()
            .args(["eks", "list-clusters"])
            .output()
            .context("Failed to run 'aws eks list-clusters'. Is the AWS CLI installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("aws eks list-clusters failed: {}", stderr.trim());
        }

        let response: ListClustersResponse =
            serde_json::from_slice(&output.stdout).context("Failed to parse aws eks list-clusters output")?;

        let mut clusters = Vec::new();
        for name in &response.clusters {
            let cluster = self
                .describe_cluster(name)
                .with_context(|| format!("Failed to describe EKS cluster {name}"))?;
            clusters.push(convert_cluster(account, &self.config.region, &cluster));
        }

        Ok(clusters)
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        let eks_cluster = self.describe_cluster(&cluster.name)?;

        if eks_cluster.endpoint.is_empty() {
            bail!("EKS cluster {} has no endpoint", cluster.name);
        }

        Ok(build_kubeconfig(
            &eks_cluster,
            &self.config.region,
            self.config.profile.as_deref(),
        ))
    }
}

impl Eks {
    fn describe_cluster(&self, name: &str) -> anyhow::Result<EksCluster> {
        let output = self
            .aws_cmd()
            .args(["eks", "describe-cluster", "--name", name])
            .output()
            .with_context(|| format!("Failed to run 'aws eks describe-cluster --name {name}'"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("aws eks describe-cluster failed for {name}: {}", stderr.trim());
        }

        let response: DescribeClusterResponse = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("Failed to parse describe-cluster output for {name}"))?;

        Ok(response.cluster)
    }
}
