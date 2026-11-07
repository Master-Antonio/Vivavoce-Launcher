//! SavedVariables file locator and fault-tolerant Lua parser.
//!
//! Locates and parses `WTF/Account/<ACCOUNT>/SavedVariables/VivaVoce.lua` files to extract
//! recent dialogue playback history (`recentPlays` or `recentEvents`), diagnostic traces,
//! and reported errors.

use std::path::{Path, PathBuf};

use crate::wow::{Result, WowError};

/// A strongly-typed representation of an in-game dialogue line played by VivaVoce.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecentDialogue {
    /// Relative sound file path (e.g. "Sounds/16/16013_q123_4.ogg")
    pub sound_file: String,
    /// Associated quest identifier, if this dialogue is quest-bound
    pub quest_id: Option<u32>,
    /// Associated NPC creature identifier, if spoken by an NPC
    pub npc_id: Option<u32>,
    /// Dialogue title, quest name, or speaker header
    pub title: String,
    /// Full transcript text of the dialogue in Italian
    pub text: String,
    /// Epoch timestamp (in seconds) of when the dialogue was played
    pub timestamp: u64,
    /// Audio duration in seconds, if known
    pub sound_length: Option<f32>,
}

/// A diagnostic entry logged by the VivaVoce in-game addon.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticNote {
    /// Diagnostic description or trace message
    pub message: String,
    /// Optional epoch timestamp
    pub timestamp: Option<u64>,
}

/// Parsed data extracted from a VivaVoce SavedVariables file.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedVariablesData {
    /// WoW account name (e.g. "425836249#1")
    pub account_name: String,
    /// Path to the SavedVariables file on disk
    pub file_path: PathBuf,
    /// Chronological list of recently played dialogues
    pub recent_dialogues: Vec<RecentDialogue>,
    /// Addon diagnostic notes and status traces
    pub diagnostics: Vec<DiagnosticNote>,
    /// Error messages recorded by the addon
    pub errors: Vec<String>,
}

/// Finds all VivaVoce SavedVariables files within the given WoW root, flavor, or WTF directory.
///
/// Looks for `WTF/Account/<ACCOUNT>/SavedVariables/VivaVoce.lua` (and `VivaVoceDB.lua`).
#[must_use]
pub fn find_saved_variables_files(search_root: &Path) -> Vec<PathBuf> {
    if search_root.is_file() {
        if let Some(file_name) = search_root.file_name().and_then(|n| n.to_str()) {
            let lower = file_name.to_ascii_lowercase();
            if lower == "vivavoce.lua" || lower == "vivavocedb.lua" {
                return vec![search_root.to_path_buf()];
            }
        }
    }

    let mut found = Vec::new();

    // Recursively walk directory looking for VivaVoce.lua / VivaVoceDB.lua under SavedVariables/
    for entry in walkdir::WalkDir::new(search_root)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_map(std::result::Result::ok)
    {
        if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy();
            let lower = name.to_ascii_lowercase();

            if lower == "vivavoce.lua" || lower == "vivavocedb.lua" {
                // Verify parent folder is "SavedVariables"
                if let Some(parent) = entry.path().parent() {
                    let parent_name = parent.file_name().unwrap_or_default().to_string_lossy();
                    if parent_name.eq_ignore_ascii_case("SavedVariables") {
                        found.push(entry.path().to_path_buf());
                    }
                }
            }
        }
    }

    found.sort();
    found.dedup();
    found
}

/// Extracts the account name from a SavedVariables file path.
/// E.g. `.../WTF/Account/425836249#1/SavedVariables/VivaVoce.lua` -> `"425836249#1"`.
#[must_use]
pub fn extract_account_name(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let parts: Vec<&str> = raw
        .split(|c| c == '/' || c == '\\')
        .filter(|p| !p.is_empty())
        .collect();

    for window in parts.windows(2) {
        if window[0].eq_ignore_ascii_case("Account") {
            return window[1].to_string();
        }
    }

    "Default".to_string()
}

/// Reads and parses a SavedVariables file from disk.
pub fn parse_saved_variables_file(file_path: &Path) -> Result<SavedVariablesData> {
    if !file_path.exists() {
        return Err(WowError::SavedVariables(format!(
            "File does not exist: {}",
            file_path.display()
        )));
    }

    let content = std::fs::read_to_string(file_path)?;
    let account = extract_account_name(file_path);
    parse_saved_variables(&content, &account, file_path.to_path_buf())
}

/// Loads and parses all VivaVoce SavedVariables found in the given search directory.
#[must_use]
pub fn load_all_saved_variables(search_root: &Path) -> Vec<SavedVariablesData> {
    let files = find_saved_variables_files(search_root);
    let mut results = Vec::new();

    for file in files {
        match parse_saved_variables_file(&file) {
            Ok(data) => results.push(data),
            Err(e) => {
                tracing::warn!("Failed parsing SavedVariables at {:?}: {}", file, e);
            }
        }
    }

    results
}

/// Parses the contents of a SavedVariables Lua file into strongly typed `SavedVariablesData`.
pub fn parse_saved_variables(
    content: &str,
    account_name: &str,
    file_path: PathBuf,
) -> Result<SavedVariablesData> {
    let root_values = parse_lua_root(content)?;

    let mut all_dialogues = Vec::new();
    let mut all_diagnostics = Vec::new();
    let mut all_errors = Vec::new();

    // Iterate all root tables (e.g. `VivaVoceDB = { ... }`)
    for (_var_name, val) in &root_values {
        // Extract recentPlays / recentEvents
        let play_tables = val.find_tables_recursive(&[
            "recentplays",
            "recentevents",
            "history",
            "recent",
            "dialoguehistory",
        ]);

        for table in play_tables {
            if let LuaValue::Table(entries) = table {
                for (_k, entry_val) in entries {
                    if let LuaValue::Table(_) = entry_val {
                        if let Some(dialogue) = extract_dialogue(entry_val) {
                            all_dialogues.push(dialogue);
                        }
                    }
                }
            }
        }

        // Extract diagnostics
        let diag_notes = val.extract_diagnostics();
        all_diagnostics.extend(diag_notes);

        // Extract errors
        let error_msgs = val.extract_errors();
        all_errors.extend(error_msgs);
    }

    // Sort dialogues chronologically if timestamps are available
    all_dialogues.sort_by_key(|d| d.timestamp);
    all_dialogues.dedup();

    all_diagnostics.dedup();
    all_errors.dedup();

    Ok(SavedVariablesData {
        account_name: account_name.to_string(),
        file_path,
        recent_dialogues: all_dialogues,
        diagnostics: all_diagnostics,
        errors: all_errors,
    })
}

/// Extracts a single `RecentDialogue` from a table value.
fn extract_dialogue(val: &LuaValue) -> Option<RecentDialogue> {
    let sound_file = val
        .get_string_by_keys(&["sound_file", "soundfile", "sound", "file", "path"])
        .unwrap_or_default();

    let text = val
        .get_string_by_keys(&["text", "dialogue", "transcript", "subtitle", "message"])
        .unwrap_or_default();

    let title = val
        .get_string_by_keys(&["title", "questtitle", "header", "name"])
        .unwrap_or_default();

    // Need at least sound_file or text to be meaningful
    if sound_file.is_empty() && text.is_empty() {
        return None;
    }

    let quest_id = val.get_u32_by_keys(&["quest_id", "questid", "quest"]);
    let npc_id = val.get_u32_by_keys(&["npc_id", "npcid", "npc", "creature_id", "creatureid"]);
    let timestamp = val
        .get_u64_by_keys(&["timestamp", "time", "date", "ts"])
        .unwrap_or(0);
    let sound_length = val.get_f32_by_keys(&["sound_length", "soundlength", "length", "duration"]);

    Some(RecentDialogue {
        sound_file,
        quest_id,
        npc_id,
        title,
        text,
        timestamp,
        sound_length,
    })
}

// ---------------------------------------------------------------------------
// Lightweight, Fault-Tolerant Lua Table Parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum LuaValue {
    Nil,
    Boolean(bool),
    Number(f64),
    String(String),
    Table(Vec<(LuaKey, LuaValue)>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LuaKey {
    String(String),
    Integer(i64),
}

impl LuaValue {
    #[must_use]
    pub fn get_field(&self, key: &str) -> Option<&LuaValue> {
        match self {
            Self::Table(entries) => {
                for (k, v) in entries {
                    if let LuaKey::String(s) = k {
                        if s.eq_ignore_ascii_case(key) {
                            return Some(v);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    #[must_use]
    pub fn get_string_by_keys(&self, candidate_keys: &[&str]) -> Option<String> {
        for key in candidate_keys {
            if let Some(val) = self.get_field(key) {
                match val {
                    Self::String(s) => return Some(s.clone()),
                    Self::Number(n) => return Some(n.to_string()),
                    _ => {}
                }
            }
        }
        None
    }

    #[must_use]
    pub fn get_u32_by_keys(&self, candidate_keys: &[&str]) -> Option<u32> {
        for key in candidate_keys {
            if let Some(val) = self.get_field(key) {
                match val {
                    Self::Number(n) => return Some(*n as u32),
                    Self::String(s) => {
                        // Handle numeric strings like "16013" or prefixed like "16013 Thrall"
                        if let Some(first_word) = s.split_whitespace().next() {
                            if let Ok(num) = first_word.parse::<u32>() {
                                return Some(num);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    #[must_use]
    pub fn get_u64_by_keys(&self, candidate_keys: &[&str]) -> Option<u64> {
        for key in candidate_keys {
            if let Some(val) = self.get_field(key) {
                match val {
                    Self::Number(n) => return Some(*n as u64),
                    Self::String(s) => {
                        if let Ok(num) = s.trim().parse::<u64>() {
                            return Some(num);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    #[must_use]
    pub fn get_f32_by_keys(&self, candidate_keys: &[&str]) -> Option<f32> {
        for key in candidate_keys {
            if let Some(val) = self.get_field(key) {
                match val {
                    Self::Number(n) => return Some(*n as f32),
                    Self::String(s) => {
                        if let Ok(num) = s.trim().parse::<f32>() {
                            return Some(num);
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// Recursively finds all tables attached to keys matching any target name.
    #[must_use]
    pub fn find_tables_recursive<'a>(&'a self, target_keys: &[&str]) -> Vec<&'a LuaValue> {
        let mut results = Vec::new();
        self.collect_tables_recursive(target_keys, &mut results);
        results
    }

    fn collect_tables_recursive<'a>(&'a self, target_keys: &[&str], out: &mut Vec<&'a LuaValue>) {
        if let Self::Table(entries) = self {
            for (k, v) in entries {
                let matches_target = match k {
                    LuaKey::String(s) => target_keys.iter().any(|target| s.eq_ignore_ascii_case(target)),
                    _ => false,
                };

                if matches_target {
                    if let Self::Table(_) = v {
                        out.push(v);
                    }
                }

                // Recurse into child tables
                v.collect_tables_recursive(target_keys, out);
            }
        }
    }

    /// Extracts diagnostics and history strings/objects.
    #[must_use]
    pub fn extract_diagnostics(&self) -> Vec<DiagnosticNote> {
        let mut notes = Vec::new();
        let diag_keys = ["diagnostics", "_history", "_journey", "diagnosticnotes", "diag"];

        if let Self::Table(entries) = self {
            for (k, v) in entries {
                let is_diag_key = match k {
                    LuaKey::String(s) => diag_keys.iter().any(|dk| s.eq_ignore_ascii_case(dk)),
                    _ => false,
                };

                if is_diag_key {
                    match v {
                        Self::String(s) => {
                            notes.push(DiagnosticNote {
                                message: s.clone(),
                                timestamp: None,
                            });
                        }
                        Self::Table(sub_entries) => {
                            for (_sk, sv) in sub_entries {
                                match sv {
                                    Self::String(s) => {
                                        notes.push(DiagnosticNote {
                                            message: s.clone(),
                                            timestamp: None,
                                        });
                                    }
                                    Self::Table(_) => {
                                        let msg = sv
                                            .get_string_by_keys(&["message", "msg", "text", "note"])
                                            .unwrap_or_default();
                                        if !msg.is_empty() {
                                            let ts = sv.get_u64_by_keys(&["timestamp", "time", "date"]);
                                            notes.push(DiagnosticNote {
                                                message: msg,
                                                timestamp: ts,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // Recurse
                notes.extend(v.extract_diagnostics());
            }
        }

        notes
    }

    /// Extracts error strings from tables under keys like "errors", "errorlogs".
    #[must_use]
    pub fn extract_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let error_keys = ["errors", "errorlogs", "lasterrors", "error_list"];

        if let Self::Table(entries) = self {
            for (k, v) in entries {
                let is_error_key = match k {
                    LuaKey::String(s) => error_keys.iter().any(|ek| s.eq_ignore_ascii_case(ek)),
                    _ => false,
                };

                if is_error_key {
                    match v {
                        Self::String(s) => errors.push(s.clone()),
                        Self::Table(sub_entries) => {
                            for (_sk, sv) in sub_entries {
                                match sv {
                                    Self::String(s) => errors.push(s.clone()),
                                    Self::Table(_) => {
                                        if let Some(msg) = sv.get_string_by_keys(&["message", "error", "msg"]) {
                                            errors.push(msg);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }

                errors.extend(v.extract_errors());
            }
        }

        errors
    }
}

// ---------------------------------------------------------------------------
// Lexer and Token Stream
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    BraceOpen,
    BraceClose,
    BracketOpen,
    BracketClose,
    Equals,
    Comma,
    Semicolon,
    Ident(String),
    String(String),
    Number(f64),
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut idx = 0;

    while idx < len {
        let c = chars[idx];

        // Whitespace
        if c.is_whitespace() {
            idx += 1;
            continue;
        }

        // Comments: -- or --[[ ... ]]
        if c == '-' && idx + 1 < len && chars[idx + 1] == '-' {
            idx += 2;
            if idx + 1 < len && chars[idx] == '[' && chars[idx + 1] == '[' {
                idx += 2;
                // Block comment until ]]
                while idx + 1 < len && !(chars[idx] == ']' && chars[idx + 1] == ']') {
                    idx += 1;
                }
                idx = (idx + 2).min(len);
            } else {
                // Line comment until newline
                while idx < len && chars[idx] != '\n' {
                    idx += 1;
                }
            }
            continue;
        }

        // Multiline string: [[ ... ]]
        if c == '[' && idx + 1 < len && chars[idx + 1] == '[' {
            idx += 2;
            let mut s = String::new();
            while idx + 1 < len && !(chars[idx] == ']' && chars[idx + 1] == ']') {
                s.push(chars[idx]);
                idx += 1;
            }
            idx = (idx + 2).min(len);
            tokens.push(Token::String(s));
            continue;
        }

        // Single or double quoted strings
        if c == '"' || c == '\'' {
            let quote = c;
            idx += 1;
            let mut s = String::new();
            while idx < len && chars[idx] != quote {
                if chars[idx] == '\\' && idx + 1 < len {
                    idx += 1;
                    match chars[idx] {
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        '\\' => s.push('\\'),
                        '"' => s.push('"'),
                        '\'' => s.push('\''),
                        d if d.is_ascii_digit() => {
                            // Up to 3 decimal digits
                            let mut val = (d as u8 - b'0') as u32;
                            let mut count = 1;
                            while count < 3 && idx + 1 < len && chars[idx + 1].is_ascii_digit() {
                                idx += 1;
                                val = val * 10 + (chars[idx] as u8 - b'0') as u32;
                                count += 1;
                            }
                            if let Some(ch) = char::from_u32(val) {
                                s.push(ch);
                            }
                        }
                        other => s.push(other),
                    }
                } else {
                    s.push(chars[idx]);
                }
                idx += 1;
            }
            if idx < len && chars[idx] == quote {
                idx += 1;
            }
            tokens.push(Token::String(s));
            continue;
        }

        // Punctuation
        match c {
            '{' => {
                tokens.push(Token::BraceOpen);
                idx += 1;
                continue;
            }
            '}' => {
                tokens.push(Token::BraceClose);
                idx += 1;
                continue;
            }
            '[' => {
                tokens.push(Token::BracketOpen);
                idx += 1;
                continue;
            }
            ']' => {
                tokens.push(Token::BracketClose);
                idx += 1;
                continue;
            }
            '=' => {
                tokens.push(Token::Equals);
                idx += 1;
                continue;
            }
            ',' => {
                tokens.push(Token::Comma);
                idx += 1;
                continue;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                idx += 1;
                continue;
            }
            _ => {}
        }

        // Numbers: digits or leading minus followed by digit/dot
        let is_digit = c.is_ascii_digit();
        let is_neg_num = (c == '-' || c == '+')
            && idx + 1 < len
            && (chars[idx + 1].is_ascii_digit() || chars[idx + 1] == '.');

        if is_digit || is_neg_num {
            let start = idx;
            idx += 1;
            let mut has_dot = c == '.';
            let mut has_exp = false;

            while idx < len {
                let ch = chars[idx];
                if ch.is_ascii_digit() {
                    idx += 1;
                } else if ch == '.' && !has_dot {
                    has_dot = true;
                    idx += 1;
                } else if (ch == 'e' || ch == 'E') && !has_exp {
                    has_exp = true;
                    idx += 1;
                    if idx < len && (chars[idx] == '+' || chars[idx] == '-') {
                        idx += 1;
                    }
                } else if (ch == 'x' || ch == 'X') && idx == start + 1 && chars[start] == '0' {
                    idx += 1;
                    while idx < len && chars[idx].is_ascii_hexdigit() {
                        idx += 1;
                    }
                    break;
                } else {
                    break;
                }
            }

            let num_str: String = chars[start..idx].iter().collect();
            if num_str.starts_with("0x") || num_str.starts_with("0X") {
                if let Ok(n) = i64::from_str_radix(&num_str[2..], 16) {
                    tokens.push(Token::Number(n as f64));
                }
            } else if let Ok(n) = num_str.parse::<f64>() {
                tokens.push(Token::Number(n));
            }
            continue;
        }

        // Identifiers
        if c.is_alphabetic() || c == '_' {
            let start = idx;
            while idx < len && (chars[idx].is_alphanumeric() || chars[idx] == '_') {
                idx += 1;
            }
            let ident: String = chars[start..idx].iter().collect();
            tokens.push(Token::Ident(ident));
            continue;
        }

        // Unknown character, skip safely
        idx += 1;
    }

    tokens
}

fn parse_lua_root(content: &str) -> Result<Vec<(String, LuaValue)>> {
    let tokens = tokenize(content);
    let mut cursor = 0;
    let mut declarations = Vec::new();

    while cursor < tokens.len() {
        // Look for: Ident = Table / Value
        if let Some(Token::Ident(var_name)) = tokens.get(cursor) {
            if let Some(Token::Equals) = tokens.get(cursor + 1) {
                let name = var_name.clone();
                cursor += 2;
                if let Some(val) = parse_value(&tokens, &mut cursor) {
                    declarations.push((name, val));
                    continue;
                }
            }
        }
        cursor += 1;
    }

    Ok(declarations)
}

fn parse_value(tokens: &[Token], cursor: &mut usize) -> Option<LuaValue> {
    if *cursor >= tokens.len() {
        return None;
    }

    match &tokens[*cursor] {
        Token::Ident(id) => {
            *cursor += 1;
            match id.as_str() {
                "true" => Some(LuaValue::Boolean(true)),
                "false" => Some(LuaValue::Boolean(false)),
                "nil" => Some(LuaValue::Nil),
                _ => Some(LuaValue::String(id.clone())),
            }
        }
        Token::String(s) => {
            *cursor += 1;
            Some(LuaValue::String(s.clone()))
        }
        Token::Number(n) => {
            let val = *n;
            *cursor += 1;
            Some(LuaValue::Number(val))
        }
        Token::BraceOpen => parse_table(tokens, cursor),
        _ => {
            *cursor += 1;
            None
        }
    }
}

fn parse_table(tokens: &[Token], cursor: &mut usize) -> Option<LuaValue> {
    // Advance past '{'
    *cursor += 1;
    let mut entries = Vec::new();
    let mut auto_index = 1i64;

    while *cursor < tokens.len() {
        // Skip optional commas and semicolons
        while *cursor < tokens.len()
            && (tokens[*cursor] == Token::Comma || tokens[*cursor] == Token::Semicolon)
        {
            *cursor += 1;
        }

        if *cursor >= tokens.len() || tokens[*cursor] == Token::BraceClose {
            if *cursor < tokens.len() {
                *cursor += 1; // Eat '}'
            }
            break;
        }

        // Case A: [ <key> ] = <val>
        if tokens[*cursor] == Token::BracketOpen {
            *cursor += 1;
            let key = match tokens.get(*cursor) {
                Some(Token::String(s)) => {
                    *cursor += 1;
                    LuaKey::String(s.clone())
                }
                Some(Token::Number(n)) => {
                    *cursor += 1;
                    LuaKey::Integer(*n as i64)
                }
                Some(Token::Ident(id)) => {
                    *cursor += 1;
                    LuaKey::String(id.clone())
                }
                _ => {
                    *cursor += 1;
                    LuaKey::Integer(auto_index)
                }
            };

            // Expect BracketClose
            if *cursor < tokens.len() && tokens[*cursor] == Token::BracketClose {
                *cursor += 1;
            }
            // Expect Equals
            if *cursor < tokens.len() && tokens[*cursor] == Token::Equals {
                *cursor += 1;
            }

            if let Some(val) = parse_value(tokens, cursor) {
                entries.push((key, val));
            }
            continue;
        }

        // Case B: ident = <val>
        if let Some(Token::Ident(key_id)) = tokens.get(*cursor) {
            if let Some(Token::Equals) = tokens.get(*cursor + 1) {
                let key_name = key_id.clone();
                *cursor += 2;
                if let Some(val) = parse_value(tokens, cursor) {
                    entries.push((LuaKey::String(key_name), val));
                }
                continue;
            }
        }

        // Case C: Array element (unkeyed value)
        if let Some(val) = parse_value(tokens, cursor) {
            entries.push((LuaKey::Integer(auto_index), val));
            auto_index += 1;
            continue;
        }

        // Fallback: recover by advancing cursor safely
        *cursor += 1;
    }

    Some(LuaValue::Table(entries))
}

// ---------------------------------------------------------------------------
// Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_account_name() {
        let p1 = Path::new("A:\\WoW\\_forever_\\WTF\\Account\\425836249#1\\SavedVariables\\VivaVoce.lua");
        assert_eq!(extract_account_name(p1), "425836249#1");

        let p2 = Path::new("C:/World of Warcraft/_retail_/WTF/Account/MY_ACC/SavedVariables/VivaVoceDB.lua");
        assert_eq!(extract_account_name(p2), "MY_ACC");

        let p3 = Path::new("C:/Standalone/VivaVoce.lua");
        assert_eq!(extract_account_name(p3), "Default");
    }

    #[test]
    fn test_lua_tokenizer_and_nested_tables() {
        let lua = r#"
            VivaVoceDB = {
                ["profileKeys"] = {
                    ["Player - Realm"] = "Default",
                },
                ["profiles"] = {
                    ["Default"] = {
                        ["recentPlays"] = {
                            {
                                ["sound_file"] = "Sounds/16/16013_q123_4.ogg",
                                ["quest_id"] = 123,
                                ["npc_id"] = 456,
                                ["title"] = "A Threat In Favor",
                                ["text"] = "Greetings traveler! We need your help.",
                                ["timestamp"] = 1730928000,
                                ["sound_length"] = 4.25,
                            },
                        },
                        ["diagnostics"] = {
                            "Frame initialized",
                            "kind=npc sound=16013_q123_4.ogg",
                        },
                        ["errors"] = {
                            "SoundQueue callback: sound file missing",
                        },
                        ["_journey"] = "16013 | kind=npc sound=16013.ogg ok",
                    },
                },
            }
        "#;

        let result = parse_saved_variables(
            lua,
            "425836249#1",
            PathBuf::from("WTF/Account/425836249#1/SavedVariables/VivaVoce.lua"),
        )
        .expect("Should parse successfully");

        assert_eq!(result.account_name, "425836249#1");
        assert_eq!(result.recent_dialogues.len(), 1);

        let d = &result.recent_dialogues[0];
        assert_eq!(d.sound_file, "Sounds/16/16013_q123_4.ogg");
        assert_eq!(d.quest_id, Some(123));
        assert_eq!(d.npc_id, Some(456));
        assert_eq!(d.title, "A Threat In Favor");
        assert_eq!(d.text, "Greetings traveler! We need your help.");
        assert_eq!(d.timestamp, 1730928000);
        assert_eq!(d.sound_length, Some(4.25));

        // Diagnostics
        assert!(result.diagnostics.iter().any(|n| n.message == "Frame initialized"));
        assert!(result.diagnostics.iter().any(|n| n.message.contains("_journey") || n.message.contains("16013 | kind=npc")));

        // Errors
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0], "SoundQueue callback: sound file missing");
    }

    #[test]
    fn test_parse_flat_recent_events_format() {
        let lua = r#"
            VivaVoceDB = {
                ["recentEvents"] = {
                    {
                        soundFile = "Sounds/99/intro.ogg",
                        questId = 999,
                        npcId = "16013 Magni Bronzebeard",
                        title = "Welcome Champion",
                        text = "The world needs ye!",
                        timestamp = 1730928500,
                        duration = 5.5,
                    },
                },
                errors = {
                    "Some test warning",
                },
            }
        "#;

        let result = parse_saved_variables(lua, "Acc1", PathBuf::new()).unwrap();
        assert_eq!(result.recent_dialogues.len(), 1);
        let d = &result.recent_dialogues[0];
        assert_eq!(d.sound_file, "Sounds/99/intro.ogg");
        assert_eq!(d.quest_id, Some(999));
        assert_eq!(d.npc_id, Some(16013)); // parsed from "16013 Magni Bronzebeard"
        assert_eq!(d.title, "Welcome Champion");
        assert_eq!(d.text, "The world needs ye!");
        assert_eq!(d.timestamp, 1730928500);
        assert_eq!(d.sound_length, Some(5.5));
        assert_eq!(result.errors, vec!["Some test warning".to_string()]);
    }

    #[test]
    fn test_fault_tolerance_and_corrupted_tokens() {
        // Malformed Lua with broken tokens and stray syntax
        let corrupted_lua = r#"
            VivaVoceDB = {
                ["brokenKey"] = { %%% corrupt syntax @@@ },
                ["recentPlays"] = {
                    {
                        ["sound_file"] = "Sounds/valid.ogg",
                        ["text"] = "I survived corruption!",
                    },
                    { invalid entry },
                },
            }
        "#;

        let result = parse_saved_variables(corrupted_lua, "Default", PathBuf::new()).unwrap();
        // The parser should recover and parse the valid dialogue entry
        assert_eq!(result.recent_dialogues.len(), 1);
        assert_eq!(result.recent_dialogues[0].sound_file, "Sounds/valid.ogg");
        assert_eq!(result.recent_dialogues[0].text, "I survived corruption!");
    }

    #[test]
    fn test_find_saved_variables_files() {
        let temp_dir = std::env::temp_dir().join(format!("vv_test_sv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        let sv_dir = temp_dir
            .join("_forever_")
            .join("WTF")
            .join("Account")
            .join("12345#1")
            .join("SavedVariables");
        std::fs::create_dir_all(&sv_dir).unwrap();

        let lua_file = sv_dir.join("VivaVoce.lua");
        std::fs::write(&lua_file, "VivaVoceDB = {}\n").unwrap();

        let found = find_saved_variables_files(&temp_dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], lua_file);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_multiline_strings_and_comments() {
        let lua = r#"
            --[[
                This is a block comment
                spanning multiple lines
            ]]
            VivaVoceDB = {
                -- Single line comment
                ["recentPlays"] = {
                    {
                        sound_file = "Sounds/quest.ogg",
                        text = [[Questa è una trascrizione lunga
multilinea per la quest.]],
                        title = "La chiamata alle armi",
                        timestamp = 1730000000,
                    },
                },
                diagnostics = {
                    { message = "Audio channel adjusted", timestamp = 1730000005 },
                },
            }
        "#;

        let result = parse_saved_variables(lua, "TestAcc", PathBuf::new()).unwrap();
        assert_eq!(result.recent_dialogues.len(), 1);
        let d = &result.recent_dialogues[0];
        assert_eq!(d.sound_file, "Sounds/quest.ogg");
        assert_eq!(d.title, "La chiamata alle armi");
        assert!(d.text.contains("Questa è una trascrizione lunga\nmultilinea"));
        assert_eq!(d.timestamp, 1730000000);

        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].message, "Audio channel adjusted");
        assert_eq!(result.diagnostics[0].timestamp, Some(1730000005));
    }

    #[test]
    fn test_load_all_saved_variables_multiple_accounts() {
        let temp_dir = std::env::temp_dir().join(format!("vv_test_multi_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);

        let acc1_sv = temp_dir.join("_forever_").join("WTF").join("Account").join("ACC_ONE").join("SavedVariables");
        let acc2_sv = temp_dir.join("_forever_").join("WTF").join("Account").join("ACC_TWO").join("SavedVariables");
        std::fs::create_dir_all(&acc1_sv).unwrap();
        std::fs::create_dir_all(&acc2_sv).unwrap();

        let lua1 = r#"
            VivaVoceDB = {
                ["recentPlays"] = {
                    { sound_file = "Sounds/one.ogg", text = "Dialogue 1" }
                }
            }
        "#;
        let lua2 = r#"
            VivaVoceDB = {
                ["recentPlays"] = {
                    { sound_file = "Sounds/two.ogg", text = "Dialogue 2" }
                }
            }
        "#;

        std::fs::write(acc1_sv.join("VivaVoce.lua"), lua1).unwrap();
        std::fs::write(acc2_sv.join("VivaVoce.lua"), lua2).unwrap();

        let all_data = load_all_saved_variables(&temp_dir);
        assert_eq!(all_data.len(), 2);

        let acc_names: Vec<String> = all_data.iter().map(|d| d.account_name.clone()).collect();
        assert!(acc_names.contains(&"ACC_ONE".to_string()));
        assert!(acc_names.contains(&"ACC_TWO".to_string()));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
