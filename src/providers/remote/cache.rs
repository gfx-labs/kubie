use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::providers::ClusterInfo;

/// Current metadata schema version. Bump this when the `ClusterInfo` struct changes.
const METADATA_VERSION: u32 = 2;

/// Versioned metadata envelope. Allows detecting and discarding stale schemas.
#[derive(Serialize, Deserialize)]
struct Metadata {
    version: u32,
    clusters: Vec<ClusterInfo>,
}

/// Persistent storage for metadata (survives reboots).
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Ephemeral storage for kubeconfig files.
static TEMP_DIR: OnceLock<PathBuf> = OnceLock::new();

/// GPG key ID for encrypting cached kubeconfigs. None = no encryption.
static GPG_KEY: OnceLock<Option<String>> = OnceLock::new();

/// Initialize cache paths and encryption settings.
///
/// - Metadata: `$XDG_CACHE_HOME/kubie/providers/` (default `~/.cache/kubie/providers/`)
/// - Configs:  `/tmp/kubie-providers-<uid>/configs/`
pub fn init(gpg_key: Option<String>) {
    DATA_DIR.get_or_init(|| {
        let base = if let Ok(dir) = std::env::var("XDG_CACHE_HOME") {
            PathBuf::from(dir)
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".cache")
        };
        base.join("kubie").join("providers")
    });

    TEMP_DIR.get_or_init(|| {
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/kubie-providers-{uid}"))
    });

    GPG_KEY.get_or_init(|| gpg_key);
}

fn data_dir() -> &'static Path {
    DATA_DIR
        .get()
        .expect("providers::cache::init() must be called before using cache")
}

fn temp_dir() -> &'static Path {
    TEMP_DIR
        .get()
        .expect("providers::cache::init() must be called before using cache")
}

fn gpg_key() -> Option<&'static str> {
    GPG_KEY.get().and_then(|k| k.as_deref())
}

/// Returns the configs directory (ephemeral, in /tmp).
pub fn configs_dir() -> PathBuf {
    temp_dir().join("configs")
}

/// Returns the metadata file path (persistent, in XDG cache).
fn metadata_path() -> PathBuf {
    data_dir().join("metadata.json")
}

/// Ensure the ephemeral configs directory exists with restrictive permissions.
fn ensure_configs_dir() -> anyhow::Result<()> {
    let dir = configs_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
        fs::set_permissions(temp_dir(), fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Ensure the persistent data directory exists.
fn ensure_data_dir() -> anyhow::Result<()> {
    let dir = data_dir();
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// Save metadata (cluster list) to persistent storage with a version tag.
pub fn save_metadata(clusters: &[ClusterInfo]) -> anyhow::Result<()> {
    ensure_data_dir()?;
    let path = metadata_path();
    let metadata = Metadata {
        version: METADATA_VERSION,
        clusters: clusters.to_vec(),
    };
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(&path, json)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Load cached metadata. Returns `None` if no cache exists or if the
/// schema version doesn't match (stale metadata is discarded automatically).
pub fn load_metadata() -> anyhow::Result<Option<Vec<ClusterInfo>>> {
    let path = metadata_path();
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)?;
    let Ok(metadata) = serde_json::from_str::<Metadata>(&data) else {
        let _ = fs::remove_file(&path);
        return Ok(None);
    };
    if metadata.version != METADATA_VERSION {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(metadata.clusters))
}

/// Generate a kubeconfig file name from provider, account, and cluster name.
pub fn config_filename(cluster: &ClusterInfo) -> String {
    let sanitize = |s: &str| -> String {
        s.replace(['@', '.', '/', ':'], "_")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect()
    };
    let safe_provider = sanitize(&cluster.provider);
    let safe_account = sanitize(&cluster.account);
    let safe_cluster = sanitize(&cluster.name);

    if gpg_key().is_some() {
        format!("{safe_provider}_{safe_account}_{safe_cluster}.yaml.gpg")
    } else {
        format!("{safe_provider}_{safe_account}_{safe_cluster}.yaml")
    }
}

/// Write a kubeconfig to the ephemeral cache, optionally GPG-encrypted.
pub fn write_config(cluster: &ClusterInfo, content: &str) -> anyhow::Result<PathBuf> {
    ensure_configs_dir()?;
    let filename = config_filename(cluster);
    let path = configs_dir().join(&filename);

    if let Some(key) = gpg_key() {
        gpg_encrypt(content, key, &path)?;
    } else {
        fs::write(&path, content)?;
    }

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

/// Read a cached kubeconfig, decrypting if necessary.
/// Returns the plaintext kubeconfig content.
pub fn read_config(cluster: &ClusterInfo) -> anyhow::Result<Option<String>> {
    let filename = config_filename(cluster);
    let path = configs_dir().join(&filename);

    if !path.exists() {
        return Ok(None);
    }

    if gpg_key().is_some() {
        let content = gpg_decrypt(&path)?;
        Ok(Some(content))
    } else {
        let content = fs::read_to_string(&path)?;
        Ok(Some(content))
    }
}

/// Write the (decrypted) kubeconfig for a cluster to a temporary file.
/// Returns the path to the temp file. Caller is responsible for cleanup.
pub fn decrypted_config_path(cluster: &ClusterInfo) -> anyhow::Result<Option<PathBuf>> {
    let content = match read_config(cluster)? {
        Some(c) => c,
        None => return Ok(None),
    };

    let tmp = tempfile::Builder::new()
        .prefix("kubie-provider-")
        .suffix(".yaml")
        .tempfile()?;
    let (_, path) = tmp.keep()?;
    fs::write(&path, &content)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(Some(path))
}

/// Find a cluster by its context name.
pub fn find_cluster_for_context(context_name: &str, clusters: &[ClusterInfo]) -> Option<ClusterInfo> {
    clusters.iter().find(|c| c.context_name == context_name).cloned()
}

// ---------------------------------------------------------------------------
// GPG helpers
// ---------------------------------------------------------------------------

/// Encrypt content with GPG and write to the given path.
fn gpg_encrypt(content: &str, recipient: &str, path: &Path) -> anyhow::Result<()> {
    let mut child = Command::new("gpg")
        .args([
            "--batch",
            "--yes",
            "--quiet",
            "--encrypt",
            "--recipient",
            recipient,
            "--output",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to run gpg for encryption. Is gpg installed?")?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(content.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gpg encryption failed: {}", stderr.trim());
    }

    Ok(())
}

/// Decrypt a GPG-encrypted file and return the plaintext content.
/// The gpg-agent handles passphrase caching (user enters it once per session).
fn gpg_decrypt(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("gpg")
        .args(["--batch", "--quiet", "--decrypt"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to run gpg for decryption. Is gpg installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gpg decryption failed: {}", stderr.trim());
    }

    String::from_utf8(output.stdout).context("Decrypted kubeconfig is not valid UTF-8")
}
