//! Automatic self-update engine for the VivaVoce Launcher using GitHub Releases.
//!
//! Checks the latest release on GitHub, compares semver tags, streams the updated
//! binary with progress feedback, safely swaps executables on disk, and restarts the app.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::info;

pub const LAUNCHER_RELEASES_API_URL: &str =
    "https://api.github.com/repos/Master-Antonio/Vivavoce-Launcher/releases/latest";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemVer {
    pub fn parse(s: &str) -> Option<Self> {
        let clean = s.trim().trim_start_matches(|c| c == 'v' || c == 'V');
        // Strip any suffix like -beta, -rc1
        let core = clean.split('-').next().unwrap_or(clean);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(Self { major, minor, patch })
    }

    pub fn is_newer_than(&self, current: &Self) -> bool {
        (self.major, self.minor, self.patch) > (current.major, current.minor, current.patch)
    }
}

/// Checks whether a remote release tag is newer than the local application version.
pub fn is_newer_version(remote_tag: &str, current_version: &str) -> bool {
    match (SemVer::parse(remote_tag), SemVer::parse(current_version)) {
        (Some(remote), Some(current)) => remote.is_newer_than(&current),
        _ => false,
    }
}

/// Finds the most appropriate asset in the release for the current platform.
pub fn find_platform_asset<'a>(assets: &'a [ReleaseAsset]) -> Option<&'a ReleaseAsset> {
    #[cfg(target_os = "windows")]
    {
        // 1. Look for exact name containing windows and ending with .exe
        if let Some(a) = assets.iter().find(|a| a.name.to_ascii_lowercase().contains("windows") && a.name.ends_with(".exe")) {
            return Some(a);
        }
        // 2. Any .exe asset
        if let Some(a) = assets.iter().find(|a| a.name.ends_with(".exe")) {
            return Some(a);
        }
    }

    #[cfg(target_os = "macos")]
    {
        // 1. Standalone Universal macOS binary (direct executable replacement)
        if let Some(a) = assets.iter().find(|a| {
            let lower = a.name.to_ascii_lowercase();
            lower.contains("universal") && !lower.ends_with(".zip")
        }) {
            return Some(a);
        }

        // 2. Arch-specific direct binary
        if cfg!(target_arch = "aarch64") {
            if let Some(a) = assets.iter().find(|a| {
                let lower = a.name.to_ascii_lowercase();
                lower.contains("arm64") && !lower.ends_with(".zip")
            }) {
                return Some(a);
            }
        } else {
            if let Some(a) = assets.iter().find(|a| {
                let lower = a.name.to_ascii_lowercase();
                lower.contains("x86_64") && !lower.ends_with(".zip")
            }) {
                return Some(a);
            }
        }

        // 3. Any standalone macOS binary
        if let Some(a) = assets.iter().find(|a| {
            let lower = a.name.to_ascii_lowercase();
            (lower.contains("macos") || lower.contains("mac")) && !lower.ends_with(".zip")
        }) {
            return Some(a);
        }

        // 4. Zip archive fallback
        if let Some(a) = assets.iter().find(|a| a.name.ends_with(".zip")) {
            return Some(a);
        }
    }

    // Fallback: first asset
    assets.first()
}

/// Queries GitHub for the latest release of the Launcher.
pub async fn fetch_latest_release(api_url: &str) -> Result<ReleaseInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Errore client HTTP: {}", e))?;

    let resp = client
        .get(api_url)
        .header("User-Agent", concat!("VivaVoce-Launcher-Updater/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Connessione a GitHub fallita: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Risposta GitHub non valida: HTTP {}", resp.status()));
    }

    let release: ReleaseInfo = resp
        .json()
        .await
        .map_err(|e| format!("Parsing JSON release fallito: {}", e))?;

    Ok(release)
}

/// Downloads the update file and replaces the current executable.
pub async fn apply_self_update<F>(
    download_url: &str,
    progress_callback: F,
) -> Result<PathBuf, String>
where
    F: Fn(f32, u64, u64) + Send + 'static,
{
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Impossibile trovare il percorso dell'eseguibile corrente: {}", e))?;

    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| "Impossibile ricavare la cartella dell'eseguibile".to_string())?;

    let temp_download = exe_dir.join("vivavoce-launcher-update.tmp");

    // Clean up any existing temp file
    let _ = std::fs::remove_file(&temp_download);

    info!("Scaricamento aggiornamento da {}...", download_url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Errore client HTTP: {}", e))?;

    let mut resp = client
        .get(download_url)
        .header("User-Agent", concat!("VivaVoce-Launcher-Updater/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| format!("Download aggiornamento fallito: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Download fallito con status: HTTP {}", resp.status()));
    }

    let total_size = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(&temp_download)
        .await
        .map_err(|e| format!("Creazione file temporaneo fallita: {}", e))?;

    let mut downloaded: u64 = 0;

    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("Errore chunk: {}", e))? {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Errore scrittura chunk: {}", e))?;
        downloaded += chunk.len() as u64;
        let pct = if total_size > 0 {
            (downloaded as f32) / (total_size as f32)
        } else {
            0.5
        };
        progress_callback(pct, downloaded, total_size);
    }
    file.flush().await.map_err(|e| format!("Flush fallito: {}", e))?;
    drop(file);

    // Swap executable
    #[cfg(target_os = "windows")]
    {
        let pid = std::process::id();
        let stem = current_exe.file_stem().and_then(|s| s.to_str()).unwrap_or("vivavoce-launcher");
        let old_exe = exe_dir.join(format!("{}.old", stem));
        let pid_old_exe = exe_dir.join(format!("{}.{}.old", stem, pid));
        let _ = std::fs::remove_file(&old_exe);

        // Rename running exe to .old (Windows allows renaming running binaries in-place)
        let backup_path = if std::fs::rename(&current_exe, &old_exe).is_ok() {
            old_exe
        } else {
            let _ = std::fs::remove_file(&pid_old_exe);
            std::fs::rename(&current_exe, &pid_old_exe)
                .map_err(|e| format!("Impossibile rinominare l'eseguibile corrente: {}", e))?;
            pid_old_exe
        };

        // Move the downloaded binary into place
        if let Err(e) = std::fs::rename(&temp_download, &current_exe) {
            // Rollback if rename fails
            let _ = std::fs::rename(&backup_path, &current_exe);
            return Err(format!("Sostituzione eseguibile fallita: {}", e));
        }

        // Clean up backup if possible
        let _ = std::fs::remove_file(&backup_path);
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&temp_download) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&temp_download, perms);
        }

        let old_exe = exe_dir.join("vivavoce-launcher.old");
        let _ = std::fs::remove_file(&old_exe);
        let _ = std::fs::rename(&current_exe, &old_exe);
        if let Err(e) = std::fs::rename(&temp_download, &current_exe) {
            let _ = std::fs::rename(&old_exe, &current_exe);
            return Err(format!("Sostituzione eseguibile fallita: {}", e));
        }
        let _ = std::fs::remove_file(&old_exe);
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let old_exe = exe_dir.join("vivavoce-launcher.old");
        let _ = std::fs::remove_file(&old_exe);
        let _ = std::fs::rename(&current_exe, &old_exe);
        if let Err(e) = std::fs::rename(&temp_download, &current_exe) {
            let _ = std::fs::rename(&old_exe, &current_exe);
            return Err(format!("Sostituzione eseguibile fallita: {}", e));
        }
        let _ = std::fs::remove_file(&old_exe);
    }

    Ok(current_exe)
}

/// Spawns the newly updated executable and exits the current application.
pub fn restart_launcher(exe_path: &Path) -> std::io::Result<()> {
    info!("Riavvio VivaVoce Launcher da {:?}", exe_path);

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(exe_path).spawn()?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(exe_path).spawn()?;
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new(exe_path).spawn()?;
    }

    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semver_parsing_and_comparison() {
        assert!(is_newer_version("v0.1.1", "0.1.0"));
        assert!(is_newer_version("0.2.0", "0.1.9"));
        assert!(is_newer_version("v1.0.0", "0.9.9"));
        assert!(!is_newer_version("v0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
        assert!(!is_newer_version("invalid", "0.1.0"));
    }

    #[test]
    fn test_find_platform_asset_windows() {
        let assets = vec![
            ReleaseAsset {
                name: "vivavoce-launcher-macos-arm64".to_string(),
                browser_download_url: "https://example.com/mac".to_string(),
                size: 1000,
            },
            ReleaseAsset {
                name: "vivavoce-launcher-windows-x86_64.exe".to_string(),
                browser_download_url: "https://example.com/win.exe".to_string(),
                size: 2000,
            },
        ];

        let found = find_platform_asset(&assets);
        assert!(found.is_some());
        #[cfg(target_os = "windows")]
        assert_eq!(found.unwrap().name, "vivavoce-launcher-windows-x86_64.exe");
    }
}
