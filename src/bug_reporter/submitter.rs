use super::collector::{BugReport, IssueType};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{error, info};

pub struct Submitter;

impl Submitter {
    /// Saves the bug report as a local Markdown file
    pub fn save_to_disk(report: &BugReport, target_dir: &Path) -> std::io::Result<PathBuf> {
        fs::create_dir_all(target_dir)?;
        let prefix = match report.issue_type {
            IssueType::Audio => "AudioReport",
            IssueType::Addon => "AddonReport",
            IssueType::Launcher => "LauncherReport",
        };
        let filename = format!("VivaVoce_{}_{}.md", prefix, report.timestamp_epoch);
        let file_path = target_dir.join(filename);
        let md = report.to_markdown();
        fs::write(&file_path, md)?;
        Ok(file_path)
    }

    /// Native clipboard text copy (Windows: clip, macOS: pbcopy, Linux: xclip)
    pub fn copy_to_clipboard(text: &str) {
        #[cfg(target_os = "windows")]
        if let Ok(mut child) = std::process::Command::new("clip")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }

        #[cfg(target_os = "macos")]
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        if let Ok(mut child) = std::process::Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }

    /// Copies an image or media file directly to the clipboard.
    /// - Windows: PowerShell with CF_DIB (in-memory image) and CF_HDROP.
    /// - macOS: osascript with native PNG/JPEG clipboard classes.
    pub fn copy_media_to_clipboard(path: &Path) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let p_str = path.to_string_lossy().to_string();
            let escaped = p_str.replace('\'', "''");

            // Adds both CF_HDROP (file drop) and CF_DIB (in-memory image bitmap) to the Windows clipboard.
            // Using MemoryStream ensures the original file on disk is never locked.
            // [System.Windows.Forms.Clipboard]::SetDataObject($do, $true) persists the clipboard after the PowerShell process exits.
            let script = format!(
                "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
                try {{ \
                    $do = New-Object System.Windows.Forms.DataObject; \
                    $files = New-Object System.Collections.Specialized.StringCollection; \
                    [void]$files.Add('{0}'); \
                    $do.SetFileDropList($files); \
                    try {{ \
                        $bytes = [System.IO.File]::ReadAllBytes('{0}'); \
                        $ms = New-Object System.IO.MemoryStream(,$bytes); \
                        $img = [System.Drawing.Image]::FromStream($ms); \
                        $do.SetImage($img); \
                    }} catch {{}} \
                    [System.Windows.Forms.Clipboard]::SetDataObject($do, $true); \
                }} catch {{}}",
                escaped
            );

            let _ = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .status();
        }

        #[cfg(target_os = "macos")]
        {
            let p_str = path.to_string_lossy();
            let lower = p_str.to_lowercase();
            let script = if lower.ends_with(".png") {
                format!("set the clipboard to (read (POSIX file \"{}\") as «class PNGf»)", p_str)
            } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                format!("set the clipboard to (read (POSIX file \"{}\") as JPEG picture)", p_str)
            } else {
                format!("set the clipboard to (POSIX file \"{}\")", p_str)
            };

            let _ = std::process::Command::new("osascript")
                .args(["-e", &script])
                .status();
        }
    }

    /// Highlights the file in file manager (Windows: Explorer, macOS: Finder, Linux: file manager)
    pub fn reveal_in_explorer(path: &Path) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let arg = format!("/select,\"{}\"", path.to_string_lossy());
            let _ = std::process::Command::new("explorer")
                .arg(arg)
                .creation_flags(0x08000000)
                .spawn();
        }

        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .args(["-R", &path.to_string_lossy()])
                .spawn();
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            if let Some(parent) = path.parent() {
                let _ = std::process::Command::new("xdg-open")
                    .arg(parent)
                    .spawn();
            }
        }
    }

    /// Builds a pre-filled GitHub new issue URL with labels, title and markdown body
    pub fn build_github_issue_url(report: &BugReport) -> String {
        let (tag, repo) = match report.issue_type {
            IssueType::Audio => ("Audio", "Master-Antonio/Vivavoce-Releases"),
            IssueType::Addon => ("Addon", "Master-Antonio/Vivavoce-Releases"),
            IssueType::Launcher => ("Launcher", "Master-Antonio/Vivavoce-Launcher"),
        };
        let title = format!("[{}] {}: {}", tag, report.category, report.title);
        let label = report.issue_type.github_label();
        let body = report.to_markdown();

        format!(
            "https://github.com/{}/issues/new?title={}&labels={}&body={}",
            repo,
            urlencoding::encode(&title),
            urlencoding::encode(label),
            urlencoding::encode(&body)
        )
    }

    /// Submits the issue directly via GitHub REST API if a Personal Access Token is configured
    pub async fn submit_to_github_api(
        report: &BugReport,
        token: &str,
    ) -> Result<String, String> {
        let (tag, repo) = match report.issue_type {
            IssueType::Audio => ("Audio", "Master-Antonio/Vivavoce-Releases"),
            IssueType::Addon => ("Addon", "Master-Antonio/Vivavoce-Releases"),
            IssueType::Launcher => ("Launcher", "Master-Antonio/Vivavoce-Launcher"),
        };
        let title = format!("[{}] {}: {}", tag, report.category, report.title);
        let label = report.issue_type.github_label();
        let body = report.to_markdown();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("Errore client HTTP: {}", e))?;

        let payload = serde_json::json!({
            "title": title,
            "body": body,
            "labels": [label]
        });

        let url = format!("https://api.github.com/repos/{}/issues", repo);

        info!("Invio issue via GitHub REST API verso {}...", url);

        let resp = client
            .post(&url)
            .header("User-Agent", "VivaVoceLauncher/0.1.0")
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", format!("Bearer {}", token.trim()))
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Connessione a GitHub non riuscita: {}", e))?;

        let status = resp.status();
        if status.is_success() {
            let json_body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("Risposta GitHub non valida: {}", e))?;

            if let Some(html_url) = json_body.get("html_url").and_then(|v| v.as_str()) {
                info!("Issue creata con successo su GitHub: {}", html_url);
                return Ok(html_url.to_string());
            }

            Ok(format!("https://github.com/{}/issues", repo))
        } else if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            error!("GitHub API token non valido o privo di permessi");
            Err("Token GitHub non valido o privo dei permessi 'public_repo'.".to_string())
        } else {
            let error_text = resp.text().await.unwrap_or_default();
            error!("GitHub API ha restituito errore {}: {}", status, error_text);
            Err(format!("Errore da GitHub API ({}): {}", status, error_text))
        }
    }

    /// Fallback mailto URL
    pub fn create_mailto_url(report: &BugReport) -> String {
        let tag = match report.issue_type {
            IssueType::Audio => "Audio",
            IssueType::Addon => "Addon",
            IssueType::Launcher => "Launcher",
        };
        let subject = format!("[VivaVoce {}] [{}] {}", tag, report.category, report.title);
        let body = report.to_markdown();
        format!(
            "mailto:vivavoce.project@gmail.com?subject={}&body={}",
            urlencoding::encode(&subject),
            urlencoding::encode(&body)
        )
    }
}

// Minimal zero-dependency urlencoder helper
mod urlencoding {
    pub fn encode(data: &str) -> String {
        let mut encoded = String::with_capacity(data.len() * 3);
        for byte in data.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    encoded.push(byte as char);
                }
                b' ' => encoded.push_str("%20"),
                b'\n' => encoded.push_str("%0A"),
                b'\r' => encoded.push_str("%0D"),
                _ => {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bug_reporter::collector::IssueType;

    #[test]
    fn test_build_github_issue_url() {
        let report = BugReport {
            issue_type: IssueType::Audio,
            category: "Pronuncia ed enfasi".to_string(),
            title: "Re Varian Wrynn".to_string(),
            user_description: "L'accento su Stormwind è strano".to_string(),
            repro_steps: None,
            addon_version: "1.0.0".to_string(),
            client_flavor: "Forever 1.12.1".to_string(),
            wow_path: "A:\\WoW".to_string(),
            os_info: "Windows 11".to_string(),
            sound_file: Some("Sounds/12/12345.ogg".to_string()),
            quest_id: Some(42),
            npc_id: Some(1748),
            dialogue_title: Some("Re Varian Wrynn".to_string()),
            dialogue_text: Some("Per l'Alleanza!".to_string()),
            attachment_path: None,
            timestamp_epoch: 1730920000,
        };

        let url = Submitter::build_github_issue_url(&report);
        assert!(url.starts_with("https://github.com/Master-Antonio/Vivavoce-Releases/issues/new?"));
        assert!(url.contains("labels=audio-bug"));
        assert!(url.contains("%5BAudio%5D"));
    }

    #[test]
    fn test_build_launcher_github_issue_url() {
        let report = BugReport {
            issue_type: IssueType::Launcher,
            category: "Crash o chiusura improvvisa".to_string(),
            title: "Crash scansione".to_string(),
            user_description: "Crash immediato".to_string(),
            repro_steps: None,
            addon_version: "1.0.0".to_string(),
            client_flavor: "Forever 1.12.1".to_string(),
            wow_path: "A:\\WoW".to_string(),
            os_info: "Windows 11".to_string(),
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: None,
            dialogue_text: None,
            attachment_path: None,
            timestamp_epoch: 1730920000,
        };

        let url = Submitter::build_github_issue_url(&report);
        assert!(url.starts_with("https://github.com/Master-Antonio/Vivavoce-Launcher/issues/new?"));
        assert!(url.contains("labels=launcher-bug"));
        assert!(url.contains("%5BLauncher%5D"));
    }

    #[test]
    #[cfg(windows)]
    fn test_copy_media_to_clipboard() {
        let logo_path = std::path::Path::new("ui/assets/logo.png");
        assert!(logo_path.exists());
        Submitter::copy_media_to_clipboard(logo_path);
    }
}
