//! Backup management for Hyprland config modifications.

use anyhow::{Context, Result};
use chrono::Local;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

const MAX_BACKUPS: usize = 3;

/// Manages backups for Hyprland config files
pub struct BackupManager {
    /// Directory where backups are stored
    backup_dir: PathBuf,
}

impl BackupManager {
    /// Create a new backup manager
    pub fn new() -> Result<Self> {
        let backup_dir = crate::global::data_dir()?.join("keybind-backups");
        fs::create_dir_all(&backup_dir)
            .with_context(|| format!("Failed to create backup directory: {:?}", backup_dir))?;

        Ok(Self { backup_dir })
    }

    /// Create a backup of the given config file
    ///
    /// Returns the path to the backup file
    pub fn create_backup(&self, config_path: &Path) -> Result<PathBuf> {
        let filename = config_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config");

        let timestamp = Local::now().format("%Y%m%d-%H%M%S");
        let backup_name = format!("{}.audetic-backup-{}", filename, timestamp);
        let backup_path = self.backup_dir.join(&backup_name);

        debug!("Creating backup: {:?} -> {:?}", config_path, backup_path);

        fs::copy(config_path, &backup_path)
            .with_context(|| format!("Failed to create backup of {:?}", config_path))?;

        // Rotate old backups
        self.rotate_backups(filename)?;

        Ok(backup_path)
    }

    /// Rotate old backups, keeping only the most recent MAX_BACKUPS
    fn rotate_backups(&self, base_filename: &str) -> Result<()> {
        let prefix = format!("{}.audetic-backup-", base_filename);

        let mut backups: Vec<PathBuf> = fs::read_dir(&self.backup_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect();

        // Sort by modification time (newest first)
        backups.sort_by(|a, b| {
            let a_time = fs::metadata(a).and_then(|m| m.modified()).ok();
            let b_time = fs::metadata(b).and_then(|m| m.modified()).ok();
            b_time.cmp(&a_time)
        });

        // Remove old backups beyond MAX_BACKUPS
        for old_backup in backups.iter().skip(MAX_BACKUPS) {
            debug!("Removing old backup: {:?}", old_backup);
            let _ = fs::remove_file(old_backup);
        }

        Ok(())
    }

    /// List all available backups for a config file
    pub fn list_backups(&self, base_filename: &str) -> Result<Vec<PathBuf>> {
        let prefix = format!("{}.audetic-backup-", base_filename);

        let mut backups: Vec<PathBuf> = fs::read_dir(&self.backup_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect();

        // Sort by modification time (newest first)
        backups.sort_by(|a, b| {
            let a_time = fs::metadata(a).and_then(|m| m.modified()).ok();
            let b_time = fs::metadata(b).and_then(|m| m.modified()).ok();
            b_time.cmp(&a_time)
        });

        Ok(backups)
    }

    /// Restore the most recent backup for a config file
    pub fn restore_latest(&self, config_path: &Path) -> Result<()> {
        let filename = config_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config");

        let backups = self.list_backups(filename)?;

        if let Some(latest) = backups.first() {
            debug!("Restoring backup: {:?} -> {:?}", latest, config_path);
            fs::copy(latest, config_path)
                .with_context(|| format!("Failed to restore backup to {:?}", config_path))?;
            Ok(())
        } else {
            anyhow::bail!("No backups found for {:?}", config_path)
        }
    }
}

impl Default for BackupManager {
    fn default() -> Self {
        Self::new().expect("Failed to create backup manager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_backup_creation() {
        let temp_dir = std::env::temp_dir().join("audetic-test-backup");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let config_path = temp_dir.join("test.conf");

        // Create a test config file
        let mut file = fs::File::create(&config_path).unwrap();
        writeln!(file, "test content").unwrap();

        let manager = BackupManager {
            backup_dir: temp_dir.clone(),
        };

        let backup_path = manager.create_backup(&config_path).unwrap();
        assert!(backup_path.exists());

        let backup_content = fs::read_to_string(&backup_path).unwrap();
        assert!(backup_content.contains("test content"));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
