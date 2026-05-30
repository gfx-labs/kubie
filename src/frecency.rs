//! Lightweight frecency (frequency + recency) tracking for picker selections.
//!
//! Stores per-item selection counts and timestamps in a JSON file at
//! `~/.local/share/kubie/frecency.json`. Used to sort picker items so
//! frequently and recently used contexts float to the top.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A single entry tracking how often and how recently an item was selected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Total number of times this item has been selected.
    pub count: u64,
    /// Unix timestamp (seconds) of the last selection.
    pub last_used: u64,
}

/// The full frecency database.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FrecencyDb {
    pub entries: HashMap<String, Entry>,
}

fn db_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(dir).join("kubie/frecency.json")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/kubie/frecency.json")
    } else {
        PathBuf::from("/tmp/kubie-frecency.json")
    }
}

impl FrecencyDb {
    /// Load the database from disk. Returns an empty database if the file
    /// doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let path = db_path();
        if !path.exists() {
            return Self::default();
        }
        fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default()
    }

    /// Save the database to disk. Errors are silently ignored (non-critical).
    pub fn save(&self) {
        let path = db_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(self) {
            let _ = fs::write(&path, json);
        }
    }

    /// Record a selection of the given item.
    pub fn record(&mut self, key: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let entry = self
            .entries
            .entry(key.to_string())
            .or_insert(Entry { count: 0, last_used: 0 });
        entry.count += 1;
        entry.last_used = now;
    }

    /// Compute a frecency score for the given key. Higher is better.
    ///
    /// The score combines:
    /// - Frequency: log2(count + 1) to dampen very high counts
    /// - Recency: exponential decay based on hours since last use (half-life ~24h)
    ///
    /// Items not in the database score 0.
    pub fn score(&self, key: &str) -> f64 {
        let Some(entry) = self.entries.get(key) else {
            return 0.0;
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let hours_ago = (now.saturating_sub(entry.last_used)) as f64 / 3600.0;
        let frequency = (entry.count as f64 + 1.0).log2();
        let recency = (-hours_ago / 24.0).exp(); // half-life ~24h

        frequency * 2.0 + recency * 5.0
    }
}
