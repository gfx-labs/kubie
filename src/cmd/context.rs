use anyhow::Result;

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

    // When providers are enabled and a context name was given explicitly, check if it's a provider context.
    #[cfg(feature = "remote")]
    if !local && !settings.providers.entries.is_empty() {
        if try_provider_context(settings, &context_name, namespace_name.as_deref(), recursive, no_sync)?.is_some() {
            return Ok(());
        }
    }

    enter_context(settings, installed, &context_name, namespace_name.as_deref(), recursive)
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

        let config_file = providers::remote::cache::configs_dir().join(providers::remote::cache::config_filename(&cluster));

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

/// Try to handle a named context as a provider-discovered context.
/// Returns Ok(Some(())) if handled, Ok(None) if not a provider context.
#[cfg(feature = "remote")]
fn try_provider_context(
    settings: &Settings,
    context_name: &str,
    namespace_name: Option<&str>,
    recursive: bool,
    no_sync: bool,
) -> Result<Option<()>> {
    use crate::providers;

    let prov = providers::config::build_providers(&settings.providers, None);

    let mut clusters = providers::remote::cache::load_metadata()?.unwrap_or_default();
    if providers::remote::cache::find_cluster_for_context(context_name, &clusters).is_none() && !no_sync && !prov.is_empty() {
        clusters = providers::remote::sync::full_sync(&prov)?;
    }

    if let Some(cluster) = providers::remote::cache::find_cluster_for_context(context_name, &clusters) {
        providers::remote::sync::ensure_hydrated(&cluster, &prov)?;

        let config_file = providers::remote::cache::configs_dir().join(providers::remote::cache::config_filename(&cluster));

        let mut kubeconfigs = vec![config_file.to_string_lossy().to_string()];
        let normal_paths = settings.get_kube_configs_paths()?;
        for p in normal_paths {
            kubeconfigs.push(p.to_string_lossy().to_string());
        }
        let installed = kubeconfig::get_kubeconfigs_contexts(&kubeconfigs)?;
        providers::remote::sync::cleanup_config(&config_file);

        let actual_ctx = find_provider_context_name(&installed, context_name);
        enter_context(settings, installed, &actual_ctx, namespace_name, recursive)?;
        return Ok(Some(()));
    }

    Ok(None)
}
