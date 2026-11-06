use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::engine::manifest::{normalize_relative_path, FileEntry, RemoteManifest};

/// Whitelist of VivaVoce addon folders managed by the launcher.
/// Files outside of these folders are strictly ignored to protect user addons.
pub const VIVAVOCE_MANAGED_MODULES: &[&str] = &[
    "VivaVoce",
    "VivaVoce_Quests",
    "VivaVoce_Books",
    "VivaVoce_Codex",
    "VivaVoce_Zones",
    "VivaVoce_Collector",
];

/// Result of a parallel delta scan comparing disk state with RemoteManifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeltaScanResult {
    /// Files that are missing locally or have differing sizes/hashes and need to be downloaded.
    pub files_to_download: Vec<FileEntry>,
    /// Obsolete or orphaned files inside VivaVoce directories that do not belong to the manifest.
    pub files_to_delete: Vec<PathBuf>,
    /// Count of local files matching the manifest identically.
    pub files_matched: usize,
    /// Total bytes that must be downloaded.
    pub bytes_missing: u64,
}

/// Computes the Blake3 hash of a file using memory-mapping when possible,
/// with fallback to buffered streaming for zero-copy maximum throughput.
pub fn hash_file_blake3<P: AsRef<Path>>(path: P) -> std::io::Result<String> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path)?;
    if metadata.len() == 0 {
        return Ok(blake3::hash(&[]).to_hex().to_string());
    }

    let mut hasher = blake3::Hasher::new();
    // Memory-map file for instant OS zero-copy paging
    match hasher.update_mmap(path) {
        Ok(_) => Ok(hasher.finalize().to_hex().to_string()),
        Err(_) => {
            // Robust fallback if mmap is unavailable (e.g. permissions or special mounts)
            let file = File::open(path)?;
            let mut reader = BufReader::with_capacity(64 * 1024, file);
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(hasher.finalize().to_hex().to_string())
        }
    }
}

enum FileCheckOutcome<'a> {
    Matched,
    DownloadNeeded(&'a FileEntry),
}

/// Executes an ultra-fast, multi-threaded delta scan between the given `RemoteManifest`
/// and the user's `Interface/AddOns` directory.
///
/// Performance optimizations:
/// 1. Fast metadata check: if local size != manifest size, file needs download without hashing.
/// 2. Rayon parallel iteration across all candidate files for Blake3 hashing.
/// 3. Scans only VivaVoce managed directories to discover orphaned files, preserving 3rd-party addons.
pub fn scan_delta<P: AsRef<Path>>(
    addons_dir: P,
    manifest: &RemoteManifest,
) -> std::io::Result<DeltaScanResult> {
    let addons_dir = addons_dir.as_ref();

    // 1. Parallel verification of manifest files against disk
    let entries: Vec<&FileEntry> = manifest.files.values().collect();

    let check_results: Vec<FileCheckOutcome> = entries
        .into_par_iter()
        .map(|entry| {
            let file_path = addons_dir.join(&entry.relative_path);

            // Fast check: existence and file size
            match std::fs::metadata(&file_path) {
                Ok(metadata) => {
                    if metadata.len() != entry.size {
                        // Size mismatch: definitely corrupted or outdated, skip hash
                        FileCheckOutcome::DownloadNeeded(entry)
                    } else {
                        // Size matches: compute Blake3 hash
                        match hash_file_blake3(&file_path) {
                            Ok(local_hash) => {
                                if local_hash.eq_ignore_ascii_case(&entry.blake3_hash) {
                                    FileCheckOutcome::Matched
                                } else {
                                    FileCheckOutcome::DownloadNeeded(entry)
                                }
                            }
                            Err(_) => FileCheckOutcome::DownloadNeeded(entry),
                        }
                    }
                }
                Err(_) => {
                    // File does not exist on disk
                    FileCheckOutcome::DownloadNeeded(entry)
                }
            }
        })
        .collect();

    let mut files_to_download = Vec::new();
    let mut files_matched = 0;
    let mut bytes_missing = 0u64;

    for outcome in check_results {
        match outcome {
            FileCheckOutcome::Matched => {
                files_matched += 1;
            }
            FileCheckOutcome::DownloadNeeded(entry) => {
                bytes_missing += entry.size;
                files_to_download.push(entry.clone());
            }
        }
    }

    // 2. Discover orphaned files in VivaVoce managed directories
    // Build quick lookup set for manifest relative paths
    let manifest_paths: HashSet<&str> = manifest.files.keys().map(|s| s.as_str()).collect();

    // Determine which top-level addon directories to scan
    let mut managed_dirs = HashSet::new();
    for module in VIVAVOCE_MANAGED_MODULES {
        managed_dirs.insert(module.to_string());
    }
    for rel_path in manifest.files.keys() {
        if let Some(first_segment) = rel_path.split('/').next() {
            if !first_segment.is_empty() {
                managed_dirs.insert(first_segment.to_string());
            }
        }
    }

    let mut files_to_delete = Vec::new();

    for dir_name in managed_dirs {
        let module_root = addons_dir.join(&dir_name);
        if !module_root.is_dir() {
            continue;
        }

        for entry in WalkDir::new(&module_root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.into_path();
            if let Ok(rel) = path.strip_prefix(addons_dir) {
                let normalized = normalize_relative_path(rel);
                if !manifest_paths.contains(normalized.as_str()) {
                    files_to_delete.push(path);
                }
            }
        }
    }

    Ok(DeltaScanResult {
        files_to_download,
        files_to_delete,
        files_matched,
        bytes_missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Write;

    #[test]
    fn test_hash_file_blake3_empty_and_content() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let empty_file = temp_dir.path().join("empty.txt");
        File::create(&empty_file).unwrap();

        let empty_hash = hash_file_blake3(&empty_file).unwrap();
        assert_eq!(empty_hash, blake3::hash(&[]).to_hex().to_string());

        let data_file = temp_dir.path().join("data.txt");
        let content = b"VivaVoce Italian Voiceover for World of Warcraft";
        let mut f = File::create(&data_file).unwrap();
        f.write_all(content).unwrap();
        f.flush().unwrap();

        let data_hash = hash_file_blake3(&data_file).unwrap();
        assert_eq!(data_hash, blake3::hash(content).to_hex().to_string());
    }

    #[test]
    fn test_scan_delta_identifies_missing_matched_and_orphaned() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let addons_dir = temp_dir.path();

        let vv_quests = addons_dir.join("VivaVoce_Quests").join("Audio");
        std::fs::create_dir_all(&vv_quests).unwrap();

        // 1. File that matches perfectly
        let matched_file = vv_quests.join("101.mp3");
        let matched_data = b"audio_content_101";
        std::fs::write(&matched_file, matched_data).unwrap();
        let matched_hash = blake3::hash(matched_data).to_hex().to_string();

        // 2. File that has wrong content (corrupted)
        let corrupt_file = vv_quests.join("102.mp3");
        std::fs::write(&corrupt_file, b"corrupted").unwrap();

        // 3. File on disk that is NOT in manifest (orphaned)
        let orphan_file = vv_quests.join("orphan.mp3");
        std::fs::write(&orphan_file, b"old_garbage").unwrap();

        // 4. File in another addon (e.g. Details) that must NEVER be touched
        let details_dir = addons_dir.join("Details");
        std::fs::create_dir_all(&details_dir).unwrap();
        let details_file = details_dir.join("Details.lua");
        std::fs::write(&details_file, b"user addon data").unwrap();

        let mut manifest = RemoteManifest {
            version: "1.0.0".to_string(),
            total_files: 3,
            total_bytes: 0,
            packages: vec![],
            files: HashMap::new(),
        };

        manifest.files.insert(
            "VivaVoce_Quests/Audio/101.mp3".to_string(),
            FileEntry {
                relative_path: "VivaVoce_Quests/Audio/101.mp3".to_string(),
                blake3_hash: matched_hash,
                size: matched_data.len() as u64,
            },
        );

        manifest.files.insert(
            "VivaVoce_Quests/Audio/102.mp3".to_string(),
            FileEntry {
                relative_path: "VivaVoce_Quests/Audio/102.mp3".to_string(),
                blake3_hash: "correct_expected_hash".to_string(),
                size: 50,
            },
        );

        manifest.files.insert(
            "VivaVoce_Quests/Audio/103.mp3".to_string(), // completely missing on disk
            FileEntry {
                relative_path: "VivaVoce_Quests/Audio/103.mp3".to_string(),
                blake3_hash: "some_hash_103".to_string(),
                size: 100,
            },
        );

        manifest.recalculate_totals();

        let delta = scan_delta(addons_dir, &manifest).expect("scan_delta failed");

        assert_eq!(delta.files_matched, 1);
        assert_eq!(delta.files_to_download.len(), 2);
        assert_eq!(delta.bytes_missing, 150);

        // Orphan file should be flagged for deletion, but Details addon must NOT be touched
        assert_eq!(delta.files_to_delete.len(), 1);
        assert_eq!(delta.files_to_delete[0], orphan_file);
        assert!(!delta.files_to_delete.contains(&details_file));
    }
}
