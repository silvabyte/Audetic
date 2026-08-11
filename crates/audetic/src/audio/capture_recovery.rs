use anyhow::{Context, Result};
use tracing::warn;

use std::time::Duration;

const REPLACEMENT_ATTEMPTS: usize = 3;
const REPLACEMENT_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRecovery {
    Ignored,
    Capturing,
    Degraded,
}

/// Apply the shared three-attempt/two-second replacement policy.
pub(crate) async fn start_capture_with_retries<T>(
    label: &str,
    mut start: impl FnMut() -> Result<T>,
) -> Result<T> {
    for attempt in 1..=REPLACEMENT_ATTEMPTS {
        match start() {
            Ok(capture) => return Ok(capture),
            Err(error) => {
                warn!(
                    "Failed to open {label} (attempt {attempt}/{REPLACEMENT_ATTEMPTS}): {error:#}"
                );
                if attempt == REPLACEMENT_ATTEMPTS {
                    return Err(error).with_context(|| {
                        format!("Failed to open {label} after {REPLACEMENT_ATTEMPTS} attempts")
                    });
                }
                tokio::time::sleep(REPLACEMENT_RETRY_DELAY).await;
            }
        }
    }

    unreachable!("replacement attempt range is non-empty")
}
