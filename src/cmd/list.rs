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
    #[serde(skip_serializing_if = "Option::is_none")]
    selector: Option<String>,
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
            selector: None,
        });
    }

    #[cfg(feature = "remote")]
    if !local && !settings.providers.entries.is_empty() {
        use crate::providers::remote::{cache, sync};
        use std::collections::{HashMap, HashSet};

        let configured = crate::providers::config::build_providers_with_errors(&settings.providers, None);
        let active_accounts: HashSet<(String, String)> = configured
            .providers
            .iter()
            .map(|(account, provider)| (provider.provider_type().to_string(), account.clone()))
            .collect();

        // Use the cache when it has data; only sync when it is empty (or forced).
        let clusters = match cache::load_metadata()?.unwrap_or_default() {
            c if !c.is_empty() || no_sync => c,
            _ => sync::full_sync(&configured.providers).unwrap_or_default(),
        };
        let mut alias_counts = HashMap::new();
        for cluster in &clusters {
            if active_accounts.contains(&(cluster.provider.clone(), cluster.account.clone())) {
                *alias_counts
                    .entry(crate::cmd::selector::canonical_name(cluster))
                    .or_insert(0usize) += 1;
            }
        }

        for c in clusters {
            if !active_accounts.contains(&(c.provider.clone(), c.account.clone())) {
                continue;
            }
            let canonical = crate::cmd::selector::canonical_name(&c);
            let selector = if alias_counts.get(&canonical).is_some_and(|count| *count > 1)
                || c.context_name.contains('/')
                || c.context_name.starts_with("@id:")
            {
                crate::cmd::selector::id_alias(&c)
            } else {
                canonical
            };
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
                selector: Some(selector),
            });
        }
    }

    out.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.selector.cmp(&b.selector))
            .then_with(|| a.source.cmp(&b.source))
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // Plain output: one context name per line, easy to pipe.
    // When attached to a terminal, add a short annotation for humans.
    let tty = io::stdout().is_terminal();
    for c in &out {
        let bare_name_conflicts = out.iter().filter(|other| other.name == c.name).count() > 1;
        let display_name = if bare_name_conflicts {
            c.selector.as_deref().unwrap_or(&c.name)
        } else {
            &c.name
        };
        if tty {
            let origin = match (&c.provider, &c.account) {
                (Some(p), Some(a)) => format!("{p}/{a}"),
                _ => c.source.clone(),
            };
            println!("{}\t{}", display_name, origin);
        } else {
            println!("{}", display_name);
        }
    }

    Ok(())
}
