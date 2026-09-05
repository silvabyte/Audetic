//! Durable installation-state boundary for Library Sync.
//!
//! This is the only sync-domain module that coordinates identity, settings,
//! Serve ownership, and authority-scoped outbox resets. Callers receive one
//! coherent snapshot and commit role changes with an epoch compare-and-set.

use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, HubId, SyncRole};
use thiserror::Error;

use std::path::{Path, PathBuf};

use crate::db::sync_identity::{SyncIdentity, SyncIdentityRepository};
use crate::db::sync_serve::{SyncServeOwnership, SyncServeRepository};
use crate::db::sync_settings::{SyncSettings, SyncSettingsRepository};

#[derive(Clone, Debug)]
pub struct InstallationSnapshot {
    pub identity: SyncIdentity,
    pub settings: SyncSettings,
    pub serve_ownership: Option<SyncServeOwnership>,
    pub role_epoch: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitEffects {
    pub role_epoch: u64,
    pub obsolete_staged_paths: Vec<PathBuf>,
}

pub struct HomeHubCommit<'a> {
    pub settings: &'a SyncSettings,
    pub hub_id: HubId,
    pub owner_login: &'a str,
    pub local_device_id: DeviceId,
    pub reset_destination: bool,
    pub ownership: &'a SyncServeOwnership,
}

#[derive(Debug, Error)]
#[error("sync role changed concurrently (expected epoch {expected}, current epoch {actual})")]
pub struct EpochMismatch {
    pub expected: u64,
    pub actual: u64,
}

#[derive(Clone)]
pub struct InstallationState {
    db_path: PathBuf,
}

impl InstallationState {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn load(&self) -> Result<InstallationSnapshot> {
        let mut conn = self.open()?;
        let transaction = conn
            .transaction()
            .context("starting sync installation-state read transaction")?;
        let identity = SyncIdentityRepository::get_or_create_device(&transaction)?;
        let settings = SyncSettingsRepository::get(&transaction)?;
        let role_epoch = SyncSettingsRepository::role_epoch(&transaction)?;
        let serve_ownership = SyncServeRepository::get(&transaction)?;
        transaction
            .commit()
            .context("finishing sync installation-state read transaction")?;
        Ok(InstallationSnapshot {
            identity,
            settings,
            serve_ownership,
            role_epoch,
        })
    }

    pub fn commit_home_hub(
        &self,
        expected_epoch: u64,
        command: HomeHubCommit<'_>,
    ) -> Result<CommitEffects> {
        debug_assert_eq!(command.settings.role, SyncRole::HomeHub);
        self.commit(expected_epoch, |transaction| {
            SyncIdentityRepository::save_hub(transaction, command.hub_id, command.owner_login)?;
            SyncSettingsRepository::save(transaction, command.settings)?;
            SyncServeRepository::save(transaction, command.ownership)?;
            if command.reset_destination {
                crate::db::sync_outbox::SyncOutboxRepository::reset_for_new_destination(
                    transaction,
                    command.local_device_id,
                )
            } else {
                Ok(Vec::new())
            }
        })
    }

    pub fn commit_connected_device(
        &self,
        expected_epoch: u64,
        settings: &SyncSettings,
        local_device_id: DeviceId,
        reset_destination: bool,
    ) -> Result<CommitEffects> {
        debug_assert_eq!(settings.role, SyncRole::ConnectedDevice);
        self.commit(expected_epoch, |transaction| {
            SyncSettingsRepository::save(transaction, settings)?;
            if reset_destination {
                crate::db::sync_outbox::SyncOutboxRepository::reset_for_new_destination(
                    transaction,
                    local_device_id,
                )
            } else {
                Ok(Vec::new())
            }
        })
    }

    pub fn commit_standalone(
        &self,
        expected_epoch: u64,
        settings: &SyncSettings,
    ) -> Result<CommitEffects> {
        debug_assert_eq!(settings.role, SyncRole::Standalone);
        self.commit(expected_epoch, |transaction| {
            SyncSettingsRepository::save(transaction, settings)?;
            SyncServeRepository::clear(transaction)?;
            Ok(Vec::new())
        })
    }

    pub fn commit_payload_policy(
        &self,
        expected_epoch: u64,
        enabled: bool,
    ) -> Result<CommitEffects> {
        self.commit(expected_epoch, |transaction| {
            SyncSettingsRepository::update_payload_policy(transaction, enabled)?;
            if enabled {
                crate::db::sync_outbox::SyncOutboxRepository::reset_restageable_for_backfill(
                    transaction,
                )?;
            } else {
                crate::db::sync_outbox::SyncOutboxRepository::pause_blob_uploads(transaction)?;
            }
            Ok(Vec::new())
        })
    }

    pub fn record_contact(&self, expected_epoch: u64) -> Result<bool> {
        let conn = self.open()?;
        SyncSettingsRepository::record_contact(
            &conn,
            expected_epoch,
            &chrono::Utc::now().to_rfc3339(),
        )
    }

    pub fn record_error(&self, expected_epoch: u64, error: Option<&str>) -> Result<bool> {
        let conn = self.open()?;
        SyncSettingsRepository::record_error(&conn, expected_epoch, error)
    }

    pub fn observe_contact(
        &self,
        expected_epoch: u64,
        reachable: bool,
        error: Option<&str>,
    ) -> Result<bool> {
        if reachable {
            self.record_contact(expected_epoch)
        } else if let Some(error) = error {
            self.record_error(expected_epoch, Some(error))
        } else {
            Ok(self.epoch_is_current(expected_epoch)?)
        }
    }

    pub fn epoch_is_current(&self, expected_epoch: u64) -> Result<bool> {
        let conn = self.open()?;
        Ok(SyncSettingsRepository::role_epoch(&conn)? == expected_epoch)
    }

    pub fn reclaim_obsolete_staged_paths(
        &self,
        expected_epoch: u64,
        paths: &[PathBuf],
    ) -> Result<bool> {
        if paths.is_empty() {
            return Ok(true);
        }
        let conn = self.open()?;
        crate::db::sync_outbox::SyncOutboxRepository::reclaim_staged_paths_for_epoch(
            &conn,
            expected_epoch,
            paths,
        )
    }

    fn commit(
        &self,
        expected_epoch: u64,
        apply: impl FnOnce(&rusqlite::Connection) -> Result<Vec<PathBuf>>,
    ) -> Result<CommitEffects> {
        let mut conn = self.open()?;
        let transaction = conn
            .transaction()
            .context("starting sync installation-state transaction")?;
        let role_epoch = match SyncSettingsRepository::compare_and_increment_role_epoch(
            &transaction,
            expected_epoch,
        )? {
            Some(epoch) => epoch,
            None => {
                let actual = SyncSettingsRepository::role_epoch(&transaction)?;
                return Err(EpochMismatch {
                    expected: expected_epoch,
                    actual,
                }
                .into());
            }
        };
        let obsolete_staged_paths = apply(&transaction)?;
        transaction
            .commit()
            .context("committing sync installation state")?;
        Ok(CommitEffects {
            role_epoch,
            obsolete_staged_paths,
        })
    }

    fn open(&self) -> Result<rusqlite::Connection> {
        crate::db::open_db_at(&self.db_path).context("opening sync database")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_transition_compare_and_set_is_monotonic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let state = InstallationState::new(path);
        let initial = state.load().unwrap();
        let settings = SyncSettings {
            role: SyncRole::Standalone,
            ..initial.settings.clone()
        };

        let committed = state
            .commit_standalone(initial.role_epoch, &settings)
            .unwrap();
        assert_eq!(committed.role_epoch, initial.role_epoch + 1);

        let error = state
            .commit_standalone(initial.role_epoch, &settings)
            .unwrap_err();
        let mismatch = error.downcast_ref::<EpochMismatch>().unwrap();
        assert_eq!(mismatch.expected, initial.role_epoch);
        assert_eq!(mismatch.actual, committed.role_epoch);
    }

    #[test]
    fn stale_health_observation_cannot_overwrite_new_role_health() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let state = InstallationState::new(path);
        let initial = state.load().unwrap();
        let committed = state
            .commit_standalone(initial.role_epoch, &initial.settings)
            .unwrap();
        state
            .record_error(committed.role_epoch, Some("new role health"))
            .unwrap();

        assert!(!state
            .record_error(initial.role_epoch, Some("stale worker health"))
            .unwrap());
        assert_eq!(
            state.load().unwrap().settings.last_error.as_deref(),
            Some("new role health")
        );
    }
}
