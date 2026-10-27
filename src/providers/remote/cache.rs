use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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

/// Previous kubeconfig cache location, retained only for cleanup.
static LEGACY_TEMP_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Initialize cache paths. Must be called once at startup when remote features are active.
///
/// - Metadata: `$XDG_CACHE_HOME/kubie/providers/` (default `~/.cache/kubie/providers/`)
/// - Configs:  `$XDG_CACHE_HOME/kubie/providers/configs/`
pub fn init() -> Result<()> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            dirs::home_dir()
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".cache"))
        })
        .context("Could not locate a user cache directory; set XDG_CACHE_HOME to an absolute path")?;
    DATA_DIR.get_or_init(|| base.join("kubie").join("providers"));

    LEGACY_TEMP_DIR.get_or_init(|| {
        // SAFETY: getuid is always safe to call.
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/kubie-providers-{uid}"))
    });
    Ok(())
}

fn data_dir() -> &'static Path {
    DATA_DIR
        .get()
        .expect("providers::cache::init() must be called before using cache")
}

fn legacy_temp_dir() -> &'static Path {
    LEGACY_TEMP_DIR
        .get()
        .expect("providers::cache::init() must be called before using cache")
}

/// Returns the private XDG kubeconfig cache directory.
pub fn configs_dir() -> PathBuf {
    data_dir().join("configs")
}

/// Returns the metadata file path (persistent, in XDG cache).
fn metadata_path() -> PathBuf {
    data_dir().join("metadata.json")
}

/// Clear only provider metadata and downloaded kubeconfigs, not isolated session copies.
pub fn clear() -> Result<()> {
    clear_paths(data_dir(), legacy_temp_dir())
}

fn clear_paths(data: &Path, temp: &Path) -> Result<()> {
    let mut existing = Vec::new();
    for dir in data.parent().into_iter().chain([data, temp]) {
        match fs::symlink_metadata(dir) {
            Ok(metadata) if !metadata.is_dir() || metadata.uid() != unsafe { libc::getuid() } => {
                bail!(
                    "Refusing to clear provider cache through an unowned directory, non-directory or symlink: {}",
                    dir.display()
                );
            }
            Ok(_) => existing.push(dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("Could not inspect cache directory {}", dir.display()))
            }
        }
    }
    for dir in existing {
        secure_existing_dir(dir)?;
    }

    let metadata = data.join("metadata.json");
    ignore_missing(fs::remove_file(&metadata))
        .with_context(|| format!("Could not clear provider metadata {}", metadata.display()))?;

    clear_configs_dir(&data.join("configs"))?;
    clear_configs_dir(&temp.join("configs"))
}

fn clear_configs_dir(configs: &Path) -> Result<()> {
    let result = match fs::symlink_metadata(configs) {
        Ok(metadata) if metadata.is_dir() => {
            secure_existing_dir(configs)?;
            fs::remove_dir_all(configs)
        }
        Ok(_) => fs::remove_file(configs),
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

/// Ensure all application-owned cache directories are private before use.
fn ensure_configs_dir() -> anyhow::Result<()> {
    ensure_data_dir()?;
    ensure_private_dir(&configs_dir())
}

fn ensure_data_dir() -> anyhow::Result<()> {
    ensure_private_dir(data_dir().parent().context("Provider cache has no parent")?)?;
    ensure_private_dir(data_dir())
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("Could not create cache directory {}", path.display()))?;
    secure_existing_dir(path)
}

fn secure_existing_dir(path: &Path) -> Result<()> {
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("Could not safely open cache directory {}", path.display()))?;
    if directory.metadata()?.uid() != unsafe { libc::getuid() } {
        bail!("Refusing unowned cache directory {}", path.display());
    }
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn private_cached_file(path: &Path) -> Result<Option<fs::File>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Could not safely open cache file {}", path.display()))
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::getuid() } {
        bail!("Refusing unowned or non-regular cache file {}", path.display());
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(Some(file))
}

fn write_private_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("Cache file has no parent")?)?;
    file.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(content)?;
    file.persist(path)
        .with_context(|| format!("Could not replace cache file {}", path.display()))?;
    Ok(())
}

/// Validate cached credentials and repair permissions before a freshness check.
pub fn checked_config_path(cluster: &ClusterInfo) -> Result<PathBuf> {
    ensure_configs_dir()?;
    let path = configs_dir().join(config_filename(cluster));
    private_cached_file(&path)?;
    Ok(path)
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
    write_private_file(&path, json.as_bytes())
}

/// Load cached metadata. Returns `None` if no cache exists or if the
/// schema version doesn't match (stale metadata is discarded automatically).
pub fn load_metadata() -> anyhow::Result<Option<Vec<ClusterInfo>>> {
    let path = metadata_path();
    ensure_data_dir()?;
    let Some(mut file) = private_cached_file(&path)? else {
        return Ok(None);
    };
    let mut data = String::new();
    file.read_to_string(&mut data)?;
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

/// Atomically replace a kubeconfig without exposing permissive intermediate files.
pub fn write_config(cluster: &ClusterInfo, content: &str) -> anyhow::Result<PathBuf> {
    ensure_configs_dir()?;
    let filename = config_filename(cluster);
    let path = configs_dir().join(&filename);
    write_private_file(&path, content.as_bytes())?;
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
        let xdg_configs = data.join("configs");
        fs::create_dir_all(&data)?;
        fs::create_dir_all(&configs)?;
        fs::create_dir_all(&xdg_configs)?;
        let session = root.path().join("kubie-config.yaml");
        fs::write(&session, "isolated session")?;
        fs::write(data.join("metadata.json"), "cached metadata")?;
        fs::write(data.join("unrelated"), "preserved")?;
        fs::write(configs.join("cluster.yaml"), "cached kubeconfig")?;
        fs::write(xdg_configs.join("cluster.yaml"), "XDG cached kubeconfig")?;
        symlink(root.path(), configs.join("outside"))?;

        clear_paths(&data, &temp)?;
        clear_paths(&data, &temp)?;
        assert!(!data.join("metadata.json").exists());
        assert!(!configs.exists());
        assert!(!xdg_configs.exists());
        assert_eq!(fs::read_to_string(&session)?, "isolated session");
        assert_eq!(fs::read_to_string(data.join("unrelated"))?, "preserved");

        symlink(root.path(), &configs)?;
        symlink(root.path(), &xdg_configs)?;
        symlink(&session, data.join("metadata.json"))?;
        clear_paths(&data, &temp)?;
        assert!(fs::symlink_metadata(&configs).is_err());
        assert!(fs::symlink_metadata(&xdg_configs).is_err());
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

    #[test]
    fn private_cache_storage_rejects_links_and_replaces_files_atomically() -> Result<()> {
        let root = tempfile::tempdir()?;
        let directory = root.path().join("providers/configs");
        ensure_private_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o775))?;
        ensure_private_dir(&directory)?;
        assert_eq!(fs::metadata(&directory)?.permissions().mode() & 0o777, 0o700);

        let outside = root.path().join("outside");
        fs::write(&outside, "unrelated file")?;
        let cached = directory.join("cluster.yaml");
        symlink(&outside, &cached)?;
        assert!(private_cached_file(&cached).is_err());
        write_private_file(&cached, b"cached credentials")?;
        assert_eq!(fs::read_to_string(&outside)?, "unrelated file");
        assert_eq!(fs::read_to_string(&cached)?, "cached credentials");
        assert_eq!(fs::metadata(&cached)?.permissions().mode() & 0o777, 0o600);

        fs::set_permissions(&cached, fs::Permissions::from_mode(0o664))?;
        assert!(private_cached_file(&cached)?.is_some());
        assert_eq!(fs::metadata(&cached)?.permissions().mode() & 0o777, 0o600);
        assert!(private_cached_file(&directory).is_err());
        let linked_directory = root.path().join("linked");
        symlink(&directory, &linked_directory)?;
        assert!(ensure_private_dir(&linked_directory).is_err());
        Ok(())
    }
}
