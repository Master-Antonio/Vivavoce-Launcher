use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use serde::{Deserialize, Serialize};

/// Represents a downloadable compressed archive containing one or more addon modules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChunk {
    /// Unique identifier for this package chunk (e.g. "vivavoce_core", "vivavoce_quests")
    pub id: String,
    /// Archive filename (e.g. "VivaVoce_Quests.tar.zst")
    pub filename: String,
    /// Expected Blake3 checksum hex string of the compressed archive
    pub blake3: String,
    /// Direct URL to download the archive chunk
    pub download_url: String,
    /// Size of the compressed archive in bytes
    pub compressed_size: u64,
    /// Total uncompressed size of all files contained inside
    pub uncompressed_size: u64,
}

/// Represents an individual file inside the addon distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path inside the AddOns directory (normalized with forward slashes)
    pub relative_path: String,
    /// Blake3 checksum hex string of the uncompressed file
    pub blake3_hash: String,
    /// File size in bytes
    pub size: u64,
}

/// The remote manifest published on CDN / GitHub release containing metadata for all packages and files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteManifest {
    /// Semantic version of the voice pack distribution (e.g. "1.0.0")
    pub version: String,
    /// Total number of individual files across all packages
    pub total_files: usize,
    /// Total uncompressed size in bytes across all files
    pub total_bytes: u64,
    /// List of package chunks (.tar.zst archives) available for download
    pub packages: Vec<PackageChunk>,
    /// Map of normalized relative paths to FileEntry
    pub files: HashMap<String, FileEntry>,
}

impl RemoteManifest {
    /// Deserializes a `RemoteManifest` from a JSON string.
    pub fn from_json_str(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Serializes the `RemoteManifest` to a compact JSON string.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serializes the `RemoteManifest` to a pretty-printed JSON string.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Deserializes a `RemoteManifest` from any reader implementing `std::io::Read`.
    pub fn from_reader<R: Read>(reader: R) -> Result<Self, serde_json::Error> {
        serde_json::from_reader(reader)
    }

    /// Serializes the `RemoteManifest` to any writer implementing `std::io::Write`.
    pub fn to_writer<W: Write>(&self, writer: W) -> Result<(), serde_json::Error> {
        serde_json::to_writer_pretty(writer, self)
    }

    /// Recalculates `total_files` and `total_bytes` based on the internal `files` map.
    pub fn recalculate_totals(&mut self) {
        self.total_files = self.files.len();
        self.total_bytes = self.files.values().map(|f| f.size).sum();
    }

    /// Looks up a package chunk by its id.
    pub fn get_package_by_id(&self, id: &str) -> Option<&PackageChunk> {
        self.packages.iter().find(|p| p.id == id)
    }
}

/// The local launcher state stored on disk, tracking what has been installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LocalState {
    /// Currently installed manifest version, if any
    pub version: Option<String>,
    /// List of detected or installed VivaVoce modules
    pub installed_modules: Vec<String>,
    /// Map of verified local files
    pub files: HashMap<String, FileEntry>,
    /// Last verification/update ISO 8601 timestamp
    pub last_updated: Option<String>,
}

impl LocalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads `LocalState` from a JSON file.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, anyhow::Error> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let state = serde_json::from_reader(reader)?;
        Ok(state)
    }

    /// Saves `LocalState` atomically to a JSON file.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), anyhow::Error> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
        {
            let file = File::create(&tmp_path)?;
            let writer = BufWriter::new(file);
            serde_json::to_writer_pretty(writer, self)?;
        }

        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

/// Normalizes a relative path to use forward slashes `/`, ensuring cross-platform consistency.
pub fn normalize_relative_path<P: AsRef<Path>>(path: P) -> String {
    let p = path.as_ref();
    let components: Vec<&str> = p
        .iter()
        .filter_map(|c| c.to_str())
        .filter(|&c| c != "." && !c.is_empty())
        .collect();
    components.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let mut manifest = RemoteManifest {
            version: "1.0.0".to_string(),
            total_files: 0,
            total_bytes: 0,
            packages: vec![PackageChunk {
                id: "vivavoce_quests".to_string(),
                filename: "VivaVoce_Quests.tar.zst".to_string(),
                blake3: "abcdef0123456789".to_string(),
                download_url: "https://example.com/quests.tar.zst".to_string(),
                compressed_size: 1048576,
                uncompressed_size: 2097152,
            }],
            files: HashMap::new(),
        };

        manifest.files.insert(
            "VivaVoce_Quests/Audio/test.mp3".to_string(),
            FileEntry {
                relative_path: "VivaVoce_Quests/Audio/test.mp3".to_string(),
                blake3_hash: "1234567890abcdef".to_string(),
                size: 2048,
            },
        );
        manifest.recalculate_totals();

        assert_eq!(manifest.total_files, 1);
        assert_eq!(manifest.total_bytes, 2048);

        let json = manifest.to_json_pretty().expect("Failed to serialize");
        let deserialized = RemoteManifest::from_json_str(&json).expect("Failed to deserialize");
        assert_eq!(manifest, deserialized);
    }

    #[test]
    fn test_local_state_save_and_load() {
        let temp_dir = tempfile::tempdir().expect("Failed to create tempdir");
        let state_path = temp_dir.path().join("local_state.json");

        let mut state = LocalState::new();
        state.version = Some("1.0.0".to_string());
        state.installed_modules = vec!["VivaVoce".to_string(), "VivaVoce_Quests".to_string()];
        state.files.insert(
            "VivaVoce/VivaVoce.toc".to_string(),
            FileEntry {
                relative_path: "VivaVoce/VivaVoce.toc".to_string(),
                blake3_hash: "deadbeef".to_string(),
                size: 512,
            },
        );

        state.save_to_file(&state_path).expect("Failed to save state");
        let loaded = LocalState::load_from_file(&state_path).expect("Failed to load state");

        assert_eq!(state, loaded);
    }

    #[test]
    fn test_normalize_relative_path() {
        let p1 = PathBuf::from("VivaVoce_Quests\\Audio\\sub\\file.mp3");
        assert_eq!(normalize_relative_path(p1), "VivaVoce_Quests/Audio/sub/file.mp3");

        let p2 = PathBuf::from("VivaVoce/toc.txt");
        assert_eq!(normalize_relative_path(p2), "VivaVoce/toc.txt");
    }
}
