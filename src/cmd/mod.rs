use std::io::{self, IsTerminal};
use std::sync::mpsc;
use std::thread;

use anyhow::{bail, Result};

use crate::kubeconfig::Installed;
use crate::kubectl;
use crate::picker::{self, PickerItem};
use crate::settings::Settings;

pub mod context;
pub mod delete;
pub mod edit;
pub mod exec;
pub mod export;
pub mod info;
pub mod lint;
pub mod meta;
pub mod namespace;
#[cfg(feature = "update")]
pub mod update;

pub enum SelectResult {
    Cancelled,
    Listed,
    Selected(String),
}

pub fn select_or_list_context(settings: &Settings, installed: &mut Installed) -> Result<SelectResult> {
    installed.contexts.sort_by(|a, b| a.item.name.cmp(&b.item.name));

    if installed.contexts.is_empty() {
        bail!("No contexts found");
    }
    if installed.contexts.len() == 1 {
        return Ok(SelectResult::Selected(installed.contexts[0].item.name.clone()));
    }

    if io::stdout().is_terminal() {
        let items: Vec<PickerItem> = installed
            .contexts
            .iter()
            .map(|ctx| {
                let cluster_name = &ctx.item.context.cluster;
                let server = installed
                    .find_cluster_by_name(cluster_name, &ctx.source)
                    .and_then(|c| {
                        c.item
                            .cluster
                            .get("server")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();

                picker::local_context_item(
                    &ctx.item.name,
                    cluster_name,
                    &server,
                    ctx.item.context.namespace.as_deref(),
                    &ctx.source,
                )
            })
            .collect();

        match picker::pick(items, None, &settings.picker)? {
            Some(name) => Ok(SelectResult::Selected(name)),
            None => Ok(SelectResult::Cancelled),
        }
    } else {
        let context_names: Vec<_> = installed.contexts.iter().map(|c| c.item.name.clone()).collect();
        for c in context_names {
            println!("{c}");
        }
        Ok(SelectResult::Listed)
    }
}

pub fn select_or_list_namespace(settings: &Settings, namespaces: Option<Vec<String>>) -> Result<SelectResult> {
    if !io::stdout().is_terminal() {
        // Non-interactive: fetch and print.
        let mut namespaces = match namespaces {
            Some(ns) => ns,
            None => kubectl::get_namespaces(None)?,
        };
        namespaces.sort();
        if namespaces.is_empty() {
            bail!("No namespaces found");
        }
        for n in namespaces {
            println!("{n}");
        }
        return Ok(SelectResult::Listed);
    }

    // Interactive: show the picker immediately and load namespaces in the background.
    match namespaces {
        Some(mut ns) => {
            // Already have namespaces (e.g. partial match list) -- show immediately.
            ns.sort();
            if ns.is_empty() {
                bail!("No namespaces found");
            }
            let items: Vec<PickerItem> = ns.iter().map(|n| picker::simple_item(n)).collect();
            match picker::pick(items, None, &settings.picker)? {
                Some(name) => Ok(SelectResult::Selected(name)),
                None => Ok(SelectResult::Cancelled),
            }
        }
        None => {
            // No namespaces yet -- open picker immediately, fetch in background.
            let (tx, rx) = mpsc::channel::<picker::PickerUpdate>();

            thread::spawn(move || {
                if let Ok(mut ns) = kubectl::get_namespaces(None) {
                    ns.sort();
                    let items: Vec<PickerItem> = ns.iter().map(|n| picker::simple_item(n)).collect();
                    let _ = tx.send(picker::PickerUpdate::Items(items));
                }
            });

            match picker::pick(Vec::new(), Some(rx), &settings.picker)? {
                Some(name) => Ok(SelectResult::Selected(name)),
                None => Ok(SelectResult::Cancelled),
            }
        }
    }
}
