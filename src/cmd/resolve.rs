//! Non-interactive context resolution shared by `exec`, `export` and `list`.
//!
//! The goal is to be *smart*: never touch the network when the requested
//! context is already available locally or already cached, and only hydrate
//! the kubeconfig(s) that are actually needed instead of every known cluster.

use anyhow::Result;
use wildmatch::WildMatch;

use crate::kubeconfig::{self, Installed};
use crate::settings::Settings;

/// A resolved set of contexts: the merged kubeconfigs and the real context
/// names to use (provider kubeconfigs may name their context differently than
/// the display name we show in listings).
pub struct Resolved {
    pub installed: Installed,
    pub context_names: Vec<String>,
}

fn matches(pattern: &str, name: &str, allow_multiple: bool) -> bool {
    let patterns: Vec<&str> = if allow_multiple {
        pattern.split_whitespace().collect()
    } else {
        vec![pattern]
    };
    patterns.iter().any(|p| WildMatch::new(p).matches(name))
}

/// Resolve a context pattern into usable kubeconfigs.
///
/// Resolution order (cheapest first):
/// 1. Local kubeconfig contexts.
/// 2. Cached provider metadata, hydrating only the matching cluster(s).
/// 3. A full provider sync, only if the cache had no match and syncing is allowed.
pub fn resolve_contexts(
    settings: &Settings,
    pattern: &str,
    #[cfg(feature = "remote")] no_sync: bool,
    #[cfg(feature = "remote")] local: bool,
) -> Result<Resolved> {
    let allow_multiple = settings.behavior.allow_multiple_context_patterns;
    let installed = kubeconfig::get_installed_contexts(settings)?;

    let local_matches: Vec<String> = installed
        .contexts
        .iter()
        .filter(|c| matches(pattern, &c.item.name, allow_multiple))
        .map(|c| c.item.name.clone())
        .collect();

    #[cfg(not(feature = "remote"))]
    if !local_matches.is_empty() {
        return Ok(Resolved {
            installed,
            context_names: local_matches,
        });
    }

    #[cfg(feature = "remote")]
    if !local && !settings.providers.entries.is_empty() {
        use crate::providers::remote::{cache, sync};

        let prov = crate::providers::config::build_providers(&settings.providers, None);

        // Try the cache first, then fall back to a full sync.
        let mut clusters = cache::load_metadata()?.unwrap_or_default();
        let mut matching: Vec<_> = clusters
            .iter()
            .filter(|c| matches(pattern, &c.context_name, allow_multiple))
            .cloned()
            .collect();

        // Only pay for a network sync when nothing matched at all, locally or in cache.
        if matching.is_empty() && local_matches.is_empty() && !no_sync {
            clusters = sync::full_sync(&prov).unwrap_or_default();
            matching = clusters
                .iter()
                .filter(|c| matches(pattern, &c.context_name, allow_multiple))
                .cloned()
                .collect();
        }

        if !matching.is_empty() {
            let mut paths: Vec<String> = Vec::new();
            let mut context_names: Vec<String> = Vec::new();

            for cluster in &matching {
                if let Err(e) = sync::ensure_hydrated(cluster, &prov) {
                    eprintln!(
                        "Warning: failed to fetch kubeconfig for {}: {e:#}",
                        cluster.context_name
                    );
                    continue;
                }
                let path = cache::configs_dir().join(cache::config_filename(cluster));
                if !path.exists() {
                    continue;
                }
                let path_str = path.to_string_lossy().to_string();

                // The context name inside a provider kubeconfig may differ from
                // the display name, so read it back from the file itself.
                if let Ok(single) = kubeconfig::get_kubeconfigs_contexts(&vec![path_str.clone()]) {
                    for ctx in &single.contexts {
                        if !context_names.contains(&ctx.item.name) {
                            context_names.push(ctx.item.name.clone());
                        }
                    }
                }
                paths.push(path_str);
            }

            if !context_names.is_empty() {
                for p in settings.get_kube_configs_paths()? {
                    paths.push(p.to_string_lossy().to_string());
                }
                let installed = kubeconfig::get_kubeconfigs_contexts(&paths)?;
                for name in local_matches {
                    if !context_names.contains(&name) {
                        context_names.push(name);
                    }
                }
                context_names.sort();
                return Ok(Resolved {
                    installed,
                    context_names,
                });
            }
        }
    }

    Ok(Resolved {
        installed,
        context_names: local_matches,
    })
}
