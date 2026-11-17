use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Tentativi per file. Un tentativo che ha scaricato qualcosa non conta: su una
/// linea instabile un archivio da 1,5 GB puo' cadere molte volte e va finito.
const MAX_ATTEMPTS: u32 = 6;
/// Nessun byte per questo tempo = connessione bloccata: si chiude e si riprende.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

async fn backoff(attempt: u32) {
    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt.min(5)))).await;
}

/// Blake3 di un file gia' su disco (archivio completo o parte scaricata).
async fn hash_existing(path: &Path, hasher: &mut blake3::Hasher) -> std::io::Result<u64> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok(total)
}

/// Archivio gia' scaricato da un tentativo precedente, integro: si riusa.
async fn already_downloaded(path: &Path, expected_blake3: &str, expected_size: u64) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() == expected_size && expected_size > 0 => {
            let mut hasher = blake3::Hasher::new();
            hash_existing(path, &mut hasher).await.is_ok()
                && hasher.finalize().to_hex().to_string().eq_ignore_ascii_case(expected_blake3)
        }
        _ => false,
    }
}
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
    #[error("Download interrotto: {got} di {expected} byte dopo {attempts} tentativi")]
    Incomplete { got: u64, expected: u64, attempts: u32 },
    #[error("Connessione bloccata da {0} secondi")]
    Stalled(u64),
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
            .user_agent(concat!("VivaVoceLauncher/", env!("CARGO_PKG_VERSION")))
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
        if already_downloaded(&output_file_path, &package.blake3, package.compressed_size).await {
            return Ok(output_file_path);
        }
        // Nome fisso (non piu' l'id del processo): un download interrotto
        // riprende anche dopo aver chiuso e riaperto il launcher.
        let part_file_path = output_file_path.with_extension("part");

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
        if already_downloaded(&output_file_path, &patch.blake3, patch.compressed_size).await {
            return Ok(output_file_path);
        }
        let part_file_path = output_file_path.with_extension("part");

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

        let expected = expected_total_bytes.filter(|t| *t > 0);
        let mut hasher = blake3::Hasher::new();
        let mut current_bytes = 0u64;

        // Parte di un tentativo precedente (anche di un'altra sessione): si
        // ripassa nel Blake3 e si chiede al server solo il resto.
        if let Ok(meta) = tokio::fs::metadata(part_path).await {
            let len = meta.len();
            if len > 0 && expected.map_or(true, |t| len <= t) {
                current_bytes = hash_existing(part_path, &mut hasher).await?;
            } else {
                let _ = tokio::fs::remove_file(part_path).await;
            }
        }

        let start_time = Instant::now();
        let start_bytes = current_bytes;
        let mut last_sample_time = Instant::now();
        let mut last_sample_bytes = current_bytes;
        let mut current_speed = 0.0f64;
        let mut content_length = expected.unwrap_or(0);
        let mut attempt = 0u32;

        'attempts: loop {
            if let Some(total) = expected {
                if current_bytes >= total {
                    break;
                }
            }
            attempt += 1;
            if attempt > 1 {
                if let Some(ref sender) = progress {
                    let _ = sender.send(ProgressEvent {
                        stage: ProgressStage::Connecting,
                        current_bytes,
                        total_bytes: content_length,
                        speed_bps: 0.0,
                        message: format!("Connessione persa, riprendo da {:.1} MB (tentativo {}/{})...",
                            current_bytes as f64 / 1_048_576.0, attempt, MAX_ATTEMPTS),
                        eta_seconds: None,
                    }).await;
                }
                backoff(attempt - 1).await;
            }
            if let Some(ref token) = cancel_token {
                if token.load(Ordering::Relaxed) {
                    return Err(DownloadError::Cancelled);
                }
            }

            let mut request = self.client.get(url);
            if current_bytes > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={}-", current_bytes));
            }
            let response = match request.send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(e.into());
                    }
                    continue 'attempts;
                }
            };
            let status = response.status();
            let resuming = current_bytes > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
            if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
                // Parte su disco non valida per il server: si ricomincia da zero.
                let _ = tokio::fs::remove_file(part_path).await;
                hasher = blake3::Hasher::new();
                current_bytes = 0;
                last_sample_bytes = 0;
                if attempt >= MAX_ATTEMPTS {
                    return Err(DownloadError::HttpStatus(status));
                }
                continue 'attempts;
            }
            if !status.is_success() {
                // 4xx (file assente, permessi): inutile insistere. 5xx: si riprova.
                if attempt >= MAX_ATTEMPTS || status.is_client_error() {
                    return Err(DownloadError::HttpStatus(status));
                }
                continue 'attempts;
            }
            if current_bytes > 0 && !resuming {
                // Il server ha ignorato Range e rimanda tutto: si riparte.
                hasher = blake3::Hasher::new();
                current_bytes = 0;
                last_sample_bytes = 0;
            }
            if content_length == 0 {
                content_length = current_bytes + response.content_length().unwrap_or(0);
            }

            let mut file = if current_bytes > 0 {
                tokio::fs::OpenOptions::new().append(true).open(part_path).await?
            } else {
                tokio::fs::File::create(part_path).await?
            };
            let bytes_at_start = current_bytes;
            let mut stream = response;

            loop {
                if let Some(ref token) = cancel_token {
                    if token.load(Ordering::Relaxed) {
                        // La parte resta su disco: al prossimo avvio si riprende da qui.
                        let _ = file.flush().await;
                        return Err(DownloadError::Cancelled);
                    }
                }
                let chunk = match tokio::time::timeout(STALL_TIMEOUT, stream.chunk()).await {
                    Ok(Ok(Some(chunk))) => chunk,
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        file.flush().await?;
                        if current_bytes > bytes_at_start {
                            attempt = 0; // ha scaricato qualcosa: non si consuma un tentativo
                        } else if attempt >= MAX_ATTEMPTS {
                            return Err(e.into());
                        }
                        continue 'attempts;
                    }
                    Err(_) => {
                        file.flush().await?;
                        if current_bytes > bytes_at_start {
                            attempt = 0;
                        } else if attempt >= MAX_ATTEMPTS {
                            return Err(DownloadError::Stalled(STALL_TIMEOUT.as_secs()));
                        }
                        continue 'attempts;
                    }
                };

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

            // Corpo finito: se mancano byte (connessione chiusa senza errore)
            // si riprende; altrimenti il file e' completo.
            if let Some(total) = expected {
                if current_bytes < total {
                    if current_bytes > bytes_at_start {
                        attempt = 0;
                    } else if attempt >= MAX_ATTEMPTS {
                        return Err(DownloadError::Incomplete {
                            got: current_bytes, expected: total, attempts: attempt,
                        });
                    }
                    continue 'attempts;
                }
            }
            break 'attempts;
        }
        let _ = start_bytes;

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

    /// Ripresa reale da GitHub: meta' archivio gia' su disco come `.part`, il
    /// downloader chiede solo il resto (Range) e il Blake3 finale torna.
    /// Rete: `cargo test -- --ignored resume_from_partial_file`.
    #[tokio::test]
    #[ignore]
    async fn resume_from_partial_file() {
        let package = PackageChunk {
            id: "vivavoce_codex".to_string(),
            filename: "VivaVoce_Codex.tar.zst".to_string(),
            blake3: "0cb1a30adf710c48cbdcbb467c024c8bc38b7410fa91580d62faa7335a8a4c2c".to_string(),
            download_url: "https://github.com/Master-Antonio/Vivavoce-Releases/releases/download/v1.2.0/VivaVoce_Codex.tar.zst".to_string(),
            compressed_size: 276886,
            uncompressed_size: 1394735,
        };
        let client = reqwest::Client::new();
        let half = package.compressed_size / 2;
        let head = client.get(&package.download_url)
            .header(reqwest::header::RANGE, format!("bytes=0-{}", half - 1))
            .send().await.unwrap().bytes().await.unwrap();
        assert_eq!(head.len() as u64, half);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("VivaVoce_Codex.part"), &head).unwrap();

        let downloader = Downloader::new();
        let path = downloader.download_package(&package, dir.path(), None, None).await.unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), package.compressed_size);
        // Secondo giro: archivio gia' completo e integro, niente download.
        let again = downloader.download_package(&package, dir.path(), None, None).await.unwrap();
        assert_eq!(again, path);
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
