use serde::{Deserialize, Serialize};

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
    pub category: BugCategory,
    pub addon_version: String,
    pub client_flavor: String,
    pub wow_path: String,
    pub sound_file: Option<String>,
    pub quest_id: Option<u32>,
    pub npc_id: Option<u32>,
    pub dialogue_title: Option<String>,
    pub dialogue_text: Option<String>,
    pub user_description: String,
    pub timestamp_epoch: u64,
}

impl BugReport {
    pub fn to_markdown(&self) -> String {
        let mut md = String::with_capacity(1024);
        md.push_str("# VivaVoce - Segnalazione Problema In-Game\n\n");
        md.push_str(&format!("- **Data/Ora (Epoch)**: {}\n", self.timestamp_epoch));
        md.push_str(&format!("- **Categoria**: {}\n", self.category.as_str()));
        md.push_str(&format!("- **Versione AddOn**: {}\n", self.addon_version));
        md.push_str(&format!("- **Client WoW**: {}\n", self.client_flavor));
        md.push_str(&format!("- **Percorso Installazione**: {}\n\n", self.wow_path));

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

        md.push_str("\n---\n*Generato automaticamente da VivaVoce Launcher Native (Rust)*\n");
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_lenient_parsing() {
        assert_eq!(BugCategory::from_str_lenient("pronuncia errata"), BugCategory::Pronunciation);
        assert_eq!(BugCategory::from_str_lenient("volume troppo basso"), BugCategory::VolumeOrQuality);
        assert_eq!(BugCategory::from_str_lenient("la frase è tagliata"), BugCategory::CutOrGlitch);
        assert_eq!(BugCategory::from_str_lenient("errore lua assert"), BugCategory::LuaError);
        assert_eq!(BugCategory::from_str_lenient("qualcos'altro"), BugCategory::Other);
    }

    #[test]
    fn test_report_markdown_formatting() {
        let report = BugReport {
            category: BugCategory::Pronunciation,
            addon_version: "1.0.0".to_string(),
            client_flavor: "_forever_".to_string(),
            wow_path: "A:\\Blizzard\\World of Warcraft".to_string(),
            sound_file: Some("Sounds/16/16013_q123_4.ogg".to_string()),
            quest_id: Some(123),
            npc_id: Some(16013),
            dialogue_title: Some("Il destino di Ironforge".to_string()),
            dialogue_text: Some("Benvenuto viaggiatore!".to_string()),
            user_description: "L'accento sulla seconda parola è sbagliato".to_string(),
            timestamp_epoch: 1730928000,
        };

        let md = report.to_markdown();
        assert!(md.contains("Sounds/16/16013_q123_4.ogg"));
        assert!(md.contains("Quest ID**: `123`"));
        assert!(md.contains("16013"));
        assert!(md.contains("L'accento sulla seconda parola è sbagliato"));
    }
}
