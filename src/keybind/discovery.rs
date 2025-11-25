//! Hyprland configuration file discovery.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::debug;

/// Standard locations to search for Hyprland keybinding configs
const CONFIG_SEARCH_PATHS: &[&str] = &[
    "hypr/bindings.conf",
    "hypr/keybinds.conf",
    "hypr/hyprland.conf",
];

/// Result of configuration discovery
#[derive(Debug)]
pub struct ConfigDiscovery {
    /// The primary config file (hyprland.conf)
    pub main_config: Option<PathBuf>,
    /// The recommended file for writing bindings
    pub bindings_file: Option<PathBuf>,
    /// All sourced config files found
    pub sourced_files: Vec<PathBuf>,
}

impl ConfigDiscovery {
    /// Get the best file to write bindings to
    pub fn writable_config(&self) -> Option<&PathBuf> {
        self.bindings_file.as_ref().or(self.main_config.as_ref())
    }
}

/// Discover Hyprland configuration files
pub fn discover_config() -> Result<ConfigDiscovery> {
    let config_home = dirs::config_dir().context("Could not determine config directory")?;

    let mut discovery = ConfigDiscovery {
        main_config: None,
        bindings_file: None,
        sourced_files: Vec::new(),
    };

    // Search for config files in order of preference
    for relative_path in CONFIG_SEARCH_PATHS {
        let full_path = config_home.join(relative_path);
        debug!("Checking for config at: {:?}", full_path);

        if full_path.exists() {
            let filename = full_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            match filename {
                "bindings.conf" | "keybinds.conf" => {
                    if discovery.bindings_file.is_none() {
                        discovery.bindings_file = Some(full_path.clone());
                    }
                    discovery.sourced_files.push(full_path);
                }
                "hyprland.conf" => {
                    discovery.main_config = Some(full_path.clone());
                    // Parse sourced files from main config
                    if let Ok(sourced) = parse_sourced_files(&full_path) {
                        for src in sourced {
                            if !discovery.sourced_files.contains(&src) {
                                // Check if this is a bindings file
                                let src_filename =
                                    src.file_name().and_then(|n| n.to_str()).unwrap_or("");
                                if src_filename.contains("bind")
                                    && discovery.bindings_file.is_none()
                                {
                                    discovery.bindings_file = Some(src.clone());
                                }
                                discovery.sourced_files.push(src);
                            }
                        }
                    }
                }
                _ => {
                    discovery.sourced_files.push(full_path);
                }
            }
        }
    }

    Ok(discovery)
}

/// Parse `source = ` directives from a Hyprland config file
fn parse_sourced_files(config_path: &Path) -> Result<Vec<PathBuf>> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {:?}", config_path))?;

    let mut sourced = Vec::new();
    let home_dir = dirs::home_dir();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("source") {
            // Parse: source = path or source=path
            if let Some(path_part) = trimmed.strip_prefix("source").map(|s| s.trim()) {
                let path_str = path_part
                    .strip_prefix('=')
                    .map(|s| s.trim())
                    .unwrap_or(path_part);

                // Expand ~ to home directory
                let expanded = if path_str.starts_with('~') {
                    if let Some(ref home) = home_dir {
                        home.join(path_str.trim_start_matches("~/"))
                    } else {
                        PathBuf::from(path_str)
                    }
                } else {
                    PathBuf::from(path_str)
                };

                // Only add if the file exists and is in the user's config
                if expanded.exists() {
                    sourced.push(expanded);
                }
            }
        }
    }

    Ok(sourced)
}

/// Get all config files that should be checked for existing bindings
pub fn get_all_config_files(discovery: &ConfigDiscovery) -> Vec<&PathBuf> {
    let mut files = Vec::new();

    if let Some(ref main) = discovery.main_config {
        files.push(main);
    }

    for sourced in &discovery.sourced_files {
        if !files.contains(&sourced) {
            files.push(sourced);
        }
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_discovery_writable() {
        let discovery = ConfigDiscovery {
            main_config: Some(PathBuf::from("/home/user/.config/hypr/hyprland.conf")),
            bindings_file: Some(PathBuf::from("/home/user/.config/hypr/bindings.conf")),
            sourced_files: vec![],
        };

        assert_eq!(
            discovery.writable_config(),
            Some(&PathBuf::from("/home/user/.config/hypr/bindings.conf"))
        );
    }

    #[test]
    fn test_config_discovery_fallback_to_main() {
        let discovery = ConfigDiscovery {
            main_config: Some(PathBuf::from("/home/user/.config/hypr/hyprland.conf")),
            bindings_file: None,
            sourced_files: vec![],
        };

        assert_eq!(
            discovery.writable_config(),
            Some(&PathBuf::from("/home/user/.config/hypr/hyprland.conf"))
        );
    }
}
