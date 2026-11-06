use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::engine::scanner::VIVAVOCE_MANAGED_MODULES;

/// Results of a complete uninstallation operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallResult {
    /// Paths to VivaVoce addon directories that were safely deleted.
    pub removed_addon_dirs: Vec<PathBuf>,
    /// Paths to SavedVariables files that were deleted (if requested).
    pub removed_saved_variables: Vec<PathBuf>,
    /// Total disk space in bytes freed by the operation.
    pub freed_bytes: u64,
}

/// Detects which VivaVoce modules are currently installed inside the AddOns directory.
pub fn detect_installed_modules<P: AsRef<Path>>(addons_dir: P) -> Vec<String> {
    let addons_dir = addons_dir.as_ref();
    let mut detected = Vec::new();

    for &module_name in VIVAVOCE_MANAGED_MODULES {
        let dir = addons_dir.join(module_name);
        if dir.is_dir() {
            detected.push(module_name.to_string());
        }
    }

    detected
}

/// Recursively calculates the total disk space in bytes used by a directory.
pub fn calculate_dir_size<P: AsRef<Path>>(dir: P) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Recursively unsets readonly attributes and removes a directory tree cleanly.
fn safe_remove_dir_all<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(());
    }
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if let Ok(metadata) = entry.metadata() {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(entry.path(), permissions);
            }
        }
    }
    std::fs::remove_dir_all(path)
}

/// Uninstalls VivaVoce addon folders from the given `Interface/AddOns` directory.
///
/// SAFETY GUARANTEE:
/// Strictly validates directory basenames against `VIVAVOCE_MANAGED_MODULES`.
/// Never deletes third-party addons (Details, ElvUI, WeakAuras, Questie, etc.)
/// or parent directories.
pub fn uninstall_addons_only<P: AsRef<Path>>(addons_dir: P) -> std::io::Result<UninstallResult> {
    let addons_dir = addons_dir.as_ref();
    let mut result = UninstallResult::default();

    if !addons_dir.exists() {
        return Ok(result);
    }

    for &module_name in VIVAVOCE_MANAGED_MODULES {
        let target_dir = addons_dir.join(module_name);

        // Security check: ensure target_dir basename is strictly an allowed module
        if let Some(folder_name) = target_dir.file_name().and_then(|n| n.to_str()) {
            if !VIVAVOCE_MANAGED_MODULES.contains(&folder_name) {
                continue;
            }
        } else {
            continue;
        }

        if target_dir.is_dir() {
            let size = calculate_dir_size(&target_dir);
            safe_remove_dir_all(&target_dir)?;
            result.removed_addon_dirs.push(target_dir);
            result.freed_bytes += size;
        }
    }

    Ok(result)
}

/// Performs a full uninstallation of VivaVoce given either:
/// 1. The root World of Warcraft directory (e.g. `C:\World of Warcraft\_retail_` or `_classic_era_`)
/// 2. Or directly the `Interface/AddOns` directory.
///
/// Removes managed VivaVoce addon folders and optionally removes SavedVariables.
pub fn uninstall_vivavoce<P: AsRef<Path>>(
    path: P,
    remove_saved_variables: bool,
) -> std::io::Result<UninstallResult> {
    let p = path.as_ref();
    let (wow_dir, addons_dir) = if p
        .file_name()
        .map_or(false, |n| n.to_string_lossy().eq_ignore_ascii_case("AddOns"))
    {
        // p is already Interface/AddOns
        let wow = p.parent().and_then(|p| p.parent()).unwrap_or(p);
        (wow.to_path_buf(), p.to_path_buf())
    } else {
        (p.to_path_buf(), p.join("Interface").join("AddOns"))
    };

    let mut result = uninstall_addons_only(&addons_dir)?;

    if remove_saved_variables {
        let wtf_account_dir = wow_dir.join("WTF").join("Account");
        if wtf_account_dir.is_dir() {
            for entry in WalkDir::new(&wtf_account_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
            {
                let file_name = entry.file_name().to_string_lossy();
                // Target SavedVariables: VivaVoce*.lua and VivaVoce*.lua.bak
                if (file_name.starts_with("VivaVoce") || file_name.starts_with("vivavoce"))
                    && (file_name.ends_with(".lua") || file_name.ends_with(".lua.bak"))
                {
                    let path = entry.into_path();
                    if let Ok(meta) = std::fs::metadata(&path) {
                        result.freed_bytes += meta.len();
                    }
                    if std::fs::remove_file(&path).is_ok() {
                        result.removed_saved_variables.push(path);
                    }
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uninstall_safely_removes_vivavoce_only() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let wow_dir = temp_dir.path();
        let addons_dir = wow_dir.join("Interface").join("AddOns");
        std::fs::create_dir_all(&addons_dir).unwrap();

        // 1. Create VivaVoce folders
        let vv_core = addons_dir.join("VivaVoce");
        let vv_quests = addons_dir.join("VivaVoce_Quests");
        std::fs::create_dir_all(&vv_core).unwrap();
        std::fs::create_dir_all(&vv_quests).unwrap();
        std::fs::write(vv_core.join("VivaVoce.toc"), b"## Title: VivaVoce").unwrap();
        std::fs::write(vv_quests.join("1.mp3"), b"1234567890").unwrap();

        // 2. Create third party addons that must NEVER be touched
        let elvui = addons_dir.join("ElvUI");
        let details = addons_dir.join("Details");
        std::fs::create_dir_all(&elvui).unwrap();
        std::fs::create_dir_all(&details).unwrap();
        std::fs::write(elvui.join("ElvUI.toc"), b"User Addon").unwrap();
        std::fs::write(details.join("Details.lua"), b"User Addon").unwrap();

        // 3. Create WTF SavedVariables
        let wtf_saved = wow_dir
            .join("WTF")
            .join("Account")
            .join("12345#1")
            .join("SavedVariables");
        std::fs::create_dir_all(&wtf_saved).unwrap();

        let vv_saved_lua = wtf_saved.join("VivaVoce.lua");
        let vv_saved_bak = wtf_saved.join("VivaVoce.lua.bak");
        let other_saved = wtf_saved.join("Details.lua");

        std::fs::write(&vv_saved_lua, b"VivaVoceDB = {}").unwrap();
        std::fs::write(&vv_saved_bak, b"VivaVoceDB = {}").unwrap();
        std::fs::write(&other_saved, b"DetailsDB = {}").unwrap();

        // Detect installed
        let detected = detect_installed_modules(&addons_dir);
        assert_eq!(detected.len(), 2);
        assert!(detected.contains(&"VivaVoce".to_string()));
        assert!(detected.contains(&"VivaVoce_Quests".to_string()));

        // Perform uninstall with remove_saved_variables = true
        let uninstall_res = uninstall_vivavoce(wow_dir, true).expect("Uninstall failed");

        assert_eq!(uninstall_res.removed_addon_dirs.len(), 2);
        assert_eq!(uninstall_res.removed_saved_variables.len(), 2);
        assert!(uninstall_res.freed_bytes > 0);

        // Verify VivaVoce folders are gone
        assert!(!vv_core.exists());
        assert!(!vv_quests.exists());
        assert!(!vv_saved_lua.exists());
        assert!(!vv_saved_bak.exists());

        // Verify third-party addons and savedvariables are PRESERVED
        assert!(elvui.exists());
        assert!(details.exists());
        assert!(other_saved.exists());
    }

    #[test]
    fn test_uninstall_directly_with_addons_dir() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let wow_dir = temp_dir.path();
        let addons_dir = wow_dir.join("Interface").join("AddOns");
        std::fs::create_dir_all(&addons_dir).unwrap();

        let vv_core = addons_dir.join("VivaVoce");
        let other_addon = addons_dir.join("QuestIT");
        std::fs::create_dir_all(&vv_core).unwrap();
        std::fs::create_dir_all(&other_addon).unwrap();
        std::fs::write(vv_core.join("Core.lua"), b"print(1)").unwrap();
        std::fs::write(other_addon.join("QuestIT.toc"), b"QuestIT").unwrap();

        // Pass addons_dir directly
        let res = uninstall_vivavoce(&addons_dir, false).expect("Uninstall directly with addons_dir");
        assert_eq!(res.removed_addon_dirs.len(), 1);
        assert!(!vv_core.exists());
        assert!(other_addon.exists());
    }
}
