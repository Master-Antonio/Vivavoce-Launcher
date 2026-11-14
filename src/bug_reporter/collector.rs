use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Audio,
    Addon,
    Launcher,
}

impl IssueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Audio => "Audio / Doppiaggio",
            Self::Addon => "AddOn / Tecnico",
            Self::Launcher => "Launcher / Applicazione Desktop",
        }
    }

    pub fn github_label(&self) -> &'static str {
        match self {
            Self::Audio => "audio-bug",
            Self::Addon => "addon-bug",
            Self::Launcher => "launcher-bug",
        }
    }
}

// Kept for backward compatibility with existing tests and imports
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugCategory {
    MissingVoice,
    Pronunciation,
    VolumeOrQuality,
    GenderMismatch,
    CutOrGlitch,
    TextMismatch,
    LuaError,
    Other,
}

impl BugCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingVoice => "Voce mancante (non doppiata)",
            Self::Pronunciation => "Pronuncia / Accento",
            Self::VolumeOrQuality => "Volume / Qualità Audio",
            Self::GenderMismatch => "Voce o sesso errato (M/F)",
            Self::CutOrGlitch => "Audio tagliato / Glitch",
            Self::TextMismatch => "Testo e Voce non corrispondono",
            Self::LuaError => "Errore Lua / AddOn Crash",
            Self::Other => "Altro / Suggerimento",
        }
    }

    pub fn from_str_lenient(s: &str) -> Self {
        let lower = s.to_lowercase();
        if lower.contains("mancant") || lower.contains("assente") || lower.contains("non doppiata") || lower.contains("manca") {
            Self::MissingVoice
        } else if lower.contains("pronun") || lower.contains("accent") {
            Self::Pronunciation
        } else if lower.contains("volum") || lower.contains("qualit") || lower.contains("basso") {
            Self::VolumeOrQuality
        } else if lower.contains("sesso") || lower.contains("genere") || lower.contains("femmin") || lower.contains("masch") {
            Self::GenderMismatch
        } else if lower.contains("taglia") || lower.contains("glitch") || lower.contains("tronc") {
            Self::CutOrGlitch
        } else if lower.contains("testo") || lower.contains("incongru") || lower.contains("mismatch") {
            Self::TextMismatch
        } else if lower.contains("lua") || lower.contains("crash") || lower.contains("error") {
            Self::LuaError
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugReport {
    pub issue_type: IssueType,
    pub category: String,
    pub title: String,
    pub user_description: String,
    pub repro_steps: Option<String>,
    pub addon_version: String,
    pub client_flavor: String,
    pub wow_path: String,
    pub os_info: String,
    pub sound_file: Option<String>,
    pub quest_id: Option<u32>,
    pub npc_id: Option<u32>,
    pub dialogue_title: Option<String>,
    pub dialogue_text: Option<String>,
    pub attachment_path: Option<String>,
    pub timestamp_epoch: u64,
}

impl BugReport {
    pub fn to_markdown(&self) -> String {
        let mut md = String::with_capacity(1024);

        match self.issue_type {
            IssueType::Audio => {
                md.push_str(&format!("# [Audio] {}: {}\n\n", self.category, self.title));
                md.push_str(&format!("- **Tipo Segnalazione**: {}\n", self.issue_type.as_str()));
                md.push_str(&format!("- **Categoria**: {}\n", self.category));
                md.push_str(&format!("- **Data/Ora (Epoch)**: {}\n", self.timestamp_epoch));
                md.push_str(&format!("- **Versione AddOn**: {}\n", self.addon_version));
                md.push_str(&format!("- **Client WoW**: {}\n", self.client_flavor));
                md.push_str(&format!("- **Percorso Installazione**: `{}`\n\n", self.wow_path));

                md.push_str("## Dati Dialogo / Audio Coinvolto\n");
                if let Some(ref sound) = self.sound_file {
                    md.push_str(&format!("- **File Audio**: `{}`\n", sound));
                }
                if let Some(qid) = self.quest_id {
                    md.push_str(&format!("- **Quest ID**: `{}`\n", qid));
                }
                if let Some(nid) = self.npc_id {
                    md.push_str(&format!("- **Creature/NPC ID**: `{}`\n", nid));
                }
                if let Some(ref title) = self.dialogue_title {
                    md.push_str(&format!("- **Titolo Quest / NPC**: {}\n", title));
                }
                if let Some(ref text) = self.dialogue_text {
                    md.push_str(&format!("- **Testo a schermo**:\n> {}\n\n", text.trim()));
                }

                md.push_str("## Descrizione del Giocatore\n");
                if self.user_description.trim().is_empty() {
                    md.push_str("*(Nessuna nota aggiuntiva fornita)*\n");
                } else {
                    md.push_str(&format!("{}\n", self.user_description.trim()));
                }
            }
            IssueType::Addon => {
                md.push_str(&format!("# [Addon] {}: {}\n\n", self.category, self.title));
                md.push_str(&format!("- **Tipo Segnalazione**: {}\n", self.issue_type.as_str()));
                md.push_str(&format!("- **Categoria**: {}\n", self.category));
                md.push_str(&format!("- **Data/Ora (Epoch)**: {}\n", self.timestamp_epoch));
                md.push_str(&format!("- **Versione AddOn VivaVoce**: {}\n", self.addon_version));
                md.push_str(&format!("- **Client WoW**: {}\n", self.client_flavor));
                md.push_str(&format!("- **Sistema Operativo**: {}\n", self.os_info));
                md.push_str(&format!("- **Percorso Installazione**: `{}`\n\n", self.wow_path));

                md.push_str("## Descrizione del Problema / Errore Lua\n");
                if self.user_description.trim().is_empty() {
                    md.push_str("*(Nessuna descrizione o log fornito)*\n\n");
                } else {
                    md.push_str(&format!("```lua\n{}\n```\n\n", self.user_description.trim()));
                }

                if let Some(ref steps) = self.repro_steps {
                    if !steps.trim().is_empty() {
                        md.push_str("## Passaggi per Riprodurre\n");
                        md.push_str(&format!("{}\n\n", steps.trim()));
                    }
                }

                md.push_str("## Informazioni sull'Ambiente\n");
                md.push_str(&format!("- **Launcher**: VivaVoce Native v{} (Rust + Slint)\n", env!("CARGO_PKG_VERSION")));
                md.push_str("- **Architettura**: 6 componenti modulari integrati\n");
            }
            IssueType::Launcher => {
                md.push_str(&format!("# [Launcher] {}: {}\n\n", self.category, self.title));
                md.push_str(&format!("- **Tipo Segnalazione**: {}\n", self.issue_type.as_str()));
                md.push_str(&format!("- **Categoria**: {}\n", self.category));
                md.push_str(&format!("- **Data/Ora (Epoch)**: {}\n", self.timestamp_epoch));
                md.push_str(&format!("- **Versione Launcher**: VivaVoce Native v{}\n", env!("CARGO_PKG_VERSION")));
                md.push_str(&format!("- **Client WoW Rilevato**: {}\n", self.client_flavor));
                md.push_str(&format!("- **Sistema Operativo**: {}\n", self.os_info));
                md.push_str(&format!("- **Percorso Installazione**: `{}`\n\n", self.wow_path));

                md.push_str("## Descrizione del Problema / Comportamento Anomalo\n");
                if self.user_description.trim().is_empty() {
                    md.push_str("*(Nessuna descrizione o log fornito)*\n\n");
                } else {
                    md.push_str(&format!("{}\n\n", self.user_description.trim()));
                }

                if let Some(ref steps) = self.repro_steps {
                    if !steps.trim().is_empty() {
                        md.push_str("## Passaggi per Riprodurre\n");
                        md.push_str(&format!("{}\n\n", steps.trim()));
                    }
                }

                md.push_str("## Informazioni sull'Ambiente Launcher\n");
                md.push_str(&format!("- **Versione Launcher**: VivaVoce Native v{} (Slint GUI)\n", env!("CARGO_PKG_VERSION")));
                md.push_str("- **Architettura Scanner**: BLAKE3 Parallel Delta Scanner\n");
                md.push_str("- **Repository Destinazione**: https://github.com/Master-Antonio/Vivavoce-Launcher\n");
            }
        }

        if let Some(ref path) = self.attachment_path {
            if !path.trim().is_empty() {
                let filename = std::path::Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let lower = path.to_lowercase();
                let is_image = lower.ends_with(".png")
                    || lower.ends_with(".jpg")
                    || lower.ends_with(".jpeg")
                    || lower.ends_with(".gif")
                    || lower.ends_with(".bmp")
                    || lower.ends_with(".webp");

                md.push_str("\n## Allegato Multimediale (Screenshot / Video)\n");
                md.push_str(&format!("- **File**: `{}`\n", filename));
                md.push_str(&format!("- **Percorso locale**: `{}`\n", path.trim()));
                if is_image {
                    md.push_str("💡 **Suggerimento**: L'immagine è stata copiata automaticamente negli appunti! **Premi CTRL+V qui sotto per incollarla direttamente nell'Issue**, oppure trascina il file nella casella.\n");
                } else {
                    md.push_str("💡 **Suggerimento**: Trascina direttamente questo file video nella casella sottostante per allegarlo all'Issue.\n");
                }
            }
        }

        md.push_str("\n---\n*Generato automaticamente da VivaVoce Launcher Native (Rust)*\n");
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_report_markdown() {
        let report = BugReport {
            issue_type: IssueType::Audio,
            category: "Pronuncia ed enfasi".to_string(),
            title: "Il destino di Ironforge".to_string(),
            user_description: "L'accento sulla seconda parola è sbagliato".to_string(),
            repro_steps: None,
            addon_version: "1.0.0".to_string(),
            client_flavor: "WoW Forever".to_string(),
            wow_path: "A:\\WoW".to_string(),
            os_info: "Windows 11".to_string(),
            sound_file: Some("Sounds/16/16013_q123_4.ogg".to_string()),
            quest_id: Some(123),
            npc_id: Some(16013),
            dialogue_title: Some("Il destino di Ironforge".to_string()),
            dialogue_text: Some("Benvenuto viaggiatore!".to_string()),
            attachment_path: Some("C:\\Screenshots\\ironforge_npc.png".to_string()),
            timestamp_epoch: 1730928000,
        };

        let md = report.to_markdown();
        assert!(md.contains("# [Audio] Pronuncia ed enfasi: Il destino di Ironforge"));
        assert!(md.contains("Sounds/16/16013_q123_4.ogg"));
        assert!(md.contains("16013"));
        assert!(md.contains("ironforge_npc.png"));
    }

    #[test]
    fn test_addon_report_markdown() {
        let report = BugReport {
            issue_type: IssueType::Addon,
            category: "Errore Lua in gioco".to_string(),
            title: "Crash apertura QuestLog".to_string(),
            user_description: "Interface/AddOns/VivaVoce/Core.lua:42: attempt to index nil".to_string(),
            repro_steps: Some("1. Parlo con NPC\n2. Clicco Accetta".to_string()),
            addon_version: "1.0.0".to_string(),
            client_flavor: "WoW Forever".to_string(),
            wow_path: "A:\\WoW".to_string(),
            os_info: "Windows 11".to_string(),
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: None,
            dialogue_text: None,
            attachment_path: None,
            timestamp_epoch: 1730928000,
        };

        let md = report.to_markdown();
        assert!(md.contains("# [Addon] Errore Lua in gioco: Crash apertura QuestLog"));
        assert!(md.contains("```lua"));
        assert!(md.contains("Passaggi per Riprodurre"));
    }

    #[test]
    fn test_launcher_report_markdown() {
        let report = BugReport {
            issue_type: IssueType::Launcher,
            category: "Crash o chiusura improvvisa".to_string(),
            title: "Crash durante scansione BLAKE3".to_string(),
            user_description: "Il launcher si chiude all'istante cliccando Verifica integrità".to_string(),
            repro_steps: Some("1. Clicco Verifica integrità\n2. Chiusura immediata".to_string()),
            addon_version: "1.0.0".to_string(),
            client_flavor: "WoW Forever".to_string(),
            wow_path: "A:\\Blizzard\\World of Warcraft\\_forever_".to_string(),
            os_info: "Windows 11 x86_64".to_string(),
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: None,
            dialogue_text: None,
            attachment_path: None,
            timestamp_epoch: 1730928000,
        };

        let md = report.to_markdown();
        assert!(md.contains("# [Launcher] Crash o chiusura improvvisa: Crash durante scansione BLAKE3"));
        assert!(md.contains("Vivavoce-Launcher"));
        assert!(md.contains("BLAKE3 Parallel Delta Scanner"));
    }

    #[test]
    fn test_gender_mismatch_category() {
        let cat = BugCategory::from_str_lenient("Sesso o voce errata (NPC femmina con voce maschile)");
        assert_eq!(cat, BugCategory::GenderMismatch);
        assert_eq!(cat.as_str(), "Voce o sesso errato (M/F)");
    }

    #[test]
    fn test_missing_voice_category() {
        let cat = BugCategory::from_str_lenient("Voce mancante (battuta non doppiata)");
        assert_eq!(cat, BugCategory::MissingVoice);
        assert_eq!(cat.as_str(), "Voce mancante (non doppiata)");
    }
}
