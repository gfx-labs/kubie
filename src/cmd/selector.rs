//! Provider-qualified cluster selector parsing and matching.

use anyhow::{bail, Result};
use std::fmt;
use wildmatch::WildMatch;

use crate::providers::ClusterInfo;

const PROVIDER_KINDS: &[&str] = &["kubeconfig", "digitalocean", "gke", "rancher", "eks", "aks", "linode"];

/// Whether a selector begins with a recognized provider kind.
///
/// Unknown slash-containing strings remain ordinary local context patterns.
pub fn is_qualified(pattern: &str) -> bool {
    pattern.split_whitespace().any(|part| {
        part.split_once('/')
            .is_some_and(|(provider, _)| PROVIDER_KINDS.contains(&provider))
    })
}

/// Produce a name that can identify a provider cluster without colliding with
/// another source's context of the same name.
pub fn canonical_name(cluster: &ClusterInfo) -> String {
    format!("{}/{}/{}", cluster.provider, cluster.account, cluster.context_name)
}

/// Return an ID-specific selector that cannot be confused with a cluster or
/// context name, including names containing slashes.
pub fn id_alias(cluster: &ClusterInfo) -> String {
    let encoded_id: String = cluster.id.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}/{}/@id:{encoded_id}", cluster.provider, cluster.account)
}

#[derive(Debug)]
struct Selector<'a> {
    provider: &'a str,
    account: Option<&'a str>,
    name: &'a str,
}

fn parse(part: &str) -> Option<Selector<'_>> {
    let mut components = part.split('/');
    let provider = components.next()?;
    if !PROVIDER_KINDS.contains(&provider) {
        return None;
    }
    let rest: Vec<_> = components.collect();
    match rest.as_slice() {
        [name] if !name.is_empty() => Some(Selector {
            provider,
            account: None,
            name,
        }),
        [account, name] if !account.is_empty() && !name.is_empty() => Some(Selector {
            provider,
            account: Some(account),
            name,
        }),
        _ => None,
    }
}

fn has_provider_prefix(part: &str) -> bool {
    part.split_once('/')
        .is_some_and(|(provider, _)| PROVIDER_KINDS.contains(&provider))
}

fn has_wildcards(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

fn decoded_id(name: &str) -> Result<Option<String>> {
    let Some(encoded) = name.strip_prefix("@id:") else {
        return Ok(None);
    };
    if encoded.is_empty() || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) || encoded.len() % 2 != 0 {
        bail!("Invalid ID selector '{name}': expected a non-empty even-length hexadecimal ID");
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("Invalid ID selector '{name}': expected hexadecimal bytes"))?;
    Ok(Some(String::from_utf8(bytes)?))
}

#[derive(Debug)]
pub(crate) struct IncompleteSelection(String);

impl fmt::Display for IncompleteSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for IncompleteSelection {}

pub(crate) fn is_incomplete_selection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<IncompleteSelection>().is_some()
}

fn matches_selector(selector: &Selector<'_>, cluster: &ClusterInfo) -> bool {
    if cluster.provider != selector.provider || selector.account.is_some_and(|account| cluster.account != account) {
        return false;
    }
    let matcher = WildMatch::new(selector.name);
    matcher.matches(&cluster.name) || matcher.matches(&cluster.context_name)
}

/// Select clusters for a provider-qualified selector.
///
/// Returns `None` when no token is provider-qualified. Exact duplicate names
/// are rejected before callers hydrate any kubeconfig. Wildcards may fan out.
pub fn select(pattern: &str, clusters: &[ClusterInfo], allow_multiple: bool) -> Result<Option<Vec<ClusterInfo>>> {
    let parts: Vec<_> = if allow_multiple {
        pattern.split_whitespace().collect()
    } else {
        vec![pattern]
    };
    if !parts.iter().any(|part| has_provider_prefix(part)) {
        return Ok(None);
    }
    if parts.iter().any(|part| !has_provider_prefix(part)) {
        bail!("Cannot mix provider-qualified selectors with unqualified patterns; pass one selector at a time");
    }

    let selectors: Vec<_> = parts
        .iter()
        .map(|part| {
            parse(part).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid provider selector '{part}'. Expected <provider>/<name> or <provider>/<account>/<name>."
                )
            })
        })
        .collect::<Result<_>>()?;
    let mut selected = Vec::new();
    let mut unmatched = Vec::new();
    for selector in &selectors {
        let matches: Vec<_> = if let Some(id) = decoded_id(selector.name)? {
            clusters
                .iter()
                .filter(|cluster| {
                    cluster.provider == selector.provider
                        && selector.account.is_none_or(|account| cluster.account == account)
                        && cluster.id == id
                })
                .cloned()
                .collect()
        } else {
            clusters
                .iter()
                .filter(|cluster| matches_selector(selector, cluster))
                .cloned()
                .collect()
        };
        if matches.is_empty() {
            unmatched.push(format!("{}/{}", selector.provider, selector.name));
        }
        if matches.len() > 1 && !has_wildcards(selector.name) {
            let alternatives = matches
                .iter()
                .map(|cluster| format!("{} (name: {}, id: {})", id_alias(cluster), cluster.name, cluster.id))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Selector '{}/{}' is ambiguous; qualify the account or use a cluster ID. Matches: {}",
                selector.provider,
                selector.name,
                alternatives
            );
        }
        selected.extend(matches);
    }

    if !selected.is_empty() && !unmatched.is_empty() {
        return Err(IncompleteSelection(format!(
            "Some qualified selectors did not match: {}. No partial contexts were selected.",
            unmatched.join(", ")
        ))
        .into());
    }

    selected.sort_by(|a, b| (&a.provider, &a.account, &a.id).cmp(&(&b.provider, &b.account, &b.id)));
    selected.dedup_by(|a, b| a.provider == b.provider && a.account == b.account && a.id == b.id);
    Ok(Some(selected))
}

#[cfg(test)]
mod tests {
    use super::is_qualified;

    #[test]
    fn recognizes_qualified_slash_syntax_without_claiming_bare_or_unknown_names() {
        for (pattern, expected) in [
            ("rancher", false),
            ("gke", false),
            ("local/production", false),
            ("gke/production", true),
            ("rancher/account-a/production", true),
        ] {
            assert_eq!(is_qualified(pattern), expected, "{pattern}");
        }
    }
}
