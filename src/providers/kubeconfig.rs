//! KubeConfig provider -- reads contexts from local kubeconfig files on disk.
//!
//! This is the built-in provider that replaces the old `configs.include/exclude` system.
//! It implements both `Provider` (for the unified picker) and `EditableProvider` (for
//! edit, delete, lint operations).

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{ClusterInfo, EditableProvider, PreviewField, Provider};
use crate::kubeconfig::{self, Installed};
use crate::settings::expanduser;

/// Configuration for the kubeconfig provider.
#[derive(Debug, Serialize, Deserialize)]
pub struct KubeConfigProviderConfig {
    /// Glob patterns for kubeconfig files to include.
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    /// Glob patterns for kubeconfig files to exclude.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for KubeConfigProviderConfig {
    fn default() -> Self {
        Self {
            include: default_include(),
            exclude: Vec::new(),
        }
    }
}

fn default_include() -> Vec<String> {
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|| "~".to_string());
    vec![
        format!("{home}/.kube/config"),
        format!("{home}/.kube/*.yml"),
        format!("{home}/.kube/*.yaml"),
        format!("{home}/.kube/configs/*.yml"),
        format!("{home}/.kube/configs/*.yaml"),
        format!("{home}/.kube/kubie/*.yml"),
        format!("{home}/.kube/kubie/*.yaml"),
    ]
}

/// The kubeconfig provider.
pub struct KubeConfigProvider {
    config: KubeConfigProviderConfig,
    /// Extra paths to exclude (e.g. kubie's own config file).
    extra_excludes: Vec<String>,
}

impl KubeConfigProvider {
    pub fn new(config: KubeConfigProviderConfig) -> Self {
        Self {
            config,
            extra_excludes: Vec::new(),
        }
    }

    /// Add an extra path to the exclusion list (used to exclude kubie.yaml itself).
    pub fn exclude(&mut self, path: String) {
        self.extra_excludes.push(path);
    }

    /// Resolve all kubeconfig file paths from include/exclude globs.
    fn resolve_paths(&self) -> Result<HashSet<PathBuf>> {
        let mut paths = HashSet::new();
        for inc in &self.config.include {
            let expanded = expanduser(inc);
            for entry in glob::glob(&expanded)? {
                paths.insert(entry?);
            }
        }

        let all_excludes = self
            .config
            .exclude
            .iter()
            .chain(self.extra_excludes.iter());

        for exc in all_excludes {
            let expanded = expanduser(exc);
            for entry in glob::glob(&expanded)? {
                paths.remove(&entry?);
            }
        }

        Ok(paths)
    }
}

impl Provider for KubeConfigProvider {
    fn provider_type(&self) -> &'static str {
        "kubeconfig"
    }

    fn list_clusters(&self, account: &str) -> Result<Vec<ClusterInfo>> {
        let installed = self.get_installed()?;
        let mut clusters = Vec::new();

        for ctx in &installed.contexts {
            let cluster_name = &ctx.item.context.cluster;
            let server = installed
                .find_cluster_by_name(cluster_name, &ctx.source)
                .and_then(|c| {
                    c.item
                        .cluster
                        .get("server")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();

            let mut metadata = vec![
                PreviewField {
                    label: "Cluster".into(),
                    value: cluster_name.clone(),
                },
            ];
            if !server.is_empty() {
                metadata.push(PreviewField {
                    label: "Server".into(),
                    value: server,
                });
            }
            if let Some(ns) = &ctx.item.context.namespace {
                metadata.push(PreviewField {
                    label: "Namespace".into(),
                    value: ns.clone(),
                });
            }
            metadata.push(PreviewField {
                label: "File".into(),
                value: ctx.source.display().to_string(),
            });

            clusters.push(ClusterInfo {
                id: ctx.item.name.clone(),
                name: ctx.item.name.clone(),
                context_name: ctx.item.name.clone(),
                provider: "kubeconfig".into(),
                account: account.to_string(),
                metadata,
            });
        }

        Ok(clusters)
    }

    fn get_kubeconfig(&self, cluster: &ClusterInfo) -> Result<String> {
        // For kubeconfig provider, just build an isolated kubeconfig for the context.
        let installed = self.get_installed()?;
        let kubeconfig = installed.make_kubeconfig_for_context(&cluster.context_name, Option::<String>::None)?;
        let mut buf = Vec::new();
        serde_yaml::to_writer(&mut buf, &kubeconfig)?;
        Ok(String::from_utf8(buf)?)
    }
}

impl EditableProvider for KubeConfigProvider {
    fn get_installed(&self) -> Result<Installed> {
        let paths = self.resolve_paths()?;
        let installed = kubeconfig::load_kubeconfigs(paths)?;
        Ok(installed)
    }
}
