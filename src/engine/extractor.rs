use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::Instant;
use tokio::sync::mpsc::Sender;

use crate::engine::downloader::{ProgressEvent, ProgressStage};

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("I/O error during extraction: {0}")]
    Io(#[from] std::io::Error),
    #[error("Blake3 checksum mismatch: expected {expected}, actual {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("Invalid path in archive: {0}")]
    InvalidPath(String),
}

/// Summary report after archive extraction completes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractionSummary {
    /// Number of files successfully extracted
    pub files_extracted: usize,
    /// Total uncompressed bytes written to disk
    pub bytes_extracted: u64,
}

/// A transparent wrapper around `std::io::Read` that hashes bytes on the fly with Blake3
/// without needing an extra buffer or memory spike.
pub struct HashingReader<R> {
    inner: R,
    hasher: blake3::Hasher,
    total_bytes_read: u64,
}

impl<R: Read> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            total_bytes_read: 0,
        }
    }

    pub fn finalize_hash(&self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }

    pub fn total_bytes_read(&self) -> u64 {
        self.total_bytes_read
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
            self.total_bytes_read += n as u64;
        }
        Ok(n)
    }
}

/// Extracts a `.tar.zst` archive stream into `target_dir` with zero memory spikes.
///
/// Uses `zstd::stream::read::Decoder` directly coupled to `tar::Archive`, maintaining
/// a constant low memory footprint (< 15-30 MB) even for multi-gigabyte voice archives.
pub fn extract_tar_zst_stream<R: Read, P: AsRef<Path>>(
    reader: R,
    target_dir: P,
    expected_blake3: Option<&str>,
    progress_sender: Option<Sender<ProgressEvent>>,
) -> Result<ExtractionSummary, ExtractionError> {
    let target_dir = target_dir.as_ref();
    std::fs::create_dir_all(target_dir)?;

    let mut hashing_reader = HashingReader::new(BufReader::with_capacity(128 * 1024, reader));

    // Decompress on the fly
    let zstd_decoder = zstd::stream::read::Decoder::new(&mut hashing_reader)
        .map_err(ExtractionError::Io)?;

    let mut archive = tar::Archive::new(zstd_decoder);

    let mut files_extracted = 0;
    let mut bytes_extracted = 0u64;

    let mut last_progress_time = Instant::now();

    for entry_res in archive.entries()? {
        let mut entry = entry_res?;
        let entry_size = entry.size();

        // Safe extraction that prevents directory traversal attacks
        entry.unpack_in(target_dir)?;

        files_extracted += 1;
        bytes_extracted += entry_size;

        // Throttle progress notifications every 100ms or every 100 files
        if last_progress_time.elapsed().as_millis() >= 100 || files_extracted % 100 == 0 {
            last_progress_time = Instant::now();
            if let Some(ref sender) = progress_sender {
                let _ = sender.try_send(ProgressEvent {
                    stage: ProgressStage::Extracting,
                    current_bytes: bytes_extracted,
                    total_bytes: bytes_extracted,
                    speed_bps: 0.0,
                    message: format!("Estratti {} file...", files_extracted),
                    eta_seconds: None,
                });
            }
        }
    }

    // Verify Blake3 hash of the input compressed stream if expected
    if let Some(expected) = expected_blake3 {
        let actual = hashing_reader.finalize_hash();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(ExtractionError::ChecksumMismatch {
                expected: expected.to_string(),
                actual,
            });
        }
    }

    if let Some(ref sender) = progress_sender {
        let _ = sender.try_send(ProgressEvent {
            stage: ProgressStage::Completed,
            current_bytes: bytes_extracted,
            total_bytes: bytes_extracted,
            speed_bps: 0.0,
            message: format!("Estrazione completata: {} file estratti.", files_extracted),
            eta_seconds: Some(0),
        });
    }

    Ok(ExtractionSummary {
        files_extracted,
        bytes_extracted,
    })
}

/// Extracts a `.tar.zst` file from disk into `target_dir` with zero memory spikes.
pub fn extract_tar_zst_file<P: AsRef<Path>, Q: AsRef<Path>>(
    archive_path: P,
    target_dir: Q,
    expected_blake3: Option<&str>,
    progress_sender: Option<Sender<ProgressEvent>>,
) -> Result<ExtractionSummary, ExtractionError> {
    let file = File::open(archive_path)?;
    extract_tar_zst_stream(file, target_dir, expected_blake3, progress_sender)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_extract_synthetic_tar_zst() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let archive_path = temp_dir.path().join("test_addon.tar.zst");
        let extract_dir = temp_dir.path().join("Extracted");

        // 1. Create a synthetic tar archive in memory
        let mut tar_builder = tar::Builder::new(Vec::new());

        let file1_data = b"VivaVoce Addon Test File 1 Content";
        let mut header1 = tar::Header::new_gnu();
        header1.set_size(file1_data.len() as u64);
        header1.set_mode(0o644);
        header1.set_cksum();
        tar_builder.append_data(
            &mut header1,
            "VivaVoce_Quests/test1.txt",
            &file1_data[..],
        ).unwrap();

        let file2_data = b"VivaVoce Addon Test File 2 Content Subdir";
        let mut header2 = tar::Header::new_gnu();
        header2.set_size(file2_data.len() as u64);
        header2.set_mode(0o644);
        header2.set_cksum();
        tar_builder.append_data(
            &mut header2,
            "VivaVoce_Quests/Audio/sub/test2.mp3",
            &file2_data[..],
        ).unwrap();

        let tar_bytes = tar_builder.into_inner().unwrap();

        // 2. Compress with zstd
        let compressed = zstd::stream::encode_all(&tar_bytes[..], 3).unwrap();
        let expected_blake3 = blake3::hash(&compressed).to_hex().to_string();

        let mut out_file = File::create(&archive_path).unwrap();
        out_file.write_all(&compressed).unwrap();
        out_file.flush().unwrap();
        drop(out_file);

        // 3. Extract using streaming extractor
        let summary = extract_tar_zst_file(
            &archive_path,
            &extract_dir,
            Some(&expected_blake3),
            None,
        ).expect("Extraction failed");

        assert_eq!(summary.files_extracted, 2);
        assert_eq!(summary.bytes_extracted, (file1_data.len() + file2_data.len()) as u64);

        // Verify extracted files on disk
        let extracted_f1 = extract_dir.join("VivaVoce_Quests").join("test1.txt");
        let extracted_f2 = extract_dir.join("VivaVoce_Quests").join("Audio").join("sub").join("test2.mp3");

        assert!(extracted_f1.exists());
        assert!(extracted_f2.exists());
        assert_eq!(std::fs::read(&extracted_f1).unwrap(), file1_data);
        assert_eq!(std::fs::read(&extracted_f2).unwrap(), file2_data);
    }

    #[test]
    fn test_extract_checksum_mismatch_fails() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let archive_path = temp_dir.path().join("corrupt.tar.zst");
        let extract_dir = temp_dir.path().join("Extracted");

        let mut tar_builder = tar::Builder::new(Vec::new());
        let file_data = b"Some data";
        let mut header = tar::Header::new_gnu();
        header.set_size(file_data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder.append_data(&mut header, "test.txt", &file_data[..]).unwrap();
        let tar_bytes = tar_builder.into_inner().unwrap();
        let compressed = zstd::stream::encode_all(&tar_bytes[..], 3).unwrap();

        std::fs::write(&archive_path, &compressed).unwrap();

        let result = extract_tar_zst_file(
            &archive_path,
            &extract_dir,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
            None,
        );

        match result {
            Err(ExtractionError::ChecksumMismatch { expected, actual }) => {
                assert_eq!(expected, "0000000000000000000000000000000000000000000000000000000000000000");
                assert_eq!(actual, blake3::hash(&compressed).to_hex().to_string());
            }
            _ => panic!("Expected ChecksumMismatch error, got {:?}", result),
        }
    }
}
