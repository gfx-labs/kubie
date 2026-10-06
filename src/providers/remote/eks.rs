//! AWS EKS provider.
//!
//! Discovers EKS clusters with the AWS CLI and generates kubeconfigs that use
//! `aws eks get-token` as an exec credential plugin, the same mechanism as
//! `aws eks update-kubeconfig`. Context names are the cluster ARN, matching
//! the AWS CLI convention.
//!
//! ```yaml
//! my-eks:
//!   type: eks
//!   config:
//!     # Regions to scan. Omit to use the AWS CLI default region.
//!     regions: [us-east-1, us-west-2]
//!     # Optional: AWS CLI profile. Defaults to the CLI default credential chain.
//!     # profile: my-profile
//!     # Optional: IAM role assumed by `aws eks get-token` for cluster auth.
//!     # role_arn: arn:aws:iam::123456789012:role/eks-admin
//! ```

use std::process::Command;
use std::sync::Mutex;

use anyhow::{bail, Context};
use serde::Deserialize;
use serde_json::json;

use crate::providers::{ClusterInfo, PreviewField, Provider};

/// EKS provider configuration.
#[derive(Debug, Default, Deserialize)]
pub struct EksConfig {
    /// Single AWS region (kept for compatibility with `regions`).
    #[serde(default)]
    pub region: Option<String>,
    /// AWS regions to scan.
    #[serde(default)]
    pub regions: Vec<String>,
    /// AWS CLI profile name.
    #[serde(default)]
    pub profile: Option<String>,
    /// IAM role ARN passed to `aws eks get-token --role-arn`.
    #[serde(default)]
    pub role_arn: Option<String>,
}

impl EksConfig {
    /// Regions to scan. `None` means "use the AWS CLI default region".
    fn regions(&self) -> Vec<Option<String>> {
        let mut regions: Vec<Option<String>> = self.region.iter().chain(&self.regions).cloned().map(Some).collect();
        regions.dedup();
        if regions.is_empty() {
            regions.push(None);
        }
        regions
    }
}

/// EKS provider: discovers clusters via the AWS CLI.
pub struct Eks {
    config: EksConfig,
}

impl Eks {
    pub fn new(config: EksConfig) -> Self {
        Self { config }
    }

    fn aws_cmd(&self, region: Option<&str>) -> Command {
        let mut cmd = Command::new("aws");
        cmd.args(["--output", "json"]);
        if let Some(region) = region {
            cmd.args(["--region", region]);
        }
        if let Some(profile) = &self.config.profile {
            cmd.args(["--profile", profile]);
        }
        cmd
    }

    fn run_aws(&self, region: Option<&str>, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let output = self.aws_cmd(region).args(args).output().with_context(|| {
            format!(
                "Failed to run 'aws {}'. Is the AWS CLI installed and on PATH?",
                args.join(" ")
            )
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("aws {} failed: {}", args.join(" "), stderr.trim());
        }
        Ok(output.stdout)
    }

    fn list_cluster_names(&self, region: Option<&str>) -> anyhow::Result<Vec<String>> {
        let stdout = self.run_aws(region, &["eks", "list-clusters"])?;
        let response: ListClustersResponse =
            serde_json::from_slice(&stdout).context("Failed to parse aws eks list-clusters output")?;
        Ok(response.clusters)
    }

    fn describe_cluster(&self, region: Option<&str>, name: &str) -> anyhow::Result<EksCluster> {
        let stdout = self.run_aws(region, &["eks", "describe-cluster", "--name", name])?;
        let response: DescribeClusterResponse = serde_json::from_slice(&stdout)
            .with_context(|| format!("Failed to parse aws eks describe-cluster output for {name}"))?;
        Ok(response.cluster)
    }

    /// List and describe all clusters in one region. Describes run in parallel.
    fn list_region(&self, account: &str, region: Option<&str>) -> anyhow::Result<Vec<ClusterInfo>> {
        let names = self.list_cluster_names(region)?;
        let results: Mutex<Vec<(usize, anyhow::Result<EksCluster>)>> = Mutex::new(Vec::new());

        std::thread::scope(|s| {
            for (i, name) in names.iter().enumerate() {
                let results = &results;
                s.spawn(move || {
                    let r = self.describe_cluster(region, name);
                    results.lock().unwrap().push((i, r));
                });
            }
        });

        let mut results = results.into_inner().unwrap();
        results.sort_by_key(|(i, _)| *i);
        results
            .into_iter()
            .map(|(_, r)| r.map(|c| convert_cluster(account, &c)))
            .collect()
    }
}

// -- AWS CLI response structures --

#[derive(Debug, Deserialize)]
struct ListClustersResponse {
    #[serde(default)]
    clusters: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DescribeClusterResponse {
    cluster: EksCluster,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EksCluster {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arn: String,
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    certificate_authority: Option<EksCertAuthority>,
    #[serde(default)]
    version: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    platform_version: String,
    #[serde(default)]
    created_at: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct EksCertAuthority {
    #[serde(default)]
    data: String,
}

/// Parsed form of `arn:aws:eks:<region>:<account-id>:cluster/<name>`.
#[derive(Debug, PartialEq, Eq)]
struct ClusterArn<'a> {
    partition: &'a str,
    region: &'a str,
    account_id: &'a str,
    name: &'a str,
}

fn parse_cluster_arn(arn: &str) -> Option<ClusterArn<'_>> {
    let mut parts = arn.splitn(6, ':');
    if parts.next()? != "arn" {
        return None;
    }
    let partition = parts.next()?;
    if parts.next()? != "eks" {
        return None;
    }
    let region = parts.next()?;
    let account_id = parts.next()?;
    let name = parts.next()?.strip_prefix("cluster/")?;
    if region.is_empty() || name.is_empty() {
        return None;
    }
    Some(ClusterArn {
        partition,
        region,
        account_id,
        name,
    })
}

fn convert_cluster(account: &str, c: &EksCluster) -> ClusterInfo {
    let arn = parse_cluster_arn(&c.arn);
    let mut metadata = Vec::new();
    let mut push = |label: &str, value: &str| {
        if !value.is_empty() {
            metadata.push(PreviewField {
                label: label.into(),
                value: value.to_string(),
            });
        }
    };

    if let Some(arn) = &arn {
        push("Region", arn.region);
        push("Account", arn.account_id);
    }
    push("Version", &c.version);
    push("Status", &c.status);
    push("Platform", &c.platform_version);
    push("Endpoint", &c.endpoint);
    match &c.created_at {
        Some(serde_json::Value::String(s)) => push("Created", s),
        Some(serde_json::Value::Number(n)) => push("Created", &n.to_string()),
        _ => {}
    }

    // Fall back to the cluster name if the ARN is missing so the entry is still usable.
    let context_name = if c.arn.is_empty() {
        c.name.clone()
    } else {
        c.arn.clone()
    };

    ClusterInfo {
        id: c.arn.clone(),
        name: c.name.clone(),
        context_name,
        provider: "eks".into(),
        account: account.to_string(),
        metadata,
    }
}

/// Build a kubeconfig that authenticates with `aws eks get-token`.
fn build_kubeconfig(
    cluster: &EksCluster,
    region: &str,
    profile: Option<&str>,
    role_arn: Option<&str>,
) -> anyhow::Result<String> {
    let ca_data = cluster
        .certificate_authority
        .as_ref()
        .map(|ca| ca.data.as_str())
        .unwrap_or("");
    let context_name = &cluster.arn;

    let mut args = vec![
        "--region".to_string(),
        region.to_string(),
        "eks".to_string(),
        "get-token".to_string(),
        "--cluster-name".to_string(),
        cluster.name.clone(),
        "--output".to_string(),
        "json".to_string(),
    ];
    if let Some(role) = role_arn {
        args.push("--role-arn".into());
        args.push(role.to_string());
    }

    let mut env = vec![json!({"name": "AWS_STS_REGIONAL_ENDPOINTS", "value": "regional"})];
    if let Some(p) = profile {
        env.push(json!({"name": "AWS_PROFILE", "value": p}));
    }

    let config = json!({
        "apiVersion": "v1",
        "kind": "Config",
        "current-context": context_name,
        "clusters": [{
            "name": context_name,
            "cluster": {
                "server": cluster.endpoint,
                "certificate-authority-data": ca_data,
            },
        }],
        "contexts": [{
            "name": context_name,
            "context": {
                "cluster": context_name,
                "user": context_name,
            },
        }],
        "users": [{
            "name": context_name,
            "user": {
                "exec": {
                    "apiVersion": "client.authentication.k8s.io/v1beta1",
                    "command": "aws",
                    "args": args,
                    "env": env,
                    "interactiveMode": "IfAvailable",
                    "installHint": "Install the AWS CLI: https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html",
                },
            },
        }],
    });

    serde_yaml::to_string(&config).context("Failed to serialize EKS kubeconfig")
}

impl Provider for Eks {
    fn provider_type(&self) -> &'static str {
        "eks"
    }

    fn list_clusters(&self, account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
        let regions = self.config.regions();
        let results: Mutex<Vec<(usize, anyhow::Result<Vec<ClusterInfo>>)>> = Mutex::new(Vec::new());

        std::thread::scope(|s| {
            for (i, region) in regions.iter().enumerate() {
                let results = &results;
                s.spawn(move || {
                    let r = self
                        .list_region(account, region.as_deref())
                        .with_context(|| match region {
                            Some(r) => format!("region {r}"),
                            None => "default region".to_string(),
                        });
                    results.lock().unwrap().push((i, r));
                });
            }
        });

        let mut results = results.into_inner().unwrap();
        results.sort_by_key(|(i, _)| *i);

        let mut clusters = Vec::new();
        let mut errors = Vec::new();
        for (_, r) in results {
            match r {
                Ok(c) => clusters.extend(c),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }

        // Report partial failures only when nothing was discovered, so one
        // disabled region does not hide clusters from the others.
        if clusters.is_empty() && !errors.is_empty() {
            bail!(errors.join("; "));
        }
        Ok(clusters)
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> anyhow::Result<String> {
        let arn = parse_cluster_arn(&cluster.id);
        let region = match &arn {
            Some(arn) => Some(arn.region.to_string()),
            None => self.config.regions().into_iter().next().flatten(),
        };

        let eks_cluster = self.describe_cluster(region.as_deref(), &cluster.name)?;
        if eks_cluster.endpoint.is_empty() {
            bail!(
                "EKS cluster {} has no endpoint (status: {})",
                cluster.name,
                eks_cluster.status
            );
        }

        let region = match (region, parse_cluster_arn(&eks_cluster.arn)) {
            (Some(r), _) => r,
            (None, Some(arn)) => arn.region.to_string(),
            (None, None) => bail!("Cannot determine region for EKS cluster {}", cluster.name),
        };

        build_kubeconfig(
            &eks_cluster,
            &region,
            self.config.profile.as_deref(),
            self.config.role_arn.as_deref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cluster_arns() {
        assert_eq!(
            parse_cluster_arn("arn:aws:eks:us-east-1:123456789012:cluster/prod"),
            Some(ClusterArn {
                partition: "aws",
                region: "us-east-1",
                account_id: "123456789012",
                name: "prod",
            })
        );
        assert_eq!(
            parse_cluster_arn("arn:aws-cn:eks:cn-north-1:123456789012:cluster/a").map(|a| a.partition),
            Some("aws-cn")
        );
        assert_eq!(parse_cluster_arn("arn:aws:ecs:us-east-1:1:cluster/x"), None);
        assert_eq!(parse_cluster_arn("prod"), None);
    }

    #[test]
    fn kubeconfig_loads_as_kubeconfig() {
        let cluster = EksCluster {
            name: "prod".into(),
            arn: "arn:aws:eks:us-east-1:123456789012:cluster/prod".into(),
            endpoint: "https://ABC.gr7.us-east-1.eks.amazonaws.com".into(),
            certificate_authority: Some(EksCertAuthority { data: "Q0E=".into() }),
            ..Default::default()
        };
        let yaml = build_kubeconfig(&cluster, "us-east-1", Some("dev: profile"), None).unwrap();
        let kc: crate::kubeconfig::KubeConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(kc.contexts[0].name, cluster.arn);
        assert!(yaml.contains("dev: profile"));
    }
}
