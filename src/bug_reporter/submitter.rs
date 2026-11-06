use super::collector::BugReport;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Submitter;

impl Submitter {
    pub fn save_to_disk(report: &BugReport, target_dir: &Path) -> std::io::Result<PathBuf> {
        fs::create_dir_all(target_dir)?;
        let filename = format!("VivaVoce_BugReport_{}.md", report.timestamp_epoch);
        let file_path = target_dir.join(filename);
        let md = report.to_markdown();
        fs::write(&file_path, md)?;
        Ok(file_path)
    }

    pub fn create_mailto_url(report: &BugReport) -> String {
        let subject = format!(
            "[VivaVoce Bug] [{}] {}",
            report.category.as_str(),
            report.dialogue_title.as_deref().unwrap_or("Segnalazione Generale")
        );
        let body = report.to_markdown();
        format!(
            "mailto:vivavoce.project@gmail.com?subject={}&body={}",
            urlencoding::encode(&subject),
            urlencoding::encode(&body)
        )
    }

    pub fn build_github_issue_url(report: &BugReport) -> String {
        let title = format!(
            "[{}] {}",
            report.category.as_str(),
            report.dialogue_title.as_deref().unwrap_or("Segnalazione Audio")
        );
        let body = report.to_markdown();
        format!(
            "https://github.com/VivaVoceProject/VivaVoce/issues/new?title={}&body={}",
            urlencoding::encode(&title),
            urlencoding::encode(&body)
        )
    }
}

// Minimal zero-dependency urlencoder helper to avoid extra external crate
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
    use crate::bug_reporter::collector::BugCategory;

    #[test]
    fn test_save_to_disk() {
        let temp_dir = std::env::temp_dir().join("vivavoce_test_reports");
        let report = BugReport {
            category: BugCategory::CutOrGlitch,
            addon_version: "1.0.0".to_string(),
            client_flavor: "_forever_".to_string(),
            wow_path: "A:\\WoW".to_string(),
            sound_file: Some("Sounds/00/test.ogg".to_string()),
            quest_id: Some(999),
            npc_id: Some(1234),
            dialogue_title: Some("Test Quest".to_string()),
            dialogue_text: Some("Test Line".to_string()),
            user_description: "Interrotto a metà frase".to_string(),
            timestamp_epoch: 1730929999,
        };

        let path = Submitter::save_to_disk(&report, &temp_dir).expect("Failed to write report");
        assert!(path.exists());
        let content = fs::read_to_string(&path).expect("Failed to read report");
        assert!(content.contains("Interrotto a metà frase"));
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(temp_dir);
    }

    #[test]
    fn test_mailto_url() {
        let report = BugReport {
            category: BugCategory::Pronunciation,
            addon_version: "1.0.0".to_string(),
            client_flavor: "_forever_".to_string(),
            wow_path: "A:\\WoW".to_string(),
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: Some("Quest Prova".to_string()),
            dialogue_text: None,
            user_description: "Errore".to_string(),
            timestamp_epoch: 1730920000,
        };

        let mailto = Submitter::create_mailto_url(&report);
        assert!(mailto.starts_with("mailto:vivavoce.project@gmail.com?subject="));
        assert!(mailto.contains("Quest%20Prova"));
    }
}
