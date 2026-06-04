//! Shared helpers for talking to the `audeticd` daemon over its REST API.

use anyhow::{bail, Context, Result};
use audetic_core::url::api_url;
use serde_json::Value;

/// Friendly hint shown when the daemon can't be reached.
pub const CONNECT_HINT: &str = "Failed to connect to Audetic service. Is it running?";

/// Daemon API base — a single derived value so we never inline
/// `http://127.0.0.1:3737/api/...` in command modules.
pub fn base_url() -> String {
    let mut url = api_url("");
    if url.ends_with('/') {
        url.pop();
    }
    url
}

/// Decode the API response body, turning non-2xx status codes into a friendly
/// `anyhow::Error`. Extracts `.message` from a JSON error envelope when present,
/// otherwise falls back to a generic HTTP status message. An empty body decodes
/// to `Value::Null`.
pub async fn json_or_error(response: reqwest::Response, op: &str) -> Result<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("{op} response read failed"))?;

    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_else(|| format!("{op} failed (HTTP {status})"));
        bail!(msg);
    }

    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("{op} response parse error"))
}
