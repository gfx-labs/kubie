use std::collections::HashSet;
use std::sync::mpsc;

use super::cache;
use super::sync::fetch_all_clusters_with_errors;
use crate::picker::{self, PickerError, PickerItem, PickerUpdate};
use crate::providers::config::ProvidersConfig;
use crate::settings::Settings;

/// Result of the provider-aware context picker.
pub struct PickerResult {
    /// The selected context selector.
    pub context_name: String,
    /// Whether the selected value came from a provider-backed picker item.
    pub provider_selector: bool,
}

fn provider_picker_item(
    cluster: &crate::providers::ClusterInfo,
    clusters: &[crate::providers::ClusterInfo],
) -> PickerItem {
    let mut item = picker::provider_context_item(cluster);
    let canonical = crate::cmd::selector::canonical_name(cluster);
    let aliases = clusters
        .iter()
        .filter(|candidate| crate::cmd::selector::canonical_name(candidate) == canonical)
        .count();
    let disambiguated = aliases > 1 || cluster.context_name.contains('/') || cluster.context_name.starts_with("@id:");
    item.value = if disambiguated {
        crate::cmd::selector::id_alias(cluster)
    } else {
        canonical
    };
    item.display = if disambiguated {
        format!(
            "{} ({}/{}, id: {})",
            cluster.name, cluster.provider, cluster.account, cluster.id
        )
    } else {
        format!("{} ({}/{})", cluster.name, cluster.provider, cluster.account)
    };
    item
}

/// Interactive context picker showing both kubeconfig contexts and provider clusters.
///
/// Opens the picker immediately with cached data. A background thread fetches
/// fresh provider data and streams new items into the picker in real time.
pub fn pick_context(
    settings: &Settings,
    providers_config: &ProvidersConfig,
    no_sync: bool,
) -> anyhow::Result<Option<PickerResult>> {
    let configured = crate::providers::config::build_providers_with_errors(providers_config, None);
    let active_accounts: HashSet<(String, String)> = configured
        .providers
        .iter()
        .map(|(account, provider)| (provider.provider_type().to_string(), account.clone()))
        .collect();

    // Load cached clusters instantly, excluding identities from disabled or invalid providers.
    let mut cached_clusters = cache::load_metadata()?.unwrap_or_default();
    cached_clusters.retain(|cluster| active_accounts.contains(&(cluster.provider.clone(), cluster.account.clone())));

    let mut picker_clusters = cached_clusters.clone();
    for (name, provider) in &configured.providers {
        if provider.provider_type() == "kubeconfig" {
            if let Ok(clusters) = provider.list_clusters(name) {
                for c in clusters {
                    if !picker_clusters.iter().any(|existing| {
                        existing.provider == c.provider && existing.account == c.account && existing.id == c.id
                    }) {
                        picker_clusters.push(c);
                    }
                }
            }
        }
    }
    let items: Vec<PickerItem> = picker_clusters
        .iter()
        .map(|cluster| provider_picker_item(cluster, &picker_clusters))
        .collect();
    let existing_names: HashSet<String> = items.iter().map(|item| item.value.clone()).collect();

    // Set up background sync channel.
    let rx = if !no_sync && !providers_config.entries.is_empty() {
        let (tx, rx) = mpsc::channel::<PickerUpdate>();
        let bg_config = configured.providers;
        let config_errors = configured.errors;

        std::thread::spawn(move || {
            for error in config_errors {
                let _ = tx.send(PickerUpdate::Error(PickerError {
                    source: error.source,
                    provider_type: error.provider_type,
                    message: error.message,
                }));
            }

            let result = fetch_all_clusters_with_errors(&bg_config);
            let fresh = result.clusters;

            // Save updated metadata.
            if !fresh.is_empty() {
                let _ = cache::save_metadata(&fresh);
            }

            // Send only genuinely new items to the picker.
            let new_items: Vec<PickerItem> = fresh
                .iter()
                .map(|cluster| provider_picker_item(cluster, &fresh))
                .filter(|item| !existing_names.contains(&item.value))
                .collect();

            if !new_items.is_empty() {
                let _ = tx.send(PickerUpdate::Items(new_items));
            }

            for error in result.errors {
                let _ = tx.send(PickerUpdate::Error(PickerError {
                    source: error.source,
                    provider_type: error.provider_type,
                    message: error.message,
                }));
            }
        });

        Some(rx)
    } else {
        None
    };

    if items.is_empty() && rx.is_none() {
        anyhow::bail!("No kubernetes contexts found.");
    }

    match picker::pick(items, rx, &settings.picker)? {
        Some(selection) => Ok(Some(PickerResult {
            provider_selector: true,
            context_name: selection,
        })),
        None => Ok(None),
    }
}
