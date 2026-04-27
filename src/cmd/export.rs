use anyhow::{anyhow, Result};

use crate::kubeconfig;
use crate::settings::Settings;

pub fn export(
    settings: &Settings,
    context_name: String,
    namespace_name: String,
    #[cfg(feature = "remote")] no_sync: bool,
    #[cfg(feature = "remote")] local: bool,
) -> Result<()> {
    // If providers are configured, ensure their kubeconfigs are available.
    #[cfg(feature = "remote")]
    let extra_kubeconfigs = {
        if !local && !settings.providers.entries.is_empty() {
            get_provider_kubeconfig_paths(settings, no_sync)
        } else {
            Vec::new()
        }
    };

    #[cfg(feature = "remote")]
    let installed = if !extra_kubeconfigs.is_empty() {
        let mut all_paths: Vec<String> = Vec::new();
        for p in extra_kubeconfigs {
            all_paths.push(p.to_string_lossy().to_string());
        }
        for p in settings.get_kube_configs_paths()? {
            all_paths.push(p.to_string_lossy().to_string());
        }
        kubeconfig::get_kubeconfigs_contexts(&all_paths)?
    } else {
        kubeconfig::get_installed_contexts(settings)?
    };

    #[cfg(not(feature = "remote"))]
    let installed = kubeconfig::get_installed_contexts(settings)?;

    let matching = installed.get_contexts_matching(&context_name, settings.behavior.allow_multiple_context_patterns);

    if matching.is_empty() {
        return Err(anyhow!("No context matching {}", context_name));
    }

    for context_src in matching {
        let kubeconfig = installed.make_kubeconfig_for_context(&context_src.item.name, Some(&namespace_name))?;
        let temp_config_file = tempfile::Builder::new()
            .prefix("kubie-config")
            .suffix(".yaml")
            .tempfile()?;
        kubeconfig.write_to_file(temp_config_file.path())?;
        let (_, path) = temp_config_file.keep()?;
        println!("{}", path.display());
    }

    std::process::exit(0);
}

/// Get kubeconfig paths from providers (hydrated on demand).
#[cfg(feature = "remote")]
fn get_provider_kubeconfig_paths(
    settings: &Settings,
    no_sync: bool,
) -> Vec<std::path::PathBuf> {
    use crate::providers;

    let prov = providers::config::build_providers(&settings.providers, None);
    if prov.is_empty() {
        return Vec::new();
    }

    let clusters = if no_sync {
        providers::remote::cache::load_metadata().ok().flatten().unwrap_or_default()
    } else {
        match providers::remote::cache::load_metadata().ok().flatten() {
            Some(c) if !c.is_empty() => c,
            _ => providers::remote::sync::full_sync(&prov).unwrap_or_default(),
        }
    };

    providers::remote::sync::hydrate_all_configs(&clusters, &prov)
}
