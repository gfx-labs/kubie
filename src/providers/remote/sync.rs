use std::path::PathBuf;
use std::sync::Mutex;

use colored::Colorize;

use super::cache;
use crate::providers::ClusterInfo;
use crate::providers::NamedProvider;

/// Fetch clusters from all providers in parallel.
pub fn fetch_all_clusters(providers: &[NamedProvider]) -> Vec<ClusterInfo> {
    let results: Mutex<Vec<ClusterInfo>> = Mutex::new(Vec::new());

    std::thread::scope(|s| {
        for (name, provider) in providers {
            let results = &results;
            s.spawn(move || match provider.list_clusters(name) {
                Ok(clusters) => {
                    results.lock().unwrap().extend(clusters);
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        format!("Warning: {name} ({}) failed: {e}", provider.provider_type()).yellow()
                    );
                }
            });
        }
    });

    results.into_inner().unwrap()
}

/// Full sync: discover all clusters from all providers and save metadata.
pub fn full_sync(providers: &[NamedProvider]) -> anyhow::Result<Vec<ClusterInfo>> {
    eprintln!("{}", "Discovering clusters across all providers...".blue());

    let all_clusters = fetch_all_clusters(providers);

    if !all_clusters.is_empty() {
        eprintln!(
            "{}",
            format!("Found {} cluster(s).", all_clusters.len()).green()
        );
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
        .or_else(|| {
            providers
                .iter()
                .find(|(_, p)| p.provider_type() == cluster.provider)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No provider '{}' found for cluster {}",
                cluster.provider,
                cluster.name
            )
        })?;

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
