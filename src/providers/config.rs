use std::collections::HashMap;
use std::fmt;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde::Deserialize;

use super::kubeconfig::{KubeConfigProvider, KubeConfigProviderConfig};
#[cfg(feature = "remote")]
use super::remote::aks::{Aks, AksConfig};
#[cfg(feature = "remote")]
use super::remote::digitalocean::{DigitalOcean, DigitalOceanConfig};
#[cfg(feature = "remote")]
use super::remote::eks::{Eks, EksConfig};
#[cfg(feature = "remote")]
use super::remote::gke::{Gke, GkeConfig};
#[cfg(feature = "remote")]
use super::remote::rancher::{Rancher, RancherConfig};
use super::NamedProvider;

// ---------------------------------------------------------------------------
// Secret: a lazily-expanded string type
// ---------------------------------------------------------------------------

/// A string value that supports environment variable and command substitution.
///
/// Expansion is **lazy**: the raw string is stored at deserialize time, and
/// `$(...)` commands / `${VAR}` expansions are only evaluated on the first
/// call to `.value()`. This means `Settings::load()` is instant even when
/// providers use slow commands like `$(pass show ...)` or `$(gcloud auth ...)`.
///
/// Use this type on any provider config field that may contain secrets or
/// dynamic values. Plain `String` fields are left as-is.
///
/// Supported syntax:
/// - `${VAR}` -- expands to the value of env var `VAR` (left as-is if unset)
/// - `${VAR:-default}` -- expands to `VAR` if set and non-empty, otherwise `default`
/// - `$VAR` -- bare env var expansion
/// - `$(command args...)` -- runs command, substitutes trimmed stdout
///
/// Expansions can be combined: `Bearer $(vault read -field=token secret/k8s)`
pub struct Secret {
    raw: String,
    resolved: OnceLock<String>,
}

impl Secret {
    /// Get the expanded value. The first call triggers expansion (which may
    /// run shell commands); subsequent calls return the cached result.
    pub fn value(&self) -> &str {
        self.resolved.get_or_init(|| expand(&self.raw))
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.value().is_empty()
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Secret {
            raw: self.raw.clone(),
            resolved: match self.resolved.get() {
                Some(v) => {
                    let lock = OnceLock::new();
                    let _ = lock.set(v.clone());
                    lock
                }
                None => OnceLock::new(),
            },
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.value())
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Secret {
            raw,
            resolved: OnceLock::new(),
        })
    }
}

impl Default for Secret {
    fn default() -> Self {
        Secret {
            raw: String::new(),
            resolved: OnceLock::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Expansion engine
// ---------------------------------------------------------------------------

/// Expand `$(cmd)`, `${VAR}`, `${VAR:-default}`, and `$VAR` in a string.
fn expand(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'$' && i + 1 < len {
            // $(command ...) -- command substitution
            if bytes[i + 1] == b'(' {
                if let Some(close) = find_matching_paren(input, i + 1) {
                    let cmd_str = &input[i + 2..close];
                    match run_command(cmd_str) {
                        Ok(output) => result.push_str(&output),
                        Err(e) => {
                            eprintln!("Warning: command substitution failed for $({cmd_str}): {e}");
                            // Leave unexpanded so the user sees what failed.
                            result.push_str(&input[i..=close]);
                        }
                    }
                    i = close + 1;
                    continue;
                }
            }

            // ${VAR} or ${VAR:-default}
            if bytes[i + 1] == b'{' {
                if let Some(close) = input[i + 2..].find('}') {
                    let inner = &input[i + 2..i + 2 + close];
                    if let Some(sep) = inner.find(":-") {
                        let var_name = &inner[..sep];
                        let default_val = &inner[sep + 2..];
                        match std::env::var(var_name) {
                            Ok(val) if !val.is_empty() => result.push_str(&val),
                            _ => result.push_str(default_val),
                        }
                    } else {
                        match std::env::var(inner) {
                            Ok(val) => result.push_str(&val),
                            Err(_) => {
                                result.push_str(&input[i..=(i + 2 + close)]);
                            }
                        }
                    }
                    i += 2 + close + 1;
                    continue;
                }
            }

            // $VAR (bare)
            if bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_' {
                let start = i + 1;
                let mut end = start;
                while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let var_name = &input[start..end];
                match std::env::var(var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => result.push_str(&input[i..end]),
                }
                i = end;
                continue;
            }
        }

        result.push(input[i..].chars().next().unwrap());
        i += input[i..].chars().next().unwrap().len_utf8();
    }

    result
}

/// Find the closing `)` matching the `(` at position `open`, handling nesting
/// and respecting single/double quotes and backslash escapes.
fn find_matching_paren(input: &str, open: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                // Skip escaped character.
                i += 2;
                continue;
            }
            b'\'' => {
                // Skip single-quoted string (no escapes inside single quotes).
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
            }
            b'"' => {
                // Skip double-quoted string (backslash escapes allowed).
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1; // skip escaped char
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Run a shell command and return its trimmed stdout.
fn run_command(cmd_str: &str) -> anyhow::Result<String> {
    let cmd_str = cmd_str.trim();
    if cmd_str.is_empty() {
        anyhow::bail!("empty command");
    }

    let output = Command::new("sh")
        .args(["-c", cmd_str])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("exit {}: {}", output.status, stderr.trim());
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Provider configuration (lives under `providers:` in kubie.yaml).
///
/// Each entry is a named provider instance that discovers clusters
/// and fetches kubeconfigs on demand.
#[derive(Debug, Default, Deserialize)]
pub struct ProvidersConfig {
    /// Named provider instances. The key is a user-chosen label (e.g.
    /// "my-do-account", "staging-rancher") that shows up in the picker
    /// as the account name.
    #[serde(flatten)]
    pub entries: HashMap<String, ProviderEntry>,
}

/// A single provider entry with a type discriminator and provider-specific config.
#[derive(Debug, Deserialize)]
pub struct ProviderEntry {
    /// Provider type: "digitalocean", "rancher", etc.
    #[serde(rename = "type")]
    pub provider_type: String,

    /// Whether this provider is enabled (defaults to true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Provider-specific configuration (deserialized based on `provider_type`).
    #[serde(default)]
    pub config: serde_yaml::Value,
}

fn default_true() -> bool {
    true
}

#[allow(dead_code)]
const SAMPLE_PROVIDERS_CONFIG: &str = r#"# Provider configuration for kubie.
# Add this section to your ~/.kube/kubie.yaml file.
#
# Fields of type Secret support expansion:
#   ${VAR}             - environment variable
#   ${VAR:-default}    - environment variable with default
#   $(command args...) - command substitution (stdout, trimmed)
#
# providers:
#   digitalocean:
#     type: digitalocean
#     config:
#       token: $(doctl auth token)
#
#   my-rancher:
#     type: rancher
#     config:
#       url: https://rancher.example.com
#       token: $(vault kv get -field=token secret/rancher)
"#;

// ---------------------------------------------------------------------------
// Provider construction
// ---------------------------------------------------------------------------

/// Build the list of active providers from config.
///
/// If `kubie_config_path` is provided, it will be added to the exclude list
/// of any kubeconfig providers (to prevent kubie from loading its own config).
pub fn build_providers(config: &ProvidersConfig, kubie_config_path: Option<&str>) -> Vec<NamedProvider> {
    let mut providers: Vec<NamedProvider> = Vec::new();

    for (name, entry) in &config.entries {
        if !entry.enabled {
            continue;
        }

        match entry.provider_type.as_str() {
            "kubeconfig" => {
                let cfg: KubeConfigProviderConfig = serde_yaml::from_value(entry.config.clone()).unwrap_or_default();
                let mut provider = KubeConfigProvider::new(cfg);
                if let Some(path) = kubie_config_path {
                    provider.exclude(path.to_string());
                }
                providers.push((name.clone(), Box::new(provider)));
            }
            #[cfg(feature = "remote")]
            "digitalocean" => {
                let cfg: DigitalOceanConfig = serde_yaml::from_value(entry.config.clone()).unwrap_or_default();
                providers.push((name.clone(), Box::new(DigitalOcean::new(cfg))));
            }
            #[cfg(feature = "remote")]
            "gke" => match serde_yaml::from_value::<GkeConfig>(entry.config.clone()) {
                Ok(cfg) => {
                    providers.push((name.clone(), Box::new(Gke::new(cfg))));
                }
                Err(e) => {
                    eprintln!("Warning: failed to parse gke config for '{name}': {e}");
                }
            },
            #[cfg(feature = "remote")]
            "rancher" => match serde_yaml::from_value::<RancherConfig>(entry.config.clone()) {
                Ok(cfg) => {
                    providers.push((name.clone(), Box::new(Rancher::new(cfg))));
                }
                Err(e) => {
                    eprintln!("Warning: failed to parse rancher config for '{name}': {e}");
                }
            },
            #[cfg(feature = "remote")]
            "eks" => match serde_yaml::from_value::<EksConfig>(entry.config.clone()) {
                Ok(cfg) => {
                    providers.push((name.clone(), Box::new(Eks::new(cfg))));
                }
                Err(e) => {
                    eprintln!("Warning: failed to parse eks config for '{name}': {e}");
                }
            },
            #[cfg(feature = "remote")]
            "aks" => match serde_yaml::from_value::<AksConfig>(entry.config.clone()) {
                Ok(cfg) => {
                    providers.push((name.clone(), Box::new(Aks::new(cfg))));
                }
                Err(e) => {
                    eprintln!("Warning: failed to parse aks config for '{name}': {e}");
                }
            },
            other => {
                eprintln!("Warning: unknown provider type '{other}' for '{name}'");
            }
        }
    }

    providers
}

/// Return the sample providers config text for documentation purposes.
#[allow(dead_code)]
pub fn sample_config() -> &'static str {
    SAMPLE_PROVIDERS_CONFIG
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // SAFETY: Tests use unique env var names to avoid collisions.

    unsafe fn set_var(key: &str, val: &str) {
        unsafe { std::env::set_var(key, val) };
    }

    unsafe fn remove_var(key: &str) {
        unsafe { std::env::remove_var(key) };
    }

    #[test]
    fn expand_env_braced() {
        unsafe { set_var("KUBIE_TEST_VAR", "hello") };
        assert_eq!(expand("prefix-${KUBIE_TEST_VAR}-suffix"), "prefix-hello-suffix");
        unsafe { remove_var("KUBIE_TEST_VAR") };
    }

    #[test]
    fn expand_env_bare() {
        unsafe { set_var("KUBIE_TEST_BARE", "world") };
        assert_eq!(expand("$KUBIE_TEST_BARE!"), "world!");
        unsafe { remove_var("KUBIE_TEST_BARE") };
    }

    #[test]
    fn expand_env_default_used() {
        unsafe { remove_var("KUBIE_TEST_UNSET") };
        assert_eq!(expand("${KUBIE_TEST_UNSET:-fallback}"), "fallback");
    }

    #[test]
    fn expand_env_default_not_used() {
        unsafe { set_var("KUBIE_TEST_SET", "actual") };
        assert_eq!(expand("${KUBIE_TEST_SET:-fallback}"), "actual");
        unsafe { remove_var("KUBIE_TEST_SET") };
    }

    #[test]
    fn expand_env_unset_preserved() {
        unsafe { remove_var("KUBIE_TEST_MISSING") };
        assert_eq!(expand("${KUBIE_TEST_MISSING}"), "${KUBIE_TEST_MISSING}");
    }

    #[test]
    fn expand_env_empty_default() {
        unsafe { remove_var("KUBIE_TEST_EMPTY") };
        assert_eq!(expand("pre${KUBIE_TEST_EMPTY:-}post"), "prepost");
    }

    #[test]
    fn expand_command_substitution() {
        assert_eq!(expand("token=$(echo hello)"), "token=hello");
    }

    #[test]
    fn expand_command_with_env() {
        unsafe { set_var("KUBIE_TEST_PREFIX", "bearer") };
        assert_eq!(expand("${KUBIE_TEST_PREFIX} $(echo secret)"), "bearer secret");
        unsafe { remove_var("KUBIE_TEST_PREFIX") };
    }

    #[test]
    fn expand_nested_parens() {
        // $(echo $(echo x)) -- the inner parens are part of the command
        assert_eq!(expand("$(echo $(echo nested))"), "nested");
    }

    #[test]
    fn expand_no_expansion() {
        assert_eq!(expand("plain string"), "plain string");
    }

    #[test]
    fn expand_command_with_parens_in_double_quotes() {
        assert_eq!(expand(r#"$(echo "hello)")"#), "hello)");
    }

    #[test]
    fn expand_command_with_parens_in_single_quotes() {
        assert_eq!(expand("$(echo 'hello)')"), "hello)");
    }

    #[test]
    fn expand_command_with_escaped_paren() {
        assert_eq!(expand(r"$(echo 'test')"), "test");
    }
}
