use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
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

/// Initialize cache paths. Must be called once at startup when cloud feature is active.
///
/// - Metadata: `$XDG_CACHE_HOME/kubie/providers/` (default `~/.cache/kubie/providers/`)
/// - Configs:  `/tmp/kubie-providers-<uid>/configs/`
pub fn init() {
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
        // SAFETY: getuid is always safe to call.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/kubie-providers-{uid}"))
    });
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

/// Returns the configs directory (ephemeral, in /tmp).
pub fn configs_dir() -> PathBuf {
    temp_dir().join("configs")
}

/// Returns the metadata file path (persistent, in XDG cache).
fn metadata_path() -> PathBuf {
    data_dir().join("metadata.json")
}

/// Clear only provider metadata and downloaded kubeconfigs, not isolated session copies.
pub fn clear() -> Result<()> {
    clear_paths(data_dir(), temp_dir())
}

fn clear_paths(data: &Path, temp: &Path) -> Result<()> {
    for dir in data.parent().into_iter().chain([data, temp]) {
        match fs::symlink_metadata(dir) {
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "Refusing to clear provider cache through a non-directory or symlink: {}",
                    dir.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("Could not inspect cache directory {}", dir.display()))
            }
        }
    }

    let metadata = data.join("metadata.json");
    ignore_missing(fs::remove_file(&metadata))
        .with_context(|| format!("Could not clear provider metadata {}", metadata.display()))?;

    let configs = temp.join("configs");
    let result = match fs::symlink_metadata(&configs) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&configs),
        Ok(_) => fs::remove_file(&configs),
        Err(error) => Err(error),
    };
    ignore_missing(result).with_context(|| format!("Could not clear provider kubeconfigs {}", configs.display()))
}

fn ignore_missing(result: std::io::Result<()>) -> std::io::Result<()> {
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
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
        // Unparseable or old format -- discard.
        let _ = fs::remove_file(&path);
        return Ok(None);
    };
    if metadata.version != METADATA_VERSION {
        // Schema version mismatch -- discard.
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
    use std::hash::{Hash, Hasher};
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    cluster.provider.hash(&mut identity);
    cluster.account.hash(&mut identity);
    cluster.id.hash(&mut identity);
    format!(
        "{safe_provider}_{safe_account}_{safe_cluster}_{:016x}.yaml",
        identity.finish()
    )
}

/// Write a kubeconfig to the ephemeral cache.
pub fn write_config(cluster: &ClusterInfo, content: &str) -> anyhow::Result<PathBuf> {
    ensure_configs_dir()?;
    let filename = config_filename(cluster);
    let path = configs_dir().join(&filename);
    fs::write(&path, content)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

/// Find a cluster by its context name.
pub fn find_cluster_for_context(context_name: &str, clusters: &[ClusterInfo]) -> Option<ClusterInfo> {
    clusters.iter().find(|c| c.context_name == context_name).cloned()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn clearing_cache_preserves_session_files_and_symlink_targets() -> Result<()> {
        let root = tempfile::tempdir()?;
        let data = root.path().join("providers");
        let temp = root.path().join("provider-temp");
        let configs = temp.join("configs");
        fs::create_dir_all(&data)?;
        fs::create_dir_all(&configs)?;
        let session = root.path().join("kubie-config.yaml");
        fs::write(&session, "isolated session")?;
        fs::write(data.join("metadata.json"), "cached metadata")?;
        fs::write(data.join("unrelated"), "preserved")?;
        fs::write(configs.join("cluster.yaml"), "cached kubeconfig")?;
        symlink(root.path(), configs.join("outside"))?;

        clear_paths(&data, &temp)?;
        clear_paths(&data, &temp)?;
        assert!(!data.join("metadata.json").exists());
        assert!(!configs.exists());
        assert_eq!(fs::read_to_string(&session)?, "isolated session");
        assert_eq!(fs::read_to_string(data.join("unrelated"))?, "preserved");

        symlink(root.path(), &configs)?;
        symlink(&session, data.join("metadata.json"))?;
        clear_paths(&data, &temp)?;
        assert!(fs::symlink_metadata(&configs).is_err());
        assert_eq!(fs::read_to_string(&session)?, "isolated session");

        let malformed = data.join("metadata.json");
        fs::create_dir(&malformed)?;
        fs::write(malformed.join("preserve"), "not a metadata file")?;
        assert!(clear_paths(&data, &temp).is_err());
        assert_eq!(fs::read_to_string(malformed.join("preserve"))?, "not a metadata file");
        fs::remove_file(malformed.join("preserve"))?;
        fs::remove_dir(&malformed)?;

        let linked_parent = root.path().join("linked-parent");
        symlink(root.path(), &linked_parent)?;
        fs::write(data.join("metadata.json"), "preserve on refusal")?;
        assert!(clear_paths(&linked_parent.join("providers"), &temp).is_err());
        assert_eq!(fs::read_to_string(data.join("metadata.json"))?, "preserve on refusal");

        fs::remove_dir(&temp)?;
        symlink(root.path(), &temp)?;
        fs::write(data.join("metadata.json"), "preserve on refusal")?;
        assert!(clear_paths(&data, &temp).is_err());
        assert_eq!(fs::read_to_string(data.join("metadata.json"))?, "preserve on refusal");
        Ok(())
    }
}
