use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::Sender;

use crate::engine::manifest::{PackageChunk, PatchChunk};

/// Current phase of the download / installation process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgressStage {
    Connecting,
    Downloading,
    Verifying,
    Extracting,
    Completed,
    Failed,
}

/// Real-time progress event emitted to UI and background coordinators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub stage: ProgressStage,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: f64,
    pub message: String,
    pub eta_seconds: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Blake3 checksum mismatch: expected {expected}, calculated {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("HTTP error status: {0}")]
    HttpStatus(reqwest::StatusCode),
    #[error("Download cancelled")]
    Cancelled,
}

/// Asynchronous downloader providing chunked streaming, Blake3 verification, and ETA telemetry.
pub struct Downloader {
    client: reqwest::Client,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

impl Downloader {
    /// Creates a new Downloader with sensible default timeouts and user agent.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("VivaVoceLauncher/0.1.0")
            .pool_max_idle_per_host(4)
            .build()
            .unwrap_or_default();
        Self { client }
    }

    /// Creates a Downloader with a custom reqwest client.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Downloads a `PackageChunk` to the specified directory with streaming verification and progress.
    /// Returns the path to the downloaded archive.
    pub async fn download_package<P: AsRef<Path>>(
        &self,
        package: &PackageChunk,
        target_dir: P,
        progress: Option<Sender<ProgressEvent>>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Result<PathBuf, DownloadError> {
        let target_dir = target_dir.as_ref();
        tokio::fs::create_dir_all(target_dir).await?;

        let output_file_path = target_dir.join(&package.filename);
        let part_file_path = output_file_path.with_extension(format!("part.{}", std::process::id()));

        self.download_file_internal(
            &package.download_url,
            &part_file_path,
            Some(&package.blake3),
            Some(package.compressed_size),
            progress,
            cancel_token,
        )
        .await?;

        // Atomically rename part file to final file
        tokio::fs::rename(&part_file_path, &output_file_path).await?;
        Ok(output_file_path)
    }

    /// Downloads a `PatchChunk` delta update archive.
    pub async fn download_patch<P: AsRef<Path>>(
        &self,
        patch: &PatchChunk,
        target_dir: P,
        progress: Option<Sender<ProgressEvent>>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Result<PathBuf, DownloadError> {
        let target_dir = target_dir.as_ref();
        tokio::fs::create_dir_all(target_dir).await?;

        let output_file_path = target_dir.join(&patch.filename);
        let part_file_path = output_file_path.with_extension(format!("part.{}", std::process::id()));

        self.download_file_internal(
            &patch.download_url,
            &part_file_path,
            Some(&patch.blake3),
            Some(patch.compressed_size),
            progress,
            cancel_token,
        )
        .await?;

        tokio::fs::rename(&part_file_path, &output_file_path).await?;
        Ok(output_file_path)
    }

    /// Downloads a generic file with optional Blake3 verification.
    pub async fn download_file<P: AsRef<Path>>(
        &self,
        url: &str,
        dest_path: P,
        expected_blake3: Option<&str>,
        expected_size: Option<u64>,
        progress: Option<Sender<ProgressEvent>>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Result<PathBuf, DownloadError> {
        let dest_path = dest_path.as_ref();
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let part_file_path = dest_path.with_extension(format!("part.{}", std::process::id()));

        self.download_file_internal(
            url,
            &part_file_path,
            expected_blake3,
            expected_size,
            progress,
            cancel_token,
        )
        .await?;

        tokio::fs::rename(&part_file_path, dest_path).await?;
        Ok(dest_path.to_path_buf())
    }

    /// Internal streaming download implementation with on-the-fly Blake3 calculation.
    async fn download_file_internal(
        &self,
        url: &str,
        part_path: &Path,
        expected_blake3: Option<&str>,
        expected_total_bytes: Option<u64>,
        progress: Option<Sender<ProgressEvent>>,
        cancel_token: Option<Arc<AtomicBool>>,
    ) -> Result<(), DownloadError> {
        if let Some(ref token) = cancel_token {
            if token.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
        }

        // Notify connecting
        if let Some(ref sender) = progress {
            let _ = sender.send(ProgressEvent {
                stage: ProgressStage::Connecting,
                current_bytes: 0,
                total_bytes: expected_total_bytes.unwrap_or(0),
                speed_bps: 0.0,
                message: format!("Connessione a {}...", url),
                eta_seconds: None,
            }).await;
        }

        let response = self.client.get(url).send().await?;
        if !response.status().is_success() {
            return Err(DownloadError::HttpStatus(response.status()));
        }

        let content_length = response.content_length().or(expected_total_bytes).unwrap_or(0);

        let mut file = tokio::fs::File::create(part_path).await?;
        let mut hasher = blake3::Hasher::new();
        let mut current_bytes = 0u64;

        let start_time = Instant::now();
        let mut last_sample_time = Instant::now();
        let mut last_sample_bytes = 0u64;
        let mut current_speed = 0.0f64;

        let mut stream = response;

        while let Some(chunk) = stream.chunk().await? {
            if let Some(ref token) = cancel_token {
                if token.load(Ordering::Relaxed) {
                    let _ = tokio::fs::remove_file(part_path).await;
                    return Err(DownloadError::Cancelled);
                }
            }

            hasher.update(&chunk);
            file.write_all(&chunk).await?;

            current_bytes += chunk.len() as u64;

            let now = Instant::now();
            let elapsed_sample = (now - last_sample_time).as_secs_f64();

            // Update telemetry every 150ms or at completion
            if elapsed_sample >= 0.15 || current_bytes >= content_length {
                let delta_bytes = current_bytes - last_sample_bytes;
                let instant_speed = if elapsed_sample > 0.0 {
                    delta_bytes as f64 / elapsed_sample
                } else {
                    0.0
                };

                // Smoothed speed via exponential moving average (alpha = 0.6)
                if current_speed == 0.0 {
                    current_speed = instant_speed;
                } else {
                    current_speed = 0.6 * instant_speed + 0.4 * current_speed;
                }

                last_sample_time = now;
                last_sample_bytes = current_bytes;

                let eta_seconds = if current_speed > 0.0 && content_length > current_bytes {
                    Some(((content_length - current_bytes) as f64 / current_speed) as u64)
                } else {
                    None
                };

                if let Some(ref sender) = progress {
                    let _ = sender.send(ProgressEvent {
                        stage: ProgressStage::Downloading,
                        current_bytes,
                        total_bytes: content_length,
                        speed_bps: current_speed,
                        message: format!(
                            "Scaricamento in corso: {:.1} MB / {:.1} MB",
                            current_bytes as f64 / 1_048_576.0,
                            content_length as f64 / 1_048_576.0
                        ),
                        eta_seconds,
                    }).await;
                }
            }
        }

        file.flush().await?;
        drop(file);

        // Verification phase
        if let Some(ref sender) = progress {
            let _ = sender.send(ProgressEvent {
                stage: ProgressStage::Verifying,
                current_bytes,
                total_bytes: content_length,
                speed_bps: 0.0,
                message: "Verifica integrità Blake3 in corso...".to_string(),
                eta_seconds: None,
            }).await;
        }

        let calculated_hash = hasher.finalize().to_hex().to_string();

        if let Some(expected) = expected_blake3 {
            if !calculated_hash.eq_ignore_ascii_case(expected) {
                let _ = tokio::fs::remove_file(part_path).await;
                return Err(DownloadError::ChecksumMismatch {
                    expected: expected.to_string(),
                    actual: calculated_hash,
                });
            }
        }

        let total_time = start_time.elapsed().as_secs_f64();
        let avg_speed = if total_time > 0.0 {
            current_bytes as f64 / total_time
        } else {
            0.0
        };

        if let Some(ref sender) = progress {
            let _ = sender.send(ProgressEvent {
                stage: ProgressStage::Completed,
                current_bytes,
                total_bytes: content_length,
                speed_bps: avg_speed,
                message: "Scaricamento e verifica completati con successo.".to_string(),
                eta_seconds: Some(0),
            }).await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_stage_serialization() {
        let event = ProgressEvent {
            stage: ProgressStage::Downloading,
            current_bytes: 1024,
            total_bytes: 2048,
            speed_bps: 512.0,
            message: "Testing".to_string(),
            eta_seconds: Some(2),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"stage\":\"Downloading\""));
        assert!(json.contains("\"speed_bps\":512.0"));
    }

    #[tokio::test]
    async fn test_downloader_cancellation_token() {
        let downloader = Downloader::new();
        let cancel_token = Arc::new(AtomicBool::new(true)); // Pre-cancelled

        let temp_dir = tempfile::tempdir().unwrap();
        let package = PackageChunk {
            id: "test".to_string(),
            filename: "test.tar.zst".to_string(),
            blake3: "hash".to_string(),
            download_url: "http://127.0.0.1:9/never".to_string(),
            compressed_size: 100,
            uncompressed_size: 200,
        };

        let res = downloader.download_package(
            &package,
            temp_dir.path(),
            None,
            Some(cancel_token),
        ).await;

        match res {
            Err(DownloadError::Cancelled) => (),
            other => panic!("Expected Cancelled error, got {:?}", other),
        }
    }
}
