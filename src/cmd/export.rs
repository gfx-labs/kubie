use anyhow::{anyhow, Result};

use crate::settings::Settings;

pub fn export(
    settings: &Settings,
    context_name: String,
    namespace_name: String,
    #[cfg(feature = "remote")] no_sync: bool,
    #[cfg(feature = "remote")] local: bool,
) -> Result<()> {
    let resolved = crate::cmd::resolve::resolve_contexts(
        settings,
        &context_name,
        #[cfg(feature = "remote")]
        no_sync,
        #[cfg(feature = "remote")]
        local,
    )?;

    if resolved.context_names.is_empty() {
        return Err(anyhow!("No context matching {}", context_name));
    }

    for name in &resolved.context_names {
        let kubeconfig = resolved
            .installed
            .make_kubeconfig_for_context(name, Some(&namespace_name))?;
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
