use std::path::PathBuf;
use std::sync::Mutex;

use colored::Colorize;

use super::cache;
use crate::providers::ClusterInfo;
use crate::providers::NamedProvider;

/// The result of discovering clusters, including failures from individual providers.
pub struct FetchResult {
    pub clusters: Vec<ClusterInfo>,
    pub errors: Vec<ProviderError>,
}

/// A provider failure captured during cluster discovery.
pub struct ProviderError {
    pub source: String,
    pub provider_type: String,
    pub message: String,
}

/// Fetch clusters from all providers in parallel.
pub fn fetch_all_clusters(providers: &[NamedProvider]) -> Vec<ClusterInfo> {
    let result = fetch_all_clusters_with_errors(providers);
    for error in result.errors {
        eprintln!(
            "{}",
            format!(
                "Warning: {} ({}) failed: {}",
                error.source, error.provider_type, error.message
            )
            .yellow()
        );
    }
    result.clusters
}

/// Fetch clusters without writing provider failures to the terminal.
pub fn fetch_all_clusters_with_errors(providers: &[NamedProvider]) -> FetchResult {
    let clusters: Mutex<Vec<ClusterInfo>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<ProviderError>> = Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for (name, provider) in providers {
            let clusters = &clusters;
            let errors = &errors;
            s.spawn(move || match provider.list_clusters(name) {
                Ok(discovered) => {
                    clusters.lock().unwrap().extend(discovered);
                }
                Err(e) => {
                    errors.lock().unwrap().push(ProviderError {
                        source: name.clone(),
                        provider_type: provider.provider_type().to_string(),
                        message: format!("{e:#}"),
                    });
                }
            });
        }
    });

    FetchResult {
        clusters: clusters.into_inner().unwrap(),
        errors: errors.into_inner().unwrap(),
    }
}

/// Full sync: discover all clusters from all providers and save metadata.
pub fn full_sync(providers: &[NamedProvider]) -> anyhow::Result<Vec<ClusterInfo>> {
    eprintln!("{}", "Discovering clusters across all providers...".blue());

    let all_clusters = fetch_all_clusters(providers);

    if !all_clusters.is_empty() {
        eprintln!("{}", format!("Found {} cluster(s).", all_clusters.len()).green());
    }

    cache::save_metadata(&all_clusters)?;
    Ok(all_clusters)
}

/// Download a kubeconfig for a cluster if not cached or older than 24 hours.
pub fn ensure_hydrated(cluster: &ClusterInfo, providers: &[NamedProvider]) -> anyhow::Result<()> {
    let filename = cache::config_filename(cluster);
    let path = cache::configs_dir().join(&filename);

    if path.exists() {
        let fresh = path.metadata().and_then(|m| m.modified()).is_ok_and(|t| {
            t.elapsed()
                .is_ok_and(|age| age < std::time::Duration::from_secs(24 * 60 * 60))
        });
        if fresh {
            return Ok(());
        }
    }

    let (_, provider) = providers
        .iter()
        .find(|(name, p)| p.provider_type() == cluster.provider && *name == cluster.account)
        .or_else(|| providers.iter().find(|(_, p)| p.provider_type() == cluster.provider))
        .ok_or_else(|| anyhow::anyhow!("No provider '{}' found for cluster {}", cluster.provider, cluster.name))?;

    eprintln!(
        "{}",
        format!("Fetching kubeconfig for {}...", cluster.context_name).yellow()
    );

    let content = provider.get_kubeconfig(cluster)?;
    cache::write_config(cluster, &content)?;

    Ok(())
}

/// Hydrate all cached clusters and return paths to their kubeconfig files.
pub fn hydrate_all_configs(clusters: &[ClusterInfo], providers: &[NamedProvider]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for cluster in clusters {
        if let Err(e) = ensure_hydrated(cluster, providers) {
            eprintln!(
                "{}",
                format!("Warning: failed to hydrate {}: {e}", cluster.context_name).yellow()
            );
            continue;
        }
        let filename = cache::config_filename(cluster);
        let path = cache::configs_dir().join(&filename);
        if path.exists() {
            paths.push(path);
        }
    }
    paths
}

/// Delete an ephemeral kubeconfig file after it has been consumed.
pub fn cleanup_config(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use anyhow::bail;

    use super::*;
    use crate::providers::Provider;

    struct FailingProvider;

    impl Provider for FailingProvider {
        fn provider_type(&self) -> &'static str {
            "test"
        }

        fn list_clusters(&self, _account: &str) -> anyhow::Result<Vec<ClusterInfo>> {
            bail!("provider stderr")
        }

        fn get_kubeconfig(&self, _cluster: &ClusterInfo) -> anyhow::Result<String> {
            unreachable!()
        }
    }

    #[test]
    fn captures_provider_errors_without_losing_source_details() {
        let providers: Vec<NamedProvider> = vec![("broken-account".into(), Box::new(FailingProvider))];

        let result = fetch_all_clusters_with_errors(&providers);

        assert!(result.clusters.is_empty());
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].source, "broken-account");
        assert_eq!(result.errors[0].provider_type, "test");
        assert_eq!(result.errors[0].message, "provider stderr");
    }
}
