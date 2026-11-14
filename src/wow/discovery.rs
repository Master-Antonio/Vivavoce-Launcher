//! WoW client installation auto-discovery and flavor detection.
//!
//! Provides registry scanning on Windows, Battle.net `product.db` parsing,
//! common known folder checks, and flavor path validation (with `_forever_` prioritized).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::wow::{Result, WowError};

/// Recognized World of Warcraft flavors ordered by priority.
/// `Forever` is the top priority for World of Warcraft: Forever!
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum WowFlavor {
    /// World of Warcraft: Forever client (Priority 1)
    Forever = 0,
    /// WoW Classic Era (1.14 / 1.15 vanilla)
    ClassicEra = 1,
    /// WoW Classic (Progression / WotLK / Cataclysm)
    Classic = 2,
    /// World of Warcraft modern Retail
    Retail = 3,
}

impl WowFlavor {
    /// All flavors in order of priority.
    pub const ALL: [WowFlavor; 4] = [
        WowFlavor::Forever,
        WowFlavor::ClassicEra,
        WowFlavor::Classic,
        WowFlavor::Retail,
    ];

    /// Subdirectory name under the WoW installation root.
    #[must_use]
    pub const fn as_dir_name(&self) -> &'static str {
        match self {
            Self::Forever => "_forever_",
            Self::ClassicEra => "_classic_era_",
            Self::Classic => "_classic_",
            Self::Retail => "_retail_",
        }
    }

    /// User-friendly display name.
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Forever => "WoW Forever",
            Self::ClassicEra => "WoW Classic Era",
            Self::Classic => "WoW Classic",
            Self::Retail => "World of Warcraft Retail",
        }
    }

    /// Parse a flavor from directory or folder name (case-insensitive).
    #[must_use]
    pub fn from_dir_name(name: &str) -> Option<Self> {
        let trimmed = name.trim().trim_matches('/');
        let lower = trimmed.to_ascii_lowercase();

        if lower == "_forever_" || lower == "forever" {
            Some(Self::Forever)
        } else if lower == "_classic_era_" || lower == "classic_era" {
            Some(Self::ClassicEra)
        } else if lower == "_classic_" || lower == "classic" {
            Some(Self::Classic)
        } else if lower == "_retail_" || lower == "retail" {
            Some(Self::Retail)
        } else {
            None
        }
    }

    /// Priority rank (lower number = higher priority).
    #[must_use]
    pub const fn priority(&self) -> u8 {
        *self as u8
    }
}

impl std::fmt::Display for WowFlavor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

/// Represents a detected World of Warcraft installation on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WowInstallation {
    /// Root path of the World of Warcraft directory (e.g. `C:\Games\World of Warcraft`)
    pub root_path: PathBuf,
    /// Flavors detected within this installation, ordered by priority
    pub flavors: Vec<WowFlavor>,
    /// Primary/preferred flavor (first in priority list, e.g. `Forever` if present)
    pub primary_flavor: Option<WowFlavor>,
}

impl WowInstallation {
    /// Creates a new `WowInstallation` with sorted flavors and primary flavor set.
    #[must_use]
    pub fn new(root_path: PathBuf, mut flavors: Vec<WowFlavor>) -> Self {
        flavors.sort_by_key(|f| f.priority());
        flavors.dedup();
        let primary_flavor = flavors.first().copied();
        Self {
            root_path,
            flavors,
            primary_flavor,
        }
    }

    /// Returns whether this installation contains the given flavor.
    #[must_use]
    pub fn has_flavor(&self, flavor: WowFlavor) -> bool {
        self.flavors.contains(&flavor)
    }

    /// Gets the AddOns directory path for the given flavor.
    #[must_use]
    pub fn addons_path(&self, flavor: WowFlavor) -> PathBuf {
        get_addons_path(&self.root_path, flavor)
    }

    /// Gets the AddOns directory path for the primary flavor, if one is detected.
    #[must_use]
    pub fn primary_addons_path(&self) -> Option<PathBuf> {
        self.primary_flavor.map(|f| self.addons_path(f))
    }

    /// Returns the exact game client folder for the primary flavor.
    /// E.g. if root is `A:\Blizzard\World of Warcraft` and flavor is `Forever`,
    /// this returns `A:\Blizzard\World of Warcraft\_forever_` (or `forever`).
    #[must_use]
    pub fn game_client_path(&self) -> PathBuf {
        let root = resolve_candidate_root(&self.root_path);
        let flavor = self.primary_flavor.unwrap_or(WowFlavor::Forever);
        if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
            if let Some(existing_flavor) = WowFlavor::from_dir_name(name) {
                if existing_flavor == flavor {
                    return root;
                }
            }
        }
        if let Some(flavor_dir_underscored) = find_child_ignore_case(&root, flavor.as_dir_name()) {
            if flavor_dir_underscored.is_dir() {
                return flavor_dir_underscored;
            }
        }
        let plain_name = match flavor {
            WowFlavor::Forever => "forever",
            WowFlavor::ClassicEra => "classic_era",
            WowFlavor::Classic => "classic",
            WowFlavor::Retail => "retail",
        };
        if let Some(flavor_dir_plain) = find_child_ignore_case(&root, plain_name) {
            if flavor_dir_plain.is_dir() {
                return flavor_dir_plain;
            }
        }
        root
    }
}

/// Helper to find an immediate child path ignoring ASCII casing.
/// Critical on Linux / Steam Deck ext4 file systems accessed via Proton / Wine.
pub fn find_child_ignore_case(dir: &Path, target_name: &str) -> Option<PathBuf> {
    let direct = dir.join(target_name);
    if direct.exists() {
        return Some(direct);
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let target_lower = target_name.to_ascii_lowercase();
        for entry in entries.flatten() {
            if let Ok(file_name) = entry.file_name().into_string() {
                if file_name.to_ascii_lowercase() == target_lower {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

/// Helper to scan a directory for WoW game executables or app bundles (case-insensitive).
pub fn find_wow_executable_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    let known_names = [
        "wow.exe",
        "wowclassic.exe",
        "world of warcraft.exe",
        "world of warcraft launcher.exe",
        "forever.exe",
        "turtle.exe",
        "vanilla.exe",
        "world of warcraft.app",
        "wow.app",
        "world of warcraft-64.app",
        "world of warcraft classic.app",
    ];
    for name in &known_names {
        if let Some(found) = find_child_ignore_case(dir, name) {
            return Some(found);
        }
    }
    // Also check any .exe containing "wow" or "warcraft"
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    let lower = name.to_ascii_lowercase();
                    if lower.ends_with(".exe") && (lower.contains("wow") || lower.contains("warcraft")) {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

/// Checks if a folder directly contains WoW installation markers (flavors, Interface, or binaries).
pub fn is_direct_wow_folder(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    for flavor in WowFlavor::ALL {
        if let Some(flavor_dir) = find_child_ignore_case(dir, flavor.as_dir_name()) {
            if flavor_dir.is_dir() {
                return true;
            }
        }
        let plain = match flavor {
            WowFlavor::Forever => "forever",
            WowFlavor::ClassicEra => "classic_era",
            WowFlavor::Classic => "classic",
            WowFlavor::Retail => "retail",
        };
        if let Some(flavor_dir) = find_child_ignore_case(dir, plain) {
            if flavor_dir.is_dir() {
                return true;
            }
        }
    }
    if find_wow_executable_in_dir(dir).is_some() {
        return true;
    }
    let has_interface = find_child_ignore_case(dir, "Interface").map_or(false, |p| p.is_dir());
    let has_data = find_child_ignore_case(dir, "Data").map_or(false, |p| p.is_dir());
    has_interface || has_data
}

/// Finds the runnable game executable or app bundle for an installation (case-insensitive).
pub fn find_game_executable(root: &Path, flavor: WowFlavor) -> Option<PathBuf> {
    let resolved = resolve_candidate_root(root);

    // Try flavor folder first (_forever_ or forever), then root
    let candidates = [
        resolved.join(flavor.as_dir_name()),
        resolved.join(match flavor {
            WowFlavor::Forever => "forever",
            WowFlavor::ClassicEra => "classic_era",
            WowFlavor::Classic => "classic",
            WowFlavor::Retail => "retail",
        }),
        resolved.clone(),
    ];

    for candidate_dir in &candidates {
        if let Some(exe) = find_wow_executable_in_dir(candidate_dir) {
            return Some(exe);
        }
    }

    None
}

/// Resolves a candidate directory by:
/// 1. Unwrapping files (e.g. `WoW.exe`) to their parent directory.
/// 2. Unwrapping trailing `Interface` or `Interface/AddOns`.
/// 3. Resolving parent folders (e.g. `Games`) if they directly contain a `World of Warcraft` installation.
#[must_use]
pub fn resolve_candidate_root(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();

    // 0. If it's a file, take parent directory
    if current.is_file() {
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        }
    }

    // 1. Unwrap Interface or Interface/AddOns
    if let Some(name) = current.file_name().and_then(|n| n.to_str()) {
        if name.eq_ignore_ascii_case("AddOns") {
            if let Some(parent) = current.parent() {
                if parent.file_name().and_then(|n| n.to_str()).map_or(false, |p| p.eq_ignore_ascii_case("Interface")) {
                    if let Some(grandparent) = parent.parent() {
                        current = grandparent.to_path_buf();
                    }
                }
            }
        } else if name.eq_ignore_ascii_case("Interface") {
            if let Some(parent) = current.parent() {
                current = parent.to_path_buf();
            }
        }
    }

    // 2. Smart auto-resolution: if the selected folder is NOT directly a WoW install (e.g. user selected "Games"),
    // check if it directly contains a typical WoW subfolder like "World of Warcraft", "WoW", etc.
    if !is_direct_wow_folder(&current) {
        let common_subfolders = [
            "World of Warcraft",
            "WoW",
            "WoW Forever",
            "World of Warcraft Forever",
            "forever",
            "_forever_",
        ];
        for sub in &common_subfolders {
            if let Some(child) = find_child_ignore_case(&current, sub) {
                if child.is_dir() && is_direct_wow_folder(&child) {
                    return child;
                }
            }
        }
    }

    current
}

/// Validates that `path` is a valid WoW root or flavor folder, returning available flavors.
pub fn validate_wow_root(raw_path: &Path) -> Result<Vec<WowFlavor>> {
    let path = resolve_candidate_root(raw_path);

    if !path.exists() {
        return Err(WowError::InvalidInstallation {
            path: raw_path.to_path_buf(),
            reason: "Directory does not exist".to_string(),
        });
    }

    if !path.is_dir() {
        return Err(WowError::InvalidInstallation {
            path: raw_path.to_path_buf(),
            reason: "Path is not a directory".to_string(),
        });
    }

    // Case 1: Check if the user passed a flavor directory directly (e.g. `.../_forever_` or `.../forever`)
    if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
        if let Some(flavor) = WowFlavor::from_dir_name(dir_name) {
            return Ok(vec![flavor]);
        }
    }

    // Case 2: Standard root directory, scan subdirectories for flavors (both `_forever_` and plain `forever`)
    let mut found_flavors = Vec::new();
    for flavor in WowFlavor::ALL {
        let has_underscored = find_child_ignore_case(&path, flavor.as_dir_name()).map_or(false, |p| p.is_dir());
        let plain_name = match flavor {
            WowFlavor::Forever => "forever",
            WowFlavor::ClassicEra => "classic_era",
            WowFlavor::Classic => "classic",
            WowFlavor::Retail => "retail",
        };
        let has_plain = find_child_ignore_case(&path, plain_name).map_or(false, |p| p.is_dir());

        if has_underscored || has_plain {
            found_flavors.push(flavor);
        }
    }

    if !found_flavors.is_empty() {
        found_flavors.sort_by_key(|f| f.priority());
        return Ok(found_flavors);
    }

    // Case 3: Directory has WoW files (e.g. Wow.exe, World of Warcraft.app, Data, Interface)
    let has_exe = find_wow_executable_in_dir(&path).is_some();
    let has_interface = find_child_ignore_case(&path, "Interface").map_or(false, |p| p.is_dir());
    let has_data = find_child_ignore_case(&path, "Data").map_or(false, |p| p.is_dir());

    if has_exe || (has_data && has_interface) || has_interface {
        // Assume default Forever client if Interface, Mac app or WoW binaries are in root
        return Ok(vec![WowFlavor::Forever]);
    }

    Err(WowError::NoFlavorFound(raw_path.to_path_buf()))
}

/// Builds the AddOns directory path for a given WoW root and flavor:
/// `<wow_root>/<flavor>/Interface/AddOns`
#[must_use]
pub fn get_addons_path(wow_root: &Path, flavor: WowFlavor) -> PathBuf {
    let root = resolve_candidate_root(wow_root);

    // If the path already points to a flavor folder (e.g. `forever` or `_forever_`), don't nest it again
    if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
        if let Some(existing_flavor) = WowFlavor::from_dir_name(name) {
            if existing_flavor == flavor {
                let interface_dir = find_child_ignore_case(&root, "Interface").unwrap_or_else(|| root.join("Interface"));
                let addons_dir = find_child_ignore_case(&interface_dir, "AddOns").unwrap_or_else(|| interface_dir.join("AddOns"));
                return addons_dir;
            }
        }
    }

    // Check if `_forever_` exists (case-insensitive)
    if let Some(underscored) = find_child_ignore_case(&root, flavor.as_dir_name()) {
        if underscored.is_dir() {
            let interface_dir = find_child_ignore_case(&underscored, "Interface").unwrap_or_else(|| underscored.join("Interface"));
            let addons_dir = find_child_ignore_case(&interface_dir, "AddOns").unwrap_or_else(|| interface_dir.join("AddOns"));
            return addons_dir;
        }
    }

    // Check if plain folder name (e.g. `forever`) exists (case-insensitive)
    let plain_name = match flavor {
        WowFlavor::Forever => "forever",
        WowFlavor::ClassicEra => "classic_era",
        WowFlavor::Classic => "classic",
        WowFlavor::Retail => "retail",
    };
    if let Some(plain_dir) = find_child_ignore_case(&root, plain_name) {
        if plain_dir.is_dir() {
            let interface_dir = find_child_ignore_case(&plain_dir, "Interface").unwrap_or_else(|| plain_dir.join("Interface"));
            let addons_dir = find_child_ignore_case(&interface_dir, "AddOns").unwrap_or_else(|| interface_dir.join("AddOns"));
            return addons_dir;
        }
    }

    // If root directly contains `Interface` (typical on WoW Forever Mac & PC clients)
    if let Some(direct_interface) = find_child_ignore_case(&root, "Interface") {
        if direct_interface.is_dir() {
            let addons_dir = find_child_ignore_case(&direct_interface, "AddOns").unwrap_or_else(|| direct_interface.join("AddOns"));
            return addons_dir;
        }
    }

    // Fallback default
    root.join(flavor.as_dir_name()).join("Interface").join("AddOns")
}

/// Validates that an AddOns path exists or can be accessed.
pub fn validate_addons_path(addons_path: &Path) -> Result<()> {
    if addons_path.exists() {
        if addons_path.is_dir() {
            Ok(())
        } else {
            Err(WowError::InvalidAddonsPath(addons_path.to_path_buf()))
        }
    } else {
        // Check if parent directory exists (Interface folder)
        if let Some(parent) = addons_path.parent() {
            if parent.exists() && parent.is_dir() {
                return Ok(());
            }
        }
        Err(WowError::InvalidAddonsPath(addons_path.to_path_buf()))
    }
}

/// Ensures that the AddOns directory `<wow_root>/<flavor>/Interface/AddOns` exists,
/// creating intermediate directories if needed.
pub fn ensure_addons_path(wow_root: &Path, flavor: WowFlavor) -> Result<PathBuf> {
    let addons_dir = get_addons_path(wow_root, flavor);
    if !addons_dir.exists() {
        std::fs::create_dir_all(&addons_dir)?;
    }
    Ok(addons_dir)
}

/// Detects all World of Warcraft installations present on the system.
///
/// Scans in the following order:
/// 0. Current working directory & Exe directory (vital for Steam Deck, portable setups, and custom shortcuts)
/// 1. Windows Registry (`HKLM\SOFTWARE\WOW6432Node\...` and `HKLM\SOFTWARE\...`)
/// 2. Battle.net Agent `product.db`
/// 3. Known common file paths across system drives (including Wine/Proton Z:\ drive and Steam Deck)
///
/// Returns a list of installations, each having detected flavors sorted by priority.
pub fn detect_wow_installations() -> Vec<WowInstallation> {
    let mut candidates = Vec::new();

    // 0. Current working directory & Exe directory
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            candidates.push(exe_dir.to_path_buf());
        }
    }

    // 1. Windows Registry
    candidates.extend(scan_windows_registry());

    // 2. Battle.net Agent product.db
    candidates.extend(scan_bnet_product_db());

    // 3. Known common paths
    candidates.extend(scan_common_paths());

    // Deduplicate and canonicalize
    let mut seen = HashSet::new();
    let mut installations = Vec::new();

    for path in candidates {
        let normalized = normalize_path(&path);
        let resolved = resolve_candidate_root(&normalized);
        let key = resolved.to_string_lossy().to_ascii_lowercase();

        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);

        if let Ok(flavors) = validate_wow_root(&resolved) {
            installations.push(WowInstallation::new(resolved, flavors));
        }
    }

    // Sort installations: put ones that contain `_forever_` first
    installations.sort_by(|a, b| {
        let a_has_forever = a.has_flavor(WowFlavor::Forever);
        let b_has_forever = b.has_flavor(WowFlavor::Forever);
        match (a_has_forever, b_has_forever) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.root_path.cmp(&b.root_path),
        }
    });

    installations
}

/// Finds the primary/best WoW installation (preferring installations with `_forever_`).
pub fn find_wow_install_path() -> Result<WowInstallation> {
    let installations = detect_wow_installations();
    installations.into_iter().next().ok_or(WowError::NotFound)
}

/// Alias for `find_wow_install_path()`.
pub fn find_primary_wow_installation() -> Result<WowInstallation> {
    find_wow_install_path()
}

// ---------------------------------------------------------------------------
// Internal Discovery Helpers
// ---------------------------------------------------------------------------

/// Normalizes path by resolving components, trimming trailing slashes, and removing quotes.
/// On Windows under Wine / Proton, converts native Linux paths like `/home/deck/...` to `Z:\home\deck\...`.
pub fn normalize_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let cleaned = s.trim().trim_matches('"').trim_matches('\'').trim_end_matches(['\\', '/']);

    #[cfg(target_os = "windows")]
    {
        if cleaned.starts_with('/') {
            let win_style = format!("Z:{}", cleaned.replace('/', "\\"));
            return PathBuf::from(win_style);
        }
        let cleaned = cleaned.replace('/', "\\");
        PathBuf::from(cleaned)
    }

    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from(cleaned)
    }
}

/// Scans Windows Registry for WoW InstallPath keys.
#[cfg(target_os = "windows")]
fn scan_windows_registry() -> Vec<PathBuf> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;

    let mut found = Vec::new();

    let subkeys = [
        "SOFTWARE\\WOW6432Node\\Blizzard Entertainment\\World of Warcraft",
        "SOFTWARE\\Blizzard Entertainment\\World of Warcraft",
    ];

    let hives = [
        (HKEY_LOCAL_MACHINE, "HKLM"),
        (HKEY_CURRENT_USER, "HKCU"),
    ];

    let flag_combinations = [
        0,
        KEY_WOW64_32KEY,
        KEY_WOW64_64KEY,
    ];

    for (hive, _) in hives {
        let root = RegKey::predef(hive);
        for subkey in subkeys {
            for flags in flag_combinations {
                if let Ok(key) = root.open_subkey_with_flags(subkey, KEY_READ | flags) {
                    // Try "InstallPath"
                    if let Ok(val) = key.get_value::<String, _>("InstallPath") {
                        let p = normalize_path(Path::new(&val));
                        if p.exists() {
                            tracing::info!("Found WoW install path via registry: {:?}", p);
                            found.push(p);
                        }
                    }
                    // Also check subkey "Path" if InstallPath is absent
                    if let Ok(val) = key.get_value::<String, _>("Path") {
                        let p = normalize_path(Path::new(&val));
                        if p.exists() {
                            found.push(p);
                        }
                    }
                }
            }
        }
    }

    found
}

#[cfg(not(target_os = "windows"))]
fn scan_windows_registry() -> Vec<PathBuf> {
    Vec::new()
}

/// Scans Battle.net Agent `product.db` for WoW paths.
fn scan_bnet_product_db() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut product_db_paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        product_db_paths.push(PathBuf::from("C:\\ProgramData\\Battle.net\\Agent\\product.db"));
        if let Ok(pd) = std::env::var("ProgramData") {
            product_db_paths.push(PathBuf::from(pd).join("Battle.net\\Agent\\product.db"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        product_db_paths.push(PathBuf::from("/Users/Shared/Battle.net/Agent/product.db"));
        if let Ok(home) = std::env::var("HOME") {
            product_db_paths.push(PathBuf::from(home).join("Library/Application Support/Battle.net/Agent/product.db"));
        }
    }

    for db_path in product_db_paths {
        if db_path.exists() && db_path.is_file() {
            if let Ok(bytes) = std::fs::read(&db_path) {
                let paths = parse_product_db_bytes(&bytes);
                candidates.extend(paths);
            }
        }
    }

    candidates
}

/// Scans a byte slice (from Battle.net `product.db`) extracting valid WoW root directory paths.
pub fn parse_product_db_bytes(bytes: &[u8]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let content = String::from_utf8_lossy(bytes);
    let lower_content = content.to_ascii_lowercase();

    let mut search_idx = 0;
    while let Some(wow_rel_idx) = lower_content[search_idx..].find("world of warcraft") {
        let wow_idx = search_idx + wow_rel_idx;
        let wow_end = wow_idx + "world of warcraft".len();

        // Trace backward to find the beginning of the path (Windows drive letter or Unix root /)
        let bytes_slice = content.as_bytes();
        let mut start_idx = wow_idx;
        while start_idx > 0 {
            let b = bytes_slice[start_idx - 1];
            if b < 0x20 || b == b'"' || b == b'\'' || b == 0 {
                break;
            }
            start_idx -= 1;
        }

        let raw_path = content[start_idx..wow_end].trim();
        let candidate = PathBuf::from(raw_path);
        if candidate.exists() && candidate.is_dir() {
            paths.push(candidate);
        }

        search_idx = wow_end;
    }

    paths.sort();
    paths.dedup();
    paths
}

/// Scans well-known fallback directories for WoW installations across Windows and macOS.
fn scan_common_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let static_paths = [
            "A:\\Blizzard\\World of Warcraft",
            "C:\\Program Files (x86)\\World of Warcraft",
            "C:\\Program Files\\World of Warcraft",
            "C:\\World of Warcraft",
            "C:\\Games\\World of Warcraft",
            "D:\\World of Warcraft",
            "D:\\Games\\World of Warcraft",
            "D:\\Blizzard\\World of Warcraft",
            "E:\\World of Warcraft",
            "E:\\Games\\World of Warcraft",
            "E:\\Blizzard\\World of Warcraft",
            "F:\\World of Warcraft",
            "F:\\Games\\World of Warcraft",
            "K:\\WoW",
            // Steam Deck & Proton (Wine drive Z:\ mapping native / filesystem)
            "Z:\\home\\deck\\Games\\World of Warcraft",
            "Z:\\home\\deck\\Games\\WoW",
            "Z:\\home\\deck\\Games\\WoW Forever",
            "Z:\\home\\deck\\World of Warcraft",
            "Z:\\home\\deck\\.local\\share\\Steam\\steamapps\\common\\World of Warcraft",
        ];

        for path_str in static_paths {
            let p = PathBuf::from(path_str);
            if p.exists() && p.is_dir() {
                paths.push(p);
            }
        }

        // Also scan common drive letters for typical root variations (including Z: for Wine)
        let drives = ['C', 'D', 'E', 'F', 'G', 'A', 'K', 'Z'];
        let folders = [
            "World of Warcraft",
            "Games\\World of Warcraft",
            "Blizzard\\World of Warcraft",
            "Battle.net\\World of Warcraft",
            "home\\deck\\Games\\World of Warcraft",
            "home\\deck\\Games\\WoW",
            "home\\deck\\World of Warcraft",
        ];

        for drive in drives {
            for folder in folders {
                let candidate = PathBuf::from(format!("{}:\\{}", drive, folder));
                if candidate.exists() && candidate.is_dir() {
                    paths.push(candidate);
                }
            }
        }

        // Scan Linux user home directories under Wine Z:\home\<user>\...
        let z_home = Path::new("Z:\\home");
        if z_home.exists() && z_home.is_dir() {
            if let Ok(entries) = std::fs::read_dir(z_home) {
                for entry in entries.flatten() {
                    let user_dir = entry.path();
                    if user_dir.is_dir() {
                        let user_targets = [
                            user_dir.join("Games").join("World of Warcraft"),
                            user_dir.join("Games").join("WoW"),
                            user_dir.join("Games").join("WoW Forever"),
                            user_dir.join("World of Warcraft"),
                            user_dir.join("WoW"),
                        ];
                        for target in user_targets {
                            if target.exists() && target.is_dir() {
                                paths.push(target);
                            }
                        }
                    }
                }
            }
        }

        // Scan Steam Deck removable MicroSD cards: Z:\run\media\deck\<card_id>\...
        let z_media = Path::new("Z:\\run\\media\\deck");
        if z_media.exists() && z_media.is_dir() {
            if let Ok(entries) = std::fs::read_dir(z_media) {
                for entry in entries.flatten() {
                    let card_dir = entry.path();
                    if card_dir.is_dir() {
                        let card_targets = [
                            card_dir.join("World of Warcraft"),
                            card_dir.join("Games").join("World of Warcraft"),
                            card_dir.join("WoW"),
                            card_dir.join("Games").join("WoW"),
                        ];
                        for target in card_targets {
                            if target.exists() && target.is_dir() {
                                paths.push(target);
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mac_paths = [
            "/Applications/World of Warcraft",
            "/Applications/World of Warcraft/forever",
            "/Applications/World of Warcraft/_forever_",
            "/Applications/World of Warcraft Classic",
            "/Applications/World of Warcraft: Forever",
            "/Applications/WoW",
            "/Applications/WoW Forever",
            "/Users/Shared/World of Warcraft",
            "/Users/Shared/World of Warcraft/forever",
        ];

        for path_str in mac_paths {
            let p = PathBuf::from(path_str);
            if p.exists() && p.is_dir() {
                paths.push(p);
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            let user_paths = [
                format!("{}/Applications/World of Warcraft", home),
                format!("{}/Applications/World of Warcraft/forever", home),
                format!("{}/Applications/WoW", home),
                format!("{}/Games/World of Warcraft", home),
                format!("{}/Games/World of Warcraft/forever", home),
                format!("{}/WoW", home),
            ];
            for path_str in user_paths {
                let p = PathBuf::from(path_str);
                if p.exists() && p.is_dir() {
                    paths.push(p);
                }
            }
        }
    }

    paths
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wow_flavor_dir_names_and_priority() {
        assert_eq!(WowFlavor::Forever.as_dir_name(), "_forever_");
        assert_eq!(WowFlavor::ClassicEra.as_dir_name(), "_classic_era_");
        assert_eq!(WowFlavor::Classic.as_dir_name(), "_classic_");
        assert_eq!(WowFlavor::Retail.as_dir_name(), "_retail_");

        assert!(WowFlavor::Forever.priority() < WowFlavor::ClassicEra.priority());
        assert!(WowFlavor::ClassicEra.priority() < WowFlavor::Classic.priority());
        assert!(WowFlavor::Classic.priority() < WowFlavor::Retail.priority());
    }

    #[test]
    fn test_wow_flavor_from_dir_name() {
        assert_eq!(WowFlavor::from_dir_name("_forever_"), Some(WowFlavor::Forever));
        assert_eq!(WowFlavor::from_dir_name("_FOREVER_"), Some(WowFlavor::Forever));
        assert_eq!(WowFlavor::from_dir_name("forever"), Some(WowFlavor::Forever));
        assert_eq!(WowFlavor::from_dir_name("_classic_era_"), Some(WowFlavor::ClassicEra));
        assert_eq!(WowFlavor::from_dir_name("_classic_"), Some(WowFlavor::Classic));
        assert_eq!(WowFlavor::from_dir_name("_retail_"), Some(WowFlavor::Retail));
        assert_eq!(WowFlavor::from_dir_name("unknown_dir"), None);
    }

    #[test]
    fn test_get_addons_path() {
        let wow_root = Path::new("Games").join("World of Warcraft");
        let path = get_addons_path(&wow_root, WowFlavor::Forever);
        assert_eq!(
            path,
            wow_root.join("_forever_").join("Interface").join("AddOns")
        );

        // If the wow_root passed is already the flavor dir
        let flavor_root = wow_root.join("_forever_");
        let path_direct = get_addons_path(&flavor_root, WowFlavor::Forever);
        assert_eq!(
            path_direct,
            flavor_root.join("Interface").join("AddOns")
        );
    }

    #[test]
    fn test_validate_and_ensure_addons_path() {
        let temp_dir = std::env::temp_dir().join(format!("vv_test_discovery_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        let forever_addons = get_addons_path(&temp_dir, WowFlavor::Forever);
        // Initially doesn't exist
        assert!(!forever_addons.exists());

        // Ensure creates it
        let created = ensure_addons_path(&temp_dir, WowFlavor::Forever).expect("Failed to ensure addons dir");
        assert_eq!(created, forever_addons);
        assert!(created.exists());
        assert!(created.is_dir());

        // Validate succeeds
        assert!(validate_addons_path(&created).is_ok());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_validate_wow_root_mock_prioritizes_forever() {
        let temp_dir = std::env::temp_dir().join(format!("vv_test_flavors_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Create both _retail_ and _forever_
        std::fs::create_dir_all(temp_dir.join("_retail_")).unwrap();
        std::fs::create_dir_all(temp_dir.join("_forever_")).unwrap();

        let flavors = validate_wow_root(&temp_dir).expect("Validation should succeed");
        assert_eq!(flavors.len(), 2);
        // Forever must be first due to priority sorting
        assert_eq!(flavors[0], WowFlavor::Forever);
        assert_eq!(flavors[1], WowFlavor::Retail);

        let installation = WowInstallation::new(temp_dir.clone(), flavors);
        assert_eq!(installation.primary_flavor, Some(WowFlavor::Forever));
        assert!(installation.has_flavor(WowFlavor::Forever));
        assert!(installation.has_flavor(WowFlavor::Retail));
        assert!(!installation.has_flavor(WowFlavor::Classic));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_parse_product_db_bytes() {
        let temp_wow = std::env::temp_dir().join(format!("vv_test_db_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_wow);
        std::fs::create_dir_all(&temp_wow).unwrap();

        // Simulate binary product.db with embedded path using platform separator
        let sep = std::path::MAIN_SEPARATOR;
        let mut path_str = temp_wow.to_string_lossy().to_string();
        if !path_str.ends_with(sep) {
            path_str.push(sep);
        }
        let simulated_payload = format!(
            "\x01\x02\x03\x04{}World of Warcraft  eu\x00\x01\x02",
            path_str
        );
        let wow_folder = temp_wow.join("World of Warcraft");
        std::fs::create_dir_all(&wow_folder).unwrap();

        let paths = parse_product_db_bytes(simulated_payload.as_bytes());
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], wow_folder);

        let _ = std::fs::remove_dir_all(&temp_wow);
    }

    #[test]
    fn test_live_system_detection() {
        let installations = detect_wow_installations();
        for inst in &installations {
            assert!(inst.root_path.exists());
            assert!(!inst.flavors.is_empty());
            println!("Found installation: {:?}", inst);
        }
    }

    #[test]
    fn test_resolve_candidate_root_interface_and_addons() {
        let root = PathBuf::from("/Applications/World of Warcraft/forever");
        let with_interface = root.join("Interface");
        let with_addons = root.join("Interface").join("AddOns");

        assert_eq!(resolve_candidate_root(&root), root);
        assert_eq!(resolve_candidate_root(&with_interface), root);
        assert_eq!(resolve_candidate_root(&with_addons), root);
    }

    #[test]
    fn test_mac_plain_forever_and_app_bundle() {
        let temp_dir = std::env::temp_dir().join(format!("vv_test_mac_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Simulate Mac path layout: root has "forever" folder (no underscores)
        let forever_folder = temp_dir.join("forever");
        std::fs::create_dir_all(forever_folder.join("Interface").join("AddOns")).unwrap();
        // Create dummy World of Warcraft.app bundle
        std::fs::create_dir_all(forever_folder.join("World of Warcraft.app")).unwrap();

        // 1. Validating root detects Forever flavor via "forever" subfolder
        let flavors = validate_wow_root(&temp_dir).expect("Should detect forever subfolder");
        assert_eq!(flavors, vec![WowFlavor::Forever]);

        // 2. get_addons_path points to `forever/Interface/AddOns`
        let addons = get_addons_path(&temp_dir, WowFlavor::Forever);
        assert_eq!(addons, forever_folder.join("Interface").join("AddOns"));

        // 3. User selects `.../forever/Interface` directly -> unwraps and detects
        let direct_interface = forever_folder.join("Interface");
        let flavors_from_interface = validate_wow_root(&direct_interface).expect("Should unwrap Interface");
        assert_eq!(flavors_from_interface, vec![WowFlavor::Forever]);
        let addons_from_interface = get_addons_path(&direct_interface, WowFlavor::Forever);
        assert_eq!(addons_from_interface, forever_folder.join("Interface").join("AddOns"));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_normalize_linux_path_under_windows() {
        let linux_path = Path::new("/home/deck/Games/World of Warcraft");
        let normalized = normalize_path(linux_path);
        #[cfg(target_os = "windows")]
        assert_eq!(normalized, PathBuf::from("Z:\\home\\deck\\Games\\World of Warcraft"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(normalized, PathBuf::from("/home/deck/Games/World of Warcraft"));
    }

    #[test]
    fn test_resolve_candidate_root_from_file_and_subfolder() {
        let temp_dir = std::env::temp_dir().join(format!("vv_test_subfolder_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        let wow_folder = temp_dir.join("World of Warcraft");
        std::fs::create_dir_all(wow_folder.join("Interface")).unwrap();
        let exe_file = wow_folder.join("WoW.exe");
        std::fs::write(&exe_file, b"dummy exe").unwrap();

        // 1. Pass the executable file itself -> unwraps to parent folder
        assert_eq!(resolve_candidate_root(&exe_file), wow_folder);

        // 2. Pass the parent "temp_dir" -> smart resolution should detect "World of Warcraft" inside it!
        assert_eq!(resolve_candidate_root(&temp_dir), wow_folder);

        // 3. Validation on parent folder succeeds because it resolves to child WoW install
        let flavors = validate_wow_root(&temp_dir).expect("Should resolve child WoW installation");
        assert_eq!(flavors, vec![WowFlavor::Forever]);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
