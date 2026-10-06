//! Non-interactive context resolution shared by `exec`, `export` and `list`.
//!
//! The goal is to be *smart*: never touch the network when the requested
//! context is already available locally or already cached, and only hydrate
//! the kubeconfig(s) that are actually needed instead of every known cluster.

use anyhow::Result;
use wildmatch::WildMatch;

#[cfg(feature = "remote")]
use super::selector;
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

    #[cfg(feature = "remote")]
    if !local && selector::is_qualified(pattern) {
        return resolve_qualified(settings, pattern, allow_multiple, no_sync);
    }

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

#[cfg(feature = "remote")]
fn resolve_qualified(settings: &Settings, pattern: &str, allow_multiple: bool, no_sync: bool) -> Result<Resolved> {
    use crate::providers::remote::{cache, sync};
    use std::collections::HashMap;

    let _ = selector::select(pattern, &[], allow_multiple)?;
    let parsed: Vec<_> = if allow_multiple {
        pattern.split_whitespace().collect()
    } else {
        vec![pattern]
    };
    let provider_names: Vec<_> = parsed
        .iter()
        .filter_map(|part| {
            let mut components = part.split('/');
            let provider = components.next()?;
            let account = components.next().filter(|_| part.matches('/').count() == 2);
            Some((provider, account))
        })
        .collect();
    let build = crate::providers::config::build_providers_with_errors(&settings.providers, None);
    let providers = build.providers;
    for (kind, requested_account) in &provider_names {
        let active_configured = settings.providers.entries.iter().any(|(account, entry)| {
            entry.enabled
                && entry.provider_type == *kind
                && requested_account.is_none_or(|requested| requested == account)
        });
        if let Some(error) = build.errors.iter().find(|error| {
            error.provider_type == *kind && requested_account.is_none_or(|requested| requested == error.source)
        }) {
            anyhow::bail!(
                "Provider '{}' account '{}' could not be initialized: {}",
                kind,
                error.source,
                error.message
            );
        }
        if active_configured
            && !providers.iter().any(|(account, provider)| {
                provider.provider_type() == *kind && requested_account.is_none_or(|requested| requested == account)
            })
        {
            anyhow::bail!("No active configured provider '{}' account matched the selector", kind);
        }
        if !active_configured {
            let disabled = settings.providers.entries.iter().any(|(account, entry)| {
                !entry.enabled
                    && entry.provider_type == *kind
                    && requested_account.is_none_or(|requested| requested == account)
            });
            if disabled {
                anyhow::bail!(
                    "Provider '{}' account is disabled; enable it in provider configuration",
                    kind
                );
            }
            anyhow::bail!("No enabled provider configuration found for provider kind '{}'", kind);
        }
    }

    let active_accounts: Vec<_> = providers
        .iter()
        .map(|(account, provider)| (provider.provider_type().to_string(), account.clone()))
        .collect();
    let mut all_clusters = cache::load_metadata()?.unwrap_or_default();
    let mut clusters: Vec<_> = all_clusters
        .iter()
        .filter(|cluster| active_accounts.contains(&(cluster.provider.clone(), cluster.account.clone())))
        .cloned()
        .collect();
    let mut selection_error = None;
    let mut matching = match selector::select(pattern, &clusters, allow_multiple) {
        Ok(Some(matching)) => matching,
        Ok(None) => Vec::new(),
        Err(error) if selector::is_incomplete_selection(&error) => {
            selection_error = Some(error);
            Vec::new()
        }
        Err(error) => return Err(error),
    };

    if (matching.is_empty() || selection_error.is_some()) && !no_sync {
        let targeted: Vec<_> = crate::providers::config::build_providers(&settings.providers, None)
            .into_iter()
            .filter(|(account, provider)| {
                provider_names.iter().any(|(kind, requested_account)| {
                    provider.provider_type() == *kind && requested_account.is_none_or(|requested| requested == account)
                })
            })
            .collect();
        if !targeted.is_empty() {
            let refresh_scopes: Vec<_> = targeted
                .iter()
                .map(|(account, provider)| (provider.provider_type().to_string(), account.as_str()))
                .collect();
            let refresh = sync::fetch_all_clusters_with_errors(&targeted);
            if !refresh.errors.is_empty() {
                let sources = refresh
                    .errors
                    .iter()
                    .map(|error| format!("{} ({})", error.source, error.provider_type))
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "Could not refresh qualified provider metadata for {sources}; no partial selection was used"
                );
            }
            let refreshed = refresh.clusters;
            all_clusters.retain(|cluster| {
                !refresh_scopes
                    .iter()
                    .any(|(kind, account)| cluster.provider == *kind && cluster.account == *account)
            });
            all_clusters.extend(refreshed);
            cache::save_metadata(&all_clusters)?;
            clusters = all_clusters
                .iter()
                .filter(|cluster| active_accounts.contains(&(cluster.provider.clone(), cluster.account.clone())))
                .cloned()
                .collect();
            matching = selector::select(pattern, &clusters, allow_multiple)?.unwrap_or_default();
            selection_error = None;
        }
    }

    if matching.is_empty() {
        if let Some(error) = selection_error {
            return Err(error);
        }
        anyhow::bail!("No provider clusters matched '{pattern}'. Check the provider/account selector or refresh provider metadata.");
    }

    // Resolve each selected provider file independently so current-context and
    // duplicate internal context names cannot cause cross-source selection.
    let mut alias_counts = HashMap::new();
    for cluster in &clusters {
        *alias_counts.entry(selector::canonical_name(cluster)).or_insert(0usize) += 1;
    }
    let mut paths = Vec::new();
    let mut expected_by_path = HashMap::new();
    let mut alias_by_path = HashMap::new();
    for cluster in &matching {
        let exact_provider = providers
            .iter()
            .any(|(account, provider)| account == &cluster.account && provider.provider_type() == cluster.provider);
        if !exact_provider {
            anyhow::bail!(
                "Provider '{}' account '{}' is no longer enabled",
                cluster.provider,
                cluster.account
            );
        }
        sync::ensure_hydrated(cluster, &providers)?;
        let path = cache::configs_dir().join(cache::config_filename(cluster));
        if !path.is_file() {
            anyhow::bail!("Provider kubeconfig cache is missing for {}", cluster.context_name);
        }
        let path_string = path.to_string_lossy().to_string();
        let source: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&path)?)?;
        let contexts = source
            .get("contexts")
            .and_then(serde_yaml::Value::as_sequence)
            .cloned()
            .unwrap_or_default();
        let names: Vec<_> = contexts
            .iter()
            .filter_map(|context| {
                context
                    .get("name")
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let current = source
            .get("current-context")
            .and_then(serde_yaml::Value::as_str)
            .filter(|name| names.iter().any(|candidate| candidate == *name));
        let chosen = if let Some(current) = current {
            if names.iter().filter(|name| **name == current).count() != 1 {
                anyhow::bail!("Provider kubeconfig current-context '{}' is duplicated", current);
            }
            Some(current.to_owned())
        } else {
            let expected: Vec<_> = names.iter().filter(|name| **name == cluster.context_name).collect();
            if expected.len() > 1 {
                anyhow::bail!("Provider kubeconfig context '{}' is duplicated", cluster.context_name);
            }
            let named: Vec<_> = names.iter().filter(|name| **name == cluster.name).collect();
            if expected.is_empty() && named.len() > 1 {
                anyhow::bail!("Provider kubeconfig context '{}' is duplicated", cluster.name);
            }
            expected
                .first()
                .map(|name| (*name).clone())
                .or_else(|| named.first().map(|name| (*name).clone()))
                .or_else(|| (names.len() == 1).then(|| names[0].clone()))
        };
        let Some(chosen) = chosen else {
            anyhow::bail!(
                "No unambiguous current-context, context name, or single context in provider kubeconfig for {}",
                cluster.context_name
            );
        };
        paths.push(path_string.clone());
        expected_by_path.insert(path_string.clone(), chosen);
        let canonical = selector::canonical_name(cluster);
        let alias = if alias_counts.get(&canonical).is_some_and(|count| *count > 1)
            || cluster.context_name.contains('/')
            || cluster.context_name.starts_with("@id:")
        {
            selector::id_alias(cluster)
        } else {
            canonical
        };
        alias_by_path.insert(path_string, alias);
    }

    if paths.is_empty() {
        anyhow::bail!("No usable provider kubeconfigs were available for '{pattern}'");
    }
    let mut hydrated = kubeconfig::get_kubeconfigs_contexts(&paths)?;
    let mut context_names = Vec::new();
    hydrated.contexts.retain_mut(|context| {
        let path = context.source.to_string_lossy().to_string();
        let Some(expected) = expected_by_path.get(&path) else {
            return false;
        };
        if context.item.name != *expected {
            return false;
        }
        if let Some(alias) = alias_by_path.get(&path) {
            context.item.name = alias.clone();
            context_names.push(alias.clone());
        }
        true
    });
    if context_names.is_empty() {
        anyhow::bail!("No selected provider contexts could be loaded for '{pattern}'");
    }
    for context in &hydrated.contexts {
        let clusters: Vec<_> = hydrated
            .clusters
            .iter()
            .filter(|cluster| cluster.source == context.source && cluster.item.name == context.item.context.cluster)
            .collect();
        let users: Vec<_> = hydrated
            .users
            .iter()
            .filter(|user| user.source == context.source && user.item.name == context.item.context.user)
            .collect();
        if clusters.len() != 1 || users.len() != 1 {
            anyhow::bail!(
                "Provider context '{}' has {} same-source cluster references and {} same-source user references; expected exactly one of each",
                context.item.name,
                clusters.len(),
                users.len()
            );
        }
    }
    context_names.sort();
    context_names.dedup();
    Ok(Resolved {
        installed: hydrated,
        context_names,
    })
}
