//! Core engine module for VivaVoceLauncher.
//!
//! Handles manifest parsing, high-speed Blake3 delta scanning, asynchronous streaming
//! downloading, low-memory streaming extraction of .tar.zst packages, and safe uninstallation.

pub mod manifest;
pub mod scanner;
pub mod downloader;
pub mod extractor;
pub mod uninstaller;

pub use manifest::{normalize_relative_path, FileEntry, LocalState, PackageChunk, RemoteManifest};
pub use scanner::{hash_file_blake3, scan_delta, DeltaScanResult, VIVAVOCE_MANAGED_MODULES};
pub use downloader::{Downloader, DownloadError, ProgressEvent, ProgressStage};
pub use extractor::{
    extract_tar_zst_file, extract_tar_zst_stream, ExtractionError, ExtractionSummary, HashingReader,
};
pub use uninstaller::{
    calculate_dir_size, detect_installed_modules, safe_remove_dir_all, uninstall_addons_only,
    uninstall_vivavoce, UninstallResult,
};
