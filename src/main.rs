#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::Result;
use clap::Parser;

use cmd::meta::Kubie;
use settings::Settings;

mod providers;
mod cmd;
mod frecency;
mod ioutil;
mod kubeconfig;
mod kubectl;
mod picker;
mod session;
mod settings;
mod shell;
mod state;
mod vars;

fn main() -> Result<()> {
    let settings = Settings::load()?;

    #[cfg(feature = "remote")]
    providers::remote::cache::init();

    let kubie = Kubie::parse();

    match kubie {
        Kubie::Context {
            namespace_name,
            context_name,
            kubeconfigs,
            recursive,
            #[cfg(feature = "remote")]
            no_sync,
            #[cfg(feature = "remote")]
            local,
        } => {
            cmd::context::context(
                &settings,
                context_name,
                namespace_name,
                kubeconfigs,
                recursive,
                #[cfg(feature = "remote")]
                no_sync,
                #[cfg(feature = "remote")]
                local,
            )?;
        }
        Kubie::Namespace {
            namespace_name,
            recursive,
            unset,
        } => {
            cmd::namespace::namespace(&settings, namespace_name, recursive, unset)?;
        }
        Kubie::Info(info) => {
            cmd::info::info(info)?;
        }
        Kubie::Exec {
            context_name,
            namespace_name,
            exit_early,
            context_headers_flag,
            args,
            #[cfg(feature = "remote")]
            no_sync,
            #[cfg(feature = "remote")]
            local,
        } => {
            cmd::exec::exec(
                &settings,
                context_name,
                namespace_name,
                exit_early,
                context_headers_flag,
                args,
                #[cfg(feature = "remote")]
                no_sync,
                #[cfg(feature = "remote")]
                local,
            )?;
        }
        Kubie::Lint => {
            cmd::lint::lint(&settings)?;
        }
        Kubie::Edit { context_name } => {
            cmd::edit::edit_context(&settings, context_name)?;
        }
        Kubie::EditConfig => {
            cmd::edit::edit_config(&settings)?;
        }
        #[cfg(feature = "update")]
        Kubie::Update => {
            cmd::update::update()?;
        }
        Kubie::Delete { context_name } => {
            cmd::delete::delete_context(&settings, context_name)?;
        }
        Kubie::Export {
            context_name,
            namespace_name,
            #[cfg(feature = "remote")]
            no_sync,
            #[cfg(feature = "remote")]
            local,
        } => {
            cmd::export::export(
                &settings,
                context_name,
                namespace_name,
                #[cfg(feature = "remote")]
                no_sync,
                #[cfg(feature = "remote")]
                local,
            )?;
        }
        Kubie::GenerateCompletion(cmd) => {
            cmd::meta::generate_completion(cmd);
        }
    }

    Ok(())
}
