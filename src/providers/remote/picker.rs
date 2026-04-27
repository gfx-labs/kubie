use std::collections::HashSet;
use std::sync::mpsc;

use crate::providers::ClusterInfo;
use crate::providers::config::ProvidersConfig;
use super::cache;
use super::sync::fetch_all_clusters;
use crate::picker::{self, PickerItem};
use crate::settings::Settings;

/// Result of the provider-aware context picker.
pub struct PickerResult {
    /// The selected context name.
    pub context_name: String,
    /// All known cloud clusters (for hydration).
    pub clusters: Vec<ClusterInfo>,
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
    // Load cached clusters instantly (microseconds).
    let cached_clusters = cache::load_metadata()?.unwrap_or_default();

    let provider_names: HashSet<String> = cached_clusters
        .iter()
        .map(|c| c.context_name.clone())
        .collect();

    // Build initial items from cache.
    let mut items: Vec<PickerItem> = cached_clusters
        .iter()
        .map(|c| picker::provider_context_item(c))
        .collect();

    // Add local kubeconfig contexts (excluding provider-discovered ones).
    // Find kubeconfig providers from the config and list their clusters.
    let local_providers = crate::providers::config::build_providers(providers_config, None);
    for (name, provider) in &local_providers {
        if provider.provider_type() == "kubeconfig" {
            if let Ok(clusters) = provider.list_clusters(name) {
                for c in clusters {
                    if !provider_names.contains(&c.context_name) {
                        items.push(picker::provider_context_item(&c));
                    }
                }
            }
        }
    }

    // Set up background sync channel.
    let rx = if !no_sync && !providers_config.entries.is_empty() {
        let (tx, rx) = mpsc::channel::<Vec<PickerItem>>();
        let bg_config = crate::providers::config::build_providers(providers_config, None);
        let existing_names: HashSet<String> = items.iter().map(|i| i.value.clone()).collect();

        std::thread::spawn(move || {
            let fresh = fetch_all_clusters(&bg_config);
            if fresh.is_empty() {
                return;
            }

            // Save updated metadata.
            let _ = cache::save_metadata(&fresh);

            // Send only genuinely new items to the picker.
            let new_items: Vec<PickerItem> = fresh
                .iter()
                .filter(|c| !existing_names.contains(&c.context_name))
                .map(|c| picker::provider_context_item(c))
                .collect();

            if !new_items.is_empty() {
                let _ = tx.send(new_items);
            }
        });

        Some(rx)
    } else {
        None
    };

    if items.is_empty() && rx.is_none() {
        anyhow::bail!("No kubernetes contexts found.");
    }

    let all_clusters = cached_clusters;

    match picker::pick(items, rx, &settings.picker)? {
        Some(selection) => {
            // Reload metadata in case background sync updated it.
            let clusters = cache::load_metadata()?.unwrap_or(all_clusters);
            Ok(Some(PickerResult {
                context_name: selection,
                clusters,
            }))
        }
        None => Ok(None),
    }
}
