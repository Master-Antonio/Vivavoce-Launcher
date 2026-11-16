use std::fs::{create_dir_all, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Persistent launcher configuration saved across sessions.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct LauncherConfig {
    /// Custom WoW installation directory explicitly selected or confirmed by the user.
    pub custom_wow_path: Option<PathBuf>,
    /// Selected WoW flavor name (e.g. "Forever", "ClassicEra").
    pub selected_flavor: Option<String>,
    /// Last confirmed installed VivaVoce version.
    pub last_known_version: Option<String>,
    /// Active theme index or name.
    pub active_theme: Option<String>,
}

impl LauncherConfig {
    /// Determines the standard config file path based on the operating system:
    /// - Windows: `%APPDATA%\VivaVoce\launcher_config.json`
    /// - macOS: `~/Library/Application Support/VivaVoce/launcher_config.json`
    /// - Linux: `$XDG_CONFIG_HOME/vivavoce/launcher_config.json` or `~/.config/vivavoce/launcher_config.json`
    pub fn config_path() -> PathBuf {
        let base_dir = if cfg!(target_os = "windows") {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("VivaVoce")
        } else if cfg!(target_os = "macos") {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join("Library").join("Application Support").join("VivaVoce")
        } else {
            // Linux and other Unix
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let home = std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    home.join(".config")
                });
            base.join("vivavoce")
        };

        base_dir.join("launcher_config.json")
    }

    /// Loads the configuration from disk, returning default if absent or corrupted.
    pub fn load() -> Self {
        let path = Self::config_path();
        if !path.exists() {
            return Self::default();
        }

        match File::open(&path) {
            Ok(mut file) => {
                let mut content = String::new();
                if file.read_to_string(&mut content).is_ok() {
                    match serde_json::from_str::<LauncherConfig>(&content) {
                        Ok(config) => {
                            info!("Configurazione caricata da {:?}", path);
                            return config;
                        }
                        Err(e) => {
                            warn!("Errore nel parsing di {:?}: {}, uso configurazione predefinita", path, e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Impossibile aprire {:?}: {}", path, e);
            }
        }

        Self::default()
    }

    /// Saves the current configuration to disk.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let mut file = File::create(&path)?;
        file.write_all(json.as_bytes())?;
        file.flush()?;
        info!("Configurazione salvata con successo in {:?}", path);
        Ok(())
    }

    /// Helper to update and persist a newly selected WoW path.
    pub fn update_custom_path<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let mut config = Self::load();
        config.custom_wow_path = Some(path.as_ref().to_path_buf());
        config.save()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_serialization() {
        let mut config = LauncherConfig::default();
        config.custom_wow_path = Some(PathBuf::from("/Games/World of Warcraft"));
        config.selected_flavor = Some("Forever".to_string());
        config.last_known_version = Some("0.5.2".to_string());

        let json = serde_json::to_string(&config).unwrap();
        let loaded: LauncherConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, loaded);
    }
}
