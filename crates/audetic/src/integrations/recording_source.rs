//! Recording source polling helpers.

use anyhow::Result;

use crate::db::integrations::RecordingSource as RecordingSourceRecord;

#[cfg(test)]
use crate::db::integrations::SyncMode;

/// Automatic polling waits for `interval_minutes`. A missing interval is never due.
pub fn poll_is_due(source: &RecordingSourceRecord) -> bool {
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

    fn source(interval: Option<i64>, last_started_at: Option<&str>) -> RecordingSourceRecord {
        RecordingSourceRecord {
            provider: "example".to_string(),
            source_instance: "one".to_string(),
            sync_mode: SyncMode::Poll,
            enabled: true,
            interval_minutes: interval,
            import_after: None,
            running: false,
            last_started_at: last_started_at.map(str::to_string),
            last_completed_at: None,
            last_error: None,
        }
    }

    #[test]
    fn sources_without_an_interval_are_never_due() {
        assert!(poll_is_due(&source(Some(15), None)));
        assert!(!poll_is_due(&source(None, None)));
        assert!(!poll_is_due(
            &source(Some(15), Some("2999-01-01 00:00:00"),)
        ));
    }
}
