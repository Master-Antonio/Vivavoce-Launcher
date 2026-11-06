//! World of Warcraft client discovery and SavedVariables parsing module.
//!
//! Provides automatic detection of WoW installations on Windows (via Windows Registry,
//! Battle.net Agent `product.db`, and known filesystem locations), game flavor management
//! (prioritizing `_forever_`), and robust fault-tolerant Lua SavedVariables parsing.

use std::path::PathBuf;

pub mod discovery;
pub mod saved_variables;

pub use discovery::{
    detect_wow_installations, ensure_addons_path, find_primary_wow_installation,
    find_wow_install_path, get_addons_path, validate_addons_path, validate_wow_root, WowFlavor,
    WowInstallation,
};
pub use saved_variables::{
    find_saved_variables_files, load_all_saved_variables, parse_saved_variables,
    parse_saved_variables_file, DiagnosticNote, RecentDialogue, SavedVariablesData,
};

/// Errors that can occur during WoW discovery or SavedVariables processing.
#[derive(Debug, thiserror::Error)]
pub enum WowError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Windows Registry error: {0}")]
    Registry(String),

    #[error("World of Warcraft installation not found")]
    NotFound,

    #[error("Invalid WoW installation at {path}: {reason}")]
    InvalidInstallation { path: PathBuf, reason: String },

    #[error("No valid flavor found in WoW directory: {0}")]
    NoFlavorFound(PathBuf),

    #[error("Flavor '{flavor}' not found in WoW directory: {wow_root}")]
    FlavorNotFound { wow_root: PathBuf, flavor: String },

    #[error("AddOns directory invalid or inaccessible: {0}")]
    InvalidAddonsPath(PathBuf),

    #[error("SavedVariables error: {0}")]
    SavedVariables(String),

    #[error("Lua parsing error: {0}")]
    ParseError(String),
}

/// Convenience result alias for WoW operations.
pub type Result<T> = std::result::Result<T, WowError>;
