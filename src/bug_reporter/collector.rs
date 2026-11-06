use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Audio,
    Addon,
}

impl IssueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Audio => "Audio / Doppiaggio",
            Self::Addon => "AddOn / Tecnico",
        }
    }

    pub fn github_label(&self) -> &'static str {
        match self {
            Self::Audio => "audio-bug",
            Self::Addon => "addon-bug",
        }
    }
}

// Kept for backward compatibility with existing tests and imports
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugCategory {
    Pronunciation,
    VolumeOrQuality,
    CutOrGlitch,
    TextMismatch,
    LuaError,
    Other,
}

impl BugCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pronunciation => "Pronuncia / Accento",
            Self::VolumeOrQuality => "Volume / Qualità Audio",
            Self::CutOrGlitch => "Audio tagliato / Glitch",
            Self::TextMismatch => "Testo e Voce non corrispondono",
            Self::LuaError => "Errore Lua / AddOn Crash",
            Self::Other => "Altro / Suggerimento",
        }
    }

    pub fn from_str_lenient(s: &str) -> Self {
        let lower = s.to_lowercase();
        if lower.contains("pronun") || lower.contains("accent") {
            Self::Pronunciation
        } else if lower.contains("volum") || lower.contains("qualit") || lower.contains("basso") {
            Self::VolumeOrQuality
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
                md.push_str("- **Launcher**: VivaVoce Native v0.1.0 (Rust + Slint)\n");
                md.push_str("- **Architettura**: 6 componenti modulari integrati\n");
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
            client_flavor: "Forever 1.12.1".to_string(),
            wow_path: "A:\\WoW".to_string(),
            os_info: "Windows 11".to_string(),
            sound_file: Some("Sounds/16/16013_q123_4.ogg".to_string()),
            quest_id: Some(123),
            npc_id: Some(16013),
            dialogue_title: Some("Il destino di Ironforge".to_string()),
            dialogue_text: Some("Benvenuto viaggiatore!".to_string()),
            timestamp_epoch: 1730928000,
        };

        let md = report.to_markdown();
        assert!(md.contains("# [Audio] Pronuncia ed enfasi: Il destino di Ironforge"));
        assert!(md.contains("Sounds/16/16013_q123_4.ogg"));
        assert!(md.contains("16013"));
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
            client_flavor: "Forever 1.12.1".to_string(),
            wow_path: "A:\\WoW".to_string(),
            os_info: "Windows 11".to_string(),
            sound_file: None,
            quest_id: None,
            npc_id: None,
            dialogue_title: None,
            dialogue_text: None,
            timestamp_epoch: 1730928000,
        };

        let md = report.to_markdown();
        assert!(md.contains("# [Addon] Errore Lua in gioco: Crash apertura QuestLog"));
        assert!(md.contains("```lua"));
        assert!(md.contains("Passaggi per Riprodurre"));
    }
}
