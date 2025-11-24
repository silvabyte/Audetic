//! Safe file modification for Hyprland keybindings.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::{ProposedBinding, AUDETIC_SECTION_MARKER};

/// Write a binding to the config file
///
/// This function will:
/// 1. Look for an existing Audetic section and update it
/// 2. Or append a new section at the end of the file
pub fn write_binding(config_path: &Path, binding: &ProposedBinding) -> Result<()> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {:?}", config_path))?;

    let new_content = update_or_append_binding(&content, binding);

    fs::write(config_path, new_content)
        .with_context(|| format!("Failed to write config file: {:?}", config_path))?;

    Ok(())
}

/// Update existing Audetic binding or append new one
fn update_or_append_binding(content: &str, binding: &ProposedBinding) -> String {
    let binding_line = binding.to_hyprland_line();
    let section = format!("{}\n{}", AUDETIC_SECTION_MARKER, binding_line);

    // Check if there's an existing Audetic section
    if let Some(start_idx) = content.find(AUDETIC_SECTION_MARKER) {
        // Find the end of the Audetic section (next blank line or comment section)
        let after_marker = &content[start_idx..];
        let section_end = find_section_end(after_marker);
        let end_idx = start_idx + section_end;

        // Replace the existing section
        let mut new_content = String::new();
        new_content.push_str(&content[..start_idx]);
        new_content.push_str(&section);
        new_content.push('\n');
        new_content.push_str(&content[end_idx..]);

        new_content
    } else {
        // Append to end of file
        let mut new_content = content.to_string();

        // Ensure there's a newline before our section
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push('\n');
        new_content.push_str(&section);
        new_content.push('\n');

        new_content
    }
}

/// Find the end of the Audetic section
fn find_section_end(section: &str) -> usize {
    let mut in_section = false;
    let mut last_content_end = 0;

    for (idx, line) in section.lines().enumerate() {
        let trimmed = line.trim();

        if idx == 0 {
            // Skip the marker line
            in_section = true;
            last_content_end = line.len() + 1; // +1 for newline
            continue;
        }

        if in_section {
            if trimmed.is_empty() {
                // End of section at blank line
                break;
            } else if trimmed.starts_with('#') && !trimmed.contains("Audetic") {
                // New comment section starts
                break;
            } else if trimmed.starts_with("bind") || trimmed.contains("audetic") || trimmed.to_lowercase().contains("audetic") {
                // Part of our section
                last_content_end += line.len() + 1;
            } else if trimmed.starts_with("bind") {
                // Another bind that's not ours
                break;
            } else {
                last_content_end += line.len() + 1;
            }
        }
    }

    last_content_end
}

/// Remove Audetic binding from the config file
pub fn remove_binding(config_path: &Path) -> Result<bool> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {:?}", config_path))?;

    if let Some(start_idx) = content.find(AUDETIC_SECTION_MARKER) {
        let after_marker = &content[start_idx..];
        let section_end = find_section_end(after_marker);
        let end_idx = start_idx + section_end;

        let mut new_content = String::new();
        new_content.push_str(&content[..start_idx]);

        // Skip any trailing newlines from the removed section
        let remaining = content[end_idx..].trim_start_matches('\n');
        if !remaining.is_empty() {
            new_content.push_str(remaining);
        }

        // Ensure file ends with newline
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }

        fs::write(config_path, new_content)
            .with_context(|| format!("Failed to write config file: {:?}", config_path))?;

        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybind::Modifiers;

    #[test]
    fn test_append_binding() {
        let content = "# Existing config\nbind = SUPER, SPACE, exec, rofi\n";
        let binding = ProposedBinding {
            modifiers: Modifiers::from_strs(&["SUPER"]),
            key: "R".to_string(),
            description: "Audetic".to_string(),
            command: "curl -X POST http://127.0.0.1:3737/toggle".to_string(),
        };

        let result = update_or_append_binding(content, &binding);

        assert!(result.contains(AUDETIC_SECTION_MARKER));
        assert!(result.contains("bindd = SUPER, R, Audetic"));
        assert!(result.contains("# Existing config"));
    }

    #[test]
    fn test_update_existing_binding() {
        let content = format!(
            "# Existing config\n{}\nbindd = SUPER, R, Audetic, exec, old-command\n\n# Other stuff\n",
            AUDETIC_SECTION_MARKER
        );
        let binding = ProposedBinding {
            modifiers: Modifiers::from_strs(&["SUPER", "SHIFT"]),
            key: "R".to_string(),
            description: "Audetic".to_string(),
            command: "curl -X POST http://127.0.0.1:3737/toggle".to_string(),
        };

        let result = update_or_append_binding(&content, &binding);

        assert!(result.contains("SUPER SHIFT, R"));
        assert!(!result.contains("old-command"));
        assert!(result.contains("# Other stuff"));
    }
}
