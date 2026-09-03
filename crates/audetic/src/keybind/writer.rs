//! Safe, target-scoped file modification for Hyprland keybindings.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use audetic_core::keybind::KeybindTarget;

use super::{marker_for_target, ProposedBinding};

/// Write or update only the requested target's managed section.
pub fn write_binding(
    config_path: &Path,
    target: KeybindTarget,
    binding: &ProposedBinding,
) -> Result<()> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
    let new_content = update_or_append_binding(&content, target, binding);

    fs::write(config_path, new_content)
        .with_context(|| format!("Failed to write config file: {:?}", config_path))
}

fn update_or_append_binding(
    content: &str,
    target: KeybindTarget,
    binding: &ProposedBinding,
) -> String {
    let marker = marker_for_target(target);
    let section = format!("{marker}\n{}", binding.to_hyprland_line());

    if let Some((start, end)) = managed_section_range(content, marker) {
        let mut updated = String::with_capacity(content.len() + section.len());
        updated.push_str(&content[..start]);
        updated.push_str(&section);
        updated.push('\n');
        updated.push_str(&content[end..]);
        updated
    } else {
        let mut updated = content.to_string();
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        if !updated.ends_with("\n\n") {
            updated.push('\n');
        }
        updated.push_str(&section);
        updated.push('\n');
        updated
    }
}

/// Whether this file contains the target's managed section.
pub fn has_binding(config_path: &Path, target: KeybindTarget) -> Result<bool> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
    Ok(managed_section_range(&content, marker_for_target(target)).is_some())
}

/// Remove only the requested target's managed section.
pub fn remove_binding(config_path: &Path, target: KeybindTarget) -> Result<bool> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
    let marker = marker_for_target(target);
    let Some((start, end)) = managed_section_range(&content, marker) else {
        return Ok(false);
    };

    let mut updated = String::with_capacity(content.len());
    updated.push_str(&content[..start]);
    updated.push_str(content[end..].trim_start_matches('\n'));
    if !updated.ends_with('\n') {
        updated.push('\n');
    }

    fs::write(config_path, updated)
        .with_context(|| format!("Failed to write config file: {:?}", config_path))?;
    Ok(true)
}

/// Byte range containing a marker and its one generated binding line.
fn managed_section_range(content: &str, marker: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut marker_range = None;
    for line in content.split_inclusive('\n') {
        let line_end = offset + line.len();
        if line.trim_end_matches(['\n', '\r']).trim() == marker {
            marker_range = Some((offset, line_end));
            break;
        }
        offset = line_end;
    }
    let (start, marker_end) = marker_range?;

    if marker_end == content.len() {
        return Some((start, marker_end));
    }

    let next_line_end = content[marker_end..]
        .find('\n')
        .map(|offset| marker_end + offset + 1)
        .unwrap_or(content.len());
    let next_line = content[marker_end..next_line_end].trim();
    let end = if next_line.to_ascii_lowercase().starts_with("bind") {
        next_line_end
    } else {
        marker_end
    };
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybind::{target_endpoint, AUDETIC_MEETING_SECTION_MARKER, AUDETIC_SECTION_MARKER};

    #[test]
    fn target_sections_coexist_and_update_independently() {
        let original = "# Existing config\nbind = SUPER, SPACE, exec, rofi\n";
        let dictation = ProposedBinding::for_target(KeybindTarget::Dictation);
        let meeting = ProposedBinding::for_target(KeybindTarget::Meeting);

        let both = update_or_append_binding(
            &update_or_append_binding(original, KeybindTarget::Dictation, &dictation),
            KeybindTarget::Meeting,
            &meeting,
        );
        assert!(both.contains(AUDETIC_SECTION_MARKER));
        assert!(both.contains(AUDETIC_MEETING_SECTION_MARKER));
        assert!(both.contains(&target_endpoint(KeybindTarget::Dictation)));
        assert!(both.contains(&target_endpoint(KeybindTarget::Meeting)));

        let changed = ProposedBinding::new(KeybindTarget::Dictation, &["SUPER", "ALT"], "D");
        let updated = update_or_append_binding(&both, KeybindTarget::Dictation, &changed);
        assert!(updated.contains("bindd = SUPER ALT, D, Audetic"));
        assert!(updated.contains(&meeting.to_hyprland_line()));
    }

    #[test]
    fn removing_one_target_preserves_the_other() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("bindings.conf");
        let dictation = ProposedBinding::for_target(KeybindTarget::Dictation);
        let meeting = ProposedBinding::for_target(KeybindTarget::Meeting);
        let content = update_or_append_binding(
            &update_or_append_binding("", KeybindTarget::Dictation, &dictation),
            KeybindTarget::Meeting,
            &meeting,
        );
        fs::write(&config, content).unwrap();

        assert!(remove_binding(&config, KeybindTarget::Dictation).unwrap());
        let remaining = fs::read_to_string(&config).unwrap();
        assert!(!remaining.contains(AUDETIC_SECTION_MARKER));
        assert!(remaining.contains(AUDETIC_MEETING_SECTION_MARKER));
        assert!(remaining.contains(&meeting.to_hyprland_line()));
    }

    #[test]
    fn legacy_dictation_marker_is_updated_without_touching_following_comment() {
        let content = format!(
            "{AUDETIC_SECTION_MARKER}\nbindd = SUPER, R, Audetic, exec, old\n# Other section\nbind = ALT, X, exec, other\n"
        );
        let binding = ProposedBinding::for_target(KeybindTarget::Dictation);
        let updated = update_or_append_binding(&content, KeybindTarget::Dictation, &binding);

        assert!(!updated.contains("exec, old"));
        assert!(updated.contains("/toggle\n# Other section\nbind = ALT, X, exec, other"));
    }

    #[test]
    fn marker_text_inside_a_comment_never_claims_the_next_user_binding() {
        let content = format!(
            "# Keep this note mentioning {AUDETIC_SECTION_MARKER} for documentation\nbind = SUPER, U, exec, user-command\n"
        );
        let binding = ProposedBinding::for_target(KeybindTarget::Dictation);

        let updated = update_or_append_binding(&content, KeybindTarget::Dictation, &binding);
        assert!(updated.starts_with(&content));
        assert!(updated.contains("bind = SUPER, U, exec, user-command"));
        assert!(updated.contains(&binding.to_hyprland_line()));

        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("bindings.conf");
        fs::write(&config, &content).unwrap();
        assert!(!remove_binding(&config, KeybindTarget::Dictation).unwrap());
        assert_eq!(fs::read_to_string(config).unwrap(), content);
    }
}
