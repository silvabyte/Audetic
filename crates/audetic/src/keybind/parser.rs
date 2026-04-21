//! Parser for Hyprland keybinding configurations.

use std::fmt;
use std::path::{Path, PathBuf};

/// Represents a single modifier key
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Modifier {
    Super,
    Shift,
    Ctrl,
    Alt,
}

impl Modifier {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "SUPER" | "$MAINMOD" | "MOD" => Some(Modifier::Super),
            "SHIFT" => Some(Modifier::Shift),
            "CTRL" | "CONTROL" => Some(Modifier::Ctrl),
            "ALT" => Some(Modifier::Alt),
            _ => None,
        }
    }
}

impl fmt::Display for Modifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Modifier::Super => write!(f, "SUPER"),
            Modifier::Shift => write!(f, "SHIFT"),
            Modifier::Ctrl => write!(f, "CTRL"),
            Modifier::Alt => write!(f, "ALT"),
        }
    }
}

/// Collection of modifier keys
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Modifiers(pub Vec<Modifier>);

impl Modifiers {
    pub fn parse(s: &str) -> Self {
        let mods: Vec<Modifier> = s.split_whitespace().filter_map(Modifier::parse).collect();
        Modifiers(mods)
    }

    pub fn from_strs(strs: &[&str]) -> Self {
        let mods: Vec<Modifier> = strs.iter().filter_map(|s| Modifier::parse(s)).collect();
        Modifiers(mods)
    }

    pub fn contains(&self, modifier: &Modifier) -> bool {
        self.0.contains(modifier)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let strs: Vec<String> = self.0.iter().map(|m| m.to_string()).collect();
        write!(f, "{}", strs.join(" "))
    }
}

/// Source location of a binding
#[derive(Debug, Clone)]
pub struct BindingSource {
    pub file: PathBuf,
    pub line: usize,
}

/// Type of Hyprland bind directive
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindType {
    /// Standard bind
    Bind,
    /// Bind with description (shows in keybind viewer)
    Bindd,
    /// Bind that triggers on key release
    Bindr,
    /// Bind that works when screen is locked
    Bindl,
    /// Bind with description and locked
    Bindld,
    /// Other/unknown bind type
    Other(String),
}

impl BindType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bind" => BindType::Bind,
            "bindd" => BindType::Bindd,
            "bindr" => BindType::Bindr,
            "bindl" => BindType::Bindl,
            "bindld" => BindType::Bindld,
            other => BindType::Other(other.to_string()),
        }
    }
}

impl fmt::Display for BindType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindType::Bind => write!(f, "bind"),
            BindType::Bindd => write!(f, "bindd"),
            BindType::Bindr => write!(f, "bindr"),
            BindType::Bindl => write!(f, "bindl"),
            BindType::Bindld => write!(f, "bindld"),
            BindType::Other(s) => write!(f, "{}", s),
        }
    }
}

/// Represents a parsed Hyprland keybinding
#[derive(Debug, Clone)]
pub struct HyprBinding {
    pub bind_type: BindType,
    pub modifiers: Modifiers,
    pub key: String,
    pub description: Option<String>,
    pub dispatcher: String,
    pub command: String,
    pub source: BindingSource,
    /// The original line from the config file
    pub raw_line: String,
}

impl HyprBinding {
    /// Get a display string for the keybinding (e.g., "SUPER + R")
    pub fn display_key(&self) -> String {
        if self.modifiers.is_empty() {
            self.key.clone()
        } else {
            format!("{} + {}", self.modifiers, self.key)
        }
    }
}

/// Parse all bindings from a config file
pub fn parse_bindings(path: &Path) -> Vec<HyprBinding> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    parse_bindings_from_content(&content, path)
}

/// Parse bindings from content string (useful for testing)
pub fn parse_bindings_from_content(content: &str, source_path: &Path) -> Vec<HyprBinding> {
    let mut bindings = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check if this is a bind directive
        if let Some(binding) = parse_bind_line(trimmed, source_path, line_num + 1) {
            bindings.push(binding);
        }
    }

    bindings
}

/// Parse a single bind line
fn parse_bind_line(line: &str, source_path: &Path, line_num: usize) -> Option<HyprBinding> {
    // Match bind variants: bind, bindd, bindr, bindl, bindld, etc.
    let bind_prefixes = ["bindld", "bindd", "bindr", "bindl", "bind"];

    for prefix in bind_prefixes {
        if line.to_lowercase().starts_with(prefix) {
            let rest = &line[prefix.len()..].trim_start();

            // Should start with = or whitespace then =
            let after_eq = if let Some(stripped) = rest.strip_prefix('=') {
                stripped.trim_start()
            } else {
                continue;
            };

            return parse_bind_parts(prefix, after_eq, line, source_path, line_num);
        }
    }

    None
}

/// Parse the parts of a bind directive after the =
fn parse_bind_parts(
    bind_type_str: &str,
    parts_str: &str,
    raw_line: &str,
    source_path: &Path,
    line_num: usize,
) -> Option<HyprBinding> {
    // Split by comma, handling the command which may contain commas
    let parts: Vec<&str> = parts_str.splitn(5, ',').map(|s| s.trim()).collect();

    if parts.len() < 4 {
        return None;
    }

    let bind_type = BindType::from_str(bind_type_str);
    let modifiers = Modifiers::parse(parts[0]);
    let key = parts[1].to_string();

    // For bindd, the 3rd part is description, 4th is dispatcher, 5th is command
    // For bind, the 3rd part is dispatcher, 4th is command
    let (description, dispatcher, command) =
        if bind_type == BindType::Bindd || bind_type == BindType::Bindld {
            if parts.len() >= 5 {
                (
                    Some(parts[2].to_string()),
                    parts[3].to_string(),
                    parts[4].to_string(),
                )
            } else if parts.len() == 4 {
                // Might be missing command or description
                (
                    Some(parts[2].to_string()),
                    parts[3].to_string(),
                    String::new(),
                )
            } else {
                return None;
            }
        } else if parts.len() >= 4 {
            (None, parts[2].to_string(), parts[3].to_string())
        } else {
            return None;
        };

    Some(HyprBinding {
        bind_type,
        modifiers,
        key,
        description,
        dispatcher,
        command,
        source: BindingSource {
            file: source_path.to_path_buf(),
            line: line_num,
        },
        raw_line: raw_line.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_bind() {
        let line = "bind = SUPER, R, exec, curl http://localhost";
        let binding = parse_bind_line(line, Path::new("/test"), 1).unwrap();

        assert_eq!(binding.bind_type, BindType::Bind);
        assert!(binding.modifiers.contains(&Modifier::Super));
        assert_eq!(binding.key, "R");
        assert_eq!(binding.dispatcher, "exec");
        assert!(binding.command.contains("curl"));
    }

    #[test]
    fn test_parse_bindd_with_description() {
        let line =
            "bindd = SUPER SHIFT, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle";
        let binding = parse_bind_line(line, Path::new("/test"), 1).unwrap();

        assert_eq!(binding.bind_type, BindType::Bindd);
        assert!(binding.modifiers.contains(&Modifier::Super));
        assert!(binding.modifiers.contains(&Modifier::Shift));
        assert_eq!(binding.key, "R");
        assert_eq!(binding.description, Some("Audetic".to_string()));
        assert_eq!(binding.dispatcher, "exec");
    }

    #[test]
    fn test_modifiers_display() {
        let mods = Modifiers::from_strs(&["SUPER", "SHIFT"]);
        assert_eq!(mods.to_string(), "SUPER SHIFT");
    }

    #[test]
    fn test_modifiers_equality() {
        let mods1 = Modifiers::from_strs(&["SUPER", "SHIFT"]);
        let mods2 = Modifiers::from_strs(&["SUPER", "SHIFT"]);
        let mods3 = Modifiers::from_strs(&["SUPER"]);

        assert_eq!(mods1, mods2);
        assert_ne!(mods1, mods3);
    }
}
