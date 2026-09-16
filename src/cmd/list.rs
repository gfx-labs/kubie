use std::io::{self, IsTerminal};

use anyhow::Result;
use serde::Serialize;

use crate::kubeconfig;
use crate::settings::Settings;

#[derive(Serialize)]
struct ListedContext {
    name: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
}

/// Non-interactive listing of every known context (local + provider).
pub fn list(
    settings: &Settings,
    json: bool,
    #[cfg(feature = "remote")] no_sync: bool,
    #[cfg(feature = "remote")] local: bool,
) -> Result<()> {
    let mut out: Vec<ListedContext> = Vec::new();

    let installed = kubeconfig::get_installed_contexts(settings)?;
    for ctx in &installed.contexts {
        let cluster_name = ctx.item.context.cluster.clone();
        let server = installed
            .find_cluster_by_name(&cluster_name, &ctx.source)
            .and_then(|c| c.item.cluster.get("server").and_then(|v| v.as_str()).map(String::from));

        out.push(ListedContext {
            name: ctx.item.name.clone(),
            source: ctx.source.to_string_lossy().to_string(),
            cluster: Some(cluster_name),
            server,
            namespace: ctx.item.context.namespace.clone(),
            provider: None,
            account: None,
        });
    }

    #[cfg(feature = "remote")]
    if !local && !settings.providers.entries.is_empty() {
        use crate::providers::remote::{cache, sync};

        // Use the cache when it has data; only sync when it is empty (or forced).
        let clusters = match cache::load_metadata()?.unwrap_or_default() {
            c if !c.is_empty() || no_sync => c,
            _ => {
                let prov = crate::providers::config::build_providers(&settings.providers, None);
                sync::full_sync(&prov).unwrap_or_default()
            }
        };

        for c in clusters {
            if out.iter().any(|o| o.name == c.context_name) {
                continue;
            }
            out.push(ListedContext {
                name: c.context_name,
                source: "provider".to_string(),
                cluster: Some(c.name),
                server: c
                    .metadata
                    .iter()
                    .find(|f| f.label.eq_ignore_ascii_case("server") || f.label.eq_ignore_ascii_case("endpoint"))
                    .map(|f| f.value.clone()),
                namespace: None,
                provider: Some(c.provider),
                account: Some(c.account),
            });
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));

    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // Plain output: one context name per line, easy to pipe.
    // When attached to a terminal, add a short annotation for humans.
    let tty = io::stdout().is_terminal();
    for c in &out {
        if tty {
            let origin = match (&c.provider, &c.account) {
                (Some(p), Some(a)) => format!("{p}/{a}"),
                _ => c.source.clone(),
            };
            println!("{}\t{}", c.name, origin);
        } else {
            println!("{}", c.name);
        }
    }

    Ok(())
}
