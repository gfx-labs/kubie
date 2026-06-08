use anyhow::Result;
use dialoguer::theme::ColorfulTheme;
use dialoguer::FuzzySelect;

use crate::cmd::{select_or_list_context, SelectResult};
use crate::kubeconfig::{self, Installed};
use crate::kubectl;
use crate::session::Session;
use crate::settings::Settings;
use crate::shell::spawn_shell;
use crate::state::State;
use crate::vars;

fn enter_context(
    settings: &Settings,
    installed: Installed,
    context_name: &str,
    namespace_name: Option<&str>,
    recursive: bool,
) -> Result<()> {
    let state = State::load()?;
    let mut session = Session::load()?;

    let kubeconfig = if context_name == "-" {
        if let Some(previous) = session.get_last_context() {
            let ns = namespace_name.or(previous.namespace.as_deref());
            installed.make_kubeconfig_for_context(&previous.context, ns)?
        } else if let Some(ref last) = state.last_context {
            let ns = namespace_name.or_else(|| state.namespace_history.get(last).and_then(|s| s.as_deref()));
            installed.make_kubeconfig_for_context(last, ns)?
        } else {
            anyhow::bail!("There is no previous context to switch to.");
        }
    } else {
        let ns = namespace_name.or_else(|| state.namespace_history.get(context_name).and_then(|s| s.as_deref()));
        installed.make_kubeconfig_for_context(context_name, ns)?
    };

    session.record_context_entry(
        &kubeconfig.contexts[0].name,
        kubeconfig.contexts[0].context.namespace.as_deref(),
    )?;

    if settings.behavior.validate_namespaces.can_list_namespaces() {
        if let Some(namespace_name) = namespace_name {
            let namespaces = kubectl::get_namespaces(Some(&kubeconfig))?;
            if !namespaces.iter().any(|x| x == namespace_name) {
                eprintln!("Warning: namespace {namespace_name} does not exist.");
            }
        }
    }

    if vars::is_kubie_active() && !recursive {
        let path = kubeconfig::get_kubeconfig_path()?;
        kubeconfig.write_to_file(path.as_path())?;
        session.save(None)?;
    } else {
        spawn_shell(settings, kubeconfig, &session)?;
    }

    Ok(())
}

/// Find the actual context name inside an Installed set for a provider cluster.
///
/// The context name in the downloaded kubeconfig may not match the name we use
/// in the picker (e.g. Rancher generates its own context names, DO uses
/// "do-region-name"). We try:
/// 1. Exact match on our display name
/// 2. Any context whose name contains the cluster name
/// 3. The first context from the provider kubeconfig file
#[cfg(feature = "remote")]
fn find_provider_context_name(installed: &Installed, display_name: &str) -> String {
    // Exact match.
    if installed.find_context_by_name(display_name).is_some() {
        return display_name.to_string();
    }

    // Fuzzy: context name contains the display name or vice versa.
    for ctx in &installed.contexts {
        if ctx.item.name.contains(display_name) || display_name.contains(&ctx.item.name) {
            return ctx.item.name.clone();
        }
    }

    // Last resort: first context in the list (the provider kubeconfig was loaded first).
    installed
        .contexts
        .first()
        .map(|c| c.item.name.clone())
        .unwrap_or_else(|| display_name.to_string())
}

/// Fuzzy-match a context name against a list of known context names.
/// Shows a fuzzy-searchable selector pre-filled with the query.
/// Returns Some(resolved_name) if selected, None if cancelled.
fn fuzzy_resolve_context(query: &str, context_names: &[String]) -> Result<Option<String>> {
    const MIN_SIMILARITY: f64 = 0.6;

    let query_lower = query.to_lowercase();

    // Pre-filter to reasonable candidates using Jaro-Winkler.
    let mut candidates: Vec<(&str, f64)> = context_names
        .iter()
        .map(|name| {
            let name_lower = name.to_lowercase();
            let jw = strsim::jaro_winkler(&query_lower, &name_lower);

            let substring_bonus = if name_lower.contains(&query_lower) || query_lower.contains(&name_lower) {
                0.15
            } else {
                0.0
            };

            let query_parts: Vec<&str> = query_lower.split(['-', '_']).filter(|s| !s.is_empty()).collect();
            let total_parts = query_parts.len().max(1);
            let matching_parts = query_parts.iter().filter(|part| name_lower.contains(*part)).count();
            let segment_bonus = (matching_parts as f64 / total_parts as f64) * 0.1;

            let score = (jw + substring_bonus + segment_bonus).min(1.0);
            (name.as_str(), score)
        })
        .filter(|(_, score)| *score >= MIN_SIMILARITY)
        .collect();

    if candidates.is_empty() {
        return Ok(None);
    }

    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let items: Vec<&str> = candidates.iter().map(|(name, _)| *name).collect();

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("did you mean")
        .items(&items)
        .default(0)
        .with_initial_text(query)
        .interact_opt()?;

    Ok(selection.map(|i| items[i].to_string()))
}

pub fn context(
    settings: &Settings,
    context_name: Option<String>,
    namespace_name: Option<String>,
    kubeconfigs: Vec<String>,
    recursive: bool,
    #[cfg(feature = "remote")] no_sync: bool,
    #[cfg(feature = "remote")] local: bool,
) -> Result<()> {
    // If providers are configured and no explicit kubeconfigs given, use the provider-aware picker.
    #[cfg(feature = "remote")]
    {
        if !local && !settings.providers.entries.is_empty() && kubeconfigs.is_empty() && context_name.is_none() {
            return context_with_providers(settings, namespace_name, recursive, no_sync);
        }
    }

    let mut installed = if kubeconfigs.is_empty() {
        kubeconfig::get_installed_contexts(settings)?
    } else {
        kubeconfig::get_kubeconfigs_contexts(&kubeconfigs)?
    };

    let context_name = match context_name {
        Some(context_name) => context_name,
        None => match select_or_list_context(settings, &mut installed)? {
            SelectResult::Selected(x) => x,
            _ => return Ok(()),
        },
    };

    // Collect all known context names for fuzzy matching (local + provider).
    let mut all_context_names: Vec<String> = installed.contexts.iter().map(|c| c.item.name.clone()).collect();

    #[cfg(feature = "remote")]
    let provider_clusters = if !local && !settings.providers.entries.is_empty() {
        let clusters = crate::providers::remote::cache::load_metadata()
            .ok()
            .flatten()
            .unwrap_or_default();
        for c in &clusters {
            if !all_context_names.contains(&c.context_name) {
                all_context_names.push(c.context_name.clone());
            }
        }
        clusters
    } else {
        Vec::new()
    };

    // Resolve the context name: exact match first, then fuzzy.
    let resolved = if all_context_names.iter().any(|n| n == &context_name) {
        context_name.clone()
    } else {
        match fuzzy_resolve_context(&context_name, &all_context_names)? {
            Some(name) => name,
            None => anyhow::bail!("No context matching '{context_name}'"),
        }
    };

    // Check if it's a provider context.
    #[cfg(feature = "remote")]
    if !local && !settings.providers.entries.is_empty() {
        if let Some(cluster) = crate::providers::remote::cache::find_cluster_for_context(&resolved, &provider_clusters)
        {
            let prov = crate::providers::config::build_providers(&settings.providers, None);
            crate::providers::remote::sync::ensure_hydrated(&cluster, &prov)?;

            let config_file = crate::providers::remote::cache::configs_dir()
                .join(crate::providers::remote::cache::config_filename(&cluster));

            let mut kubeconfigs = vec![config_file.to_string_lossy().to_string()];
            let normal_paths = settings.get_kube_configs_paths()?;
            for p in normal_paths {
                kubeconfigs.push(p.to_string_lossy().to_string());
            }
            let installed = kubeconfig::get_kubeconfigs_contexts(&kubeconfigs)?;
            crate::providers::remote::sync::cleanup_config(&config_file);

            let actual_ctx = find_provider_context_name(&installed, &resolved);
            return enter_context(settings, installed, &actual_ctx, namespace_name.as_deref(), recursive);
        }
    }

    enter_context(settings, installed, &resolved, namespace_name.as_deref(), recursive)
}

/// Handle context switching through the provider-aware picker.
#[cfg(feature = "remote")]
fn context_with_providers(
    settings: &Settings,
    namespace_name: Option<String>,
    recursive: bool,
    no_sync: bool,
) -> Result<()> {
    use crate::providers;

    let result = match providers::remote::picker::pick_context(settings, &settings.providers, no_sync)? {
        Some(r) => r,
        None => return Ok(()),
    };

    let ctx_name = &result.context_name;
    let clusters = &result.clusters;

    // Check if this is a provider-discovered context.
    if let Some(cluster) = providers::remote::cache::find_cluster_for_context(ctx_name, clusters) {
        let prov = providers::config::build_providers(&settings.providers, None);
        providers::remote::sync::ensure_hydrated(&cluster, &prov)?;

        let config_file =
            providers::remote::cache::configs_dir().join(providers::remote::cache::config_filename(&cluster));

        // Load the provider kubeconfig + normal configs into memory, then clean up the temp file.
        let mut kubeconfigs = vec![config_file.to_string_lossy().to_string()];
        let normal_paths = settings.get_kube_configs_paths()?;
        for p in normal_paths {
            kubeconfigs.push(p.to_string_lossy().to_string());
        }
        let installed = kubeconfig::get_kubeconfigs_contexts(&kubeconfigs)?;
        providers::remote::sync::cleanup_config(&config_file);

        // The context name inside the downloaded kubeconfig may differ from our
        // display name. Find the actual context name from the file we just loaded.
        let actual_ctx = find_provider_context_name(&installed, ctx_name);
        return enter_context(settings, installed, &actual_ctx, namespace_name.as_deref(), recursive);
    }

    // It's a local kubeconfig context.
    let installed = kubeconfig::get_installed_contexts(settings)?;
    enter_context(settings, installed, ctx_name, namespace_name.as_deref(), recursive)
}
