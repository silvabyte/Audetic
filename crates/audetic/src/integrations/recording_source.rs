//! Provider-agnostic recording sources.
//!
//! A source either polls or accepts webhooks. The database already stores both
//! modes and a JSON `config` column, so another provider does not need a new
//! table. This module only builds the seam: Plaud is the poll implementation,
//! and no webhook provider is registered yet.

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::db::integrations::{RecordingSource as RecordingSourceRecord, SyncMode};

use super::plaud::PlaudSource;
use super::IntegrationService;

#[async_trait]
pub trait RecordingSource: Send + Sync {
    fn provider(&self) -> &str;
    fn source_instance(&self) -> &str;
    fn sync_mode(&self) -> SyncMode;

    async fn poll(&self, service: &IntegrationService, backfill: bool) -> Result<()>;

    /// Webhook providers override this. The default keeps the entry point
    /// available without a second schema.
    async fn accept_webhook(&self, _service: &IntegrationService, _payload: &[u8]) -> Result<()> {
        bail!(
            "Recording source {} does not accept webhooks",
            self.provider()
        )
    }
}

pub fn resolve(provider: &str, source_instance: &str) -> Result<Box<dyn RecordingSource>> {
    if provider == PlaudSource::PROVIDER && source_instance == PlaudSource::SOURCE_INSTANCE {
        return Ok(Box::new(PlaudSource));
    }
    bail!("No recording source implementation for {provider}/{source_instance}")
}

/// Automatic polling waits for `interval_minutes`. A missing interval, or a
/// webhook source, is never due: webhook deliveries arrive on their own.
pub fn poll_is_due(source: &RecordingSourceRecord) -> bool {
    if source.sync_mode != SyncMode::Poll {
        return false;
    }
    let Some(interval) = source.interval_minutes else {
        return false;
    };
    let Some(last_started) = source.last_started_at.as_deref() else {
        return true;
    };
    let Ok(last_started) = parse_db_timestamp(last_started) else {
        return true;
    };
    chrono::Utc::now() - last_started >= chrono::Duration::minutes(interval)
}

pub(crate) fn parse_db_timestamp(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")?;
    Ok(naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(
        mode: SyncMode,
        interval: Option<i64>,
        last_started_at: Option<&str>,
    ) -> RecordingSourceRecord {
        RecordingSourceRecord {
            provider: "example".to_string(),
            source_instance: "one".to_string(),
            sync_mode: mode,
            enabled: true,
            interval_minutes: interval,
            import_after: None,
            config: None,
            running: false,
            last_started_at: last_started_at.map(str::to_string),
            last_completed_at: None,
            last_error: None,
            updated_at: "2026-01-01 00:00:00".to_string(),
        }
    }

    #[test]
    fn plaud_resolves_and_an_unknown_provider_does_not() {
        let source = resolve(PlaudSource::PROVIDER, PlaudSource::SOURCE_INSTANCE).unwrap();
        assert_eq!(source.provider(), "plaud");
        assert_eq!(source.source_instance(), "default");
        assert_eq!(source.sync_mode(), SyncMode::Poll);
        assert!(resolve("other", "default").is_err());
    }

    #[test]
    fn only_poll_sources_with_an_interval_become_due() {
        assert!(poll_is_due(&source(SyncMode::Poll, Some(15), None)));
        assert!(!poll_is_due(&source(SyncMode::Poll, None, None)));
        assert!(!poll_is_due(&source(SyncMode::Webhook, None, None)));
        assert!(!poll_is_due(&source(
            SyncMode::Poll,
            Some(15),
            Some("2999-01-01 00:00:00"),
        )));
    }
}
