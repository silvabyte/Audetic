//! Action: what a job does when its event fires.
//!
//! Today the only action is [`Action::Command`] (shell command). The enum
//! is tagged so a future variant (e.g. `Webhook { url, .. }`) slots in
//! without breaking persisted rows — the `action_type` column matches
//! the tag and the JSON `action_config` stores the variant fields.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Default timeout for command actions (1 hour). Long enough for an LLM
/// post-processing pipeline but bounded so a stuck child eventually dies.
pub const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 3600;

/// What a job does when matched.
///
/// Stored on disk as two columns: `action_type` (the tag) and
/// `action_config` (a JSON blob holding the variant fields). The
/// [`Action`] serde shape mirrors this so we can round-trip via
/// `serde_json` when materializing the row.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Run a shell command via `sh -c`. The event JSON payload is piped
    /// to the child's stdin; the command's exit code is logged but never
    /// fails the parent event.
    Command {
        command: String,
        #[serde(default = "default_command_timeout")]
        timeout_seconds: u64,
    },
}

fn default_command_timeout() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_SECONDS
}

impl Action {
    /// Stable tag matching the `action_type` column / OpenAPI variant.
    pub fn type_tag(&self) -> &'static str {
        match self {
            Self::Command { .. } => "command",
        }
    }

    /// JSON blob persisted in the `action_config` column. Round-trips
    /// via [`Action::from_storage`].
    pub fn config_json(&self) -> serde_json::Value {
        // Strip the discriminator — we already store it in `action_type`.
        match self {
            Self::Command {
                command,
                timeout_seconds,
            } => serde_json::json!({
                "command": command,
                "timeout_seconds": timeout_seconds,
            }),
        }
    }

    /// Rebuild from the persisted `(action_type, action_config)` pair.
    pub fn from_storage(action_type: &str, action_config: &str) -> anyhow::Result<Self> {
        let cfg: serde_json::Value = serde_json::from_str(action_config)
            .map_err(|e| anyhow::anyhow!("invalid action_config json: {e}"))?;
        match action_type {
            "command" => {
                let command = cfg
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("command action missing `command`"))?
                    .to_string();
                let timeout_seconds = cfg
                    .get("timeout_seconds")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECONDS);
                Ok(Self::Command {
                    command,
                    timeout_seconds,
                })
            }
            other => Err(anyhow::anyhow!("unknown action_type `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trips_through_storage() {
        let a = Action::Command {
            command: "tee /tmp/out".to_string(),
            timeout_seconds: 30,
        };
        let cfg = a.config_json().to_string();
        let restored = Action::from_storage(a.type_tag(), &cfg).unwrap();
        match restored {
            Action::Command {
                command,
                timeout_seconds,
            } => {
                assert_eq!(command, "tee /tmp/out");
                assert_eq!(timeout_seconds, 30);
            }
        }
    }

    #[test]
    fn missing_timeout_defaults() {
        let restored = Action::from_storage("command", r#"{"command":"echo hi"}"#).unwrap();
        match restored {
            Action::Command {
                timeout_seconds, ..
            } => assert_eq!(timeout_seconds, DEFAULT_COMMAND_TIMEOUT_SECONDS),
        }
    }

    #[test]
    fn unknown_action_type_errors() {
        let err = Action::from_storage("webhook", "{}").unwrap_err();
        assert!(err.to_string().contains("webhook"));
    }

    #[test]
    fn missing_command_field_errors() {
        let err = Action::from_storage("command", "{}").unwrap_err();
        assert!(err.to_string().contains("missing"));
    }
}
