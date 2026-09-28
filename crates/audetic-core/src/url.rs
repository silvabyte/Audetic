//! Single source of truth for the daemon's API surface URLs.
//!
//! Anything that needs to refer to where the API lives — the Axum
//! router nest, the OpenAPI `servers` URL, the hyprland keybind
//! command, install-time readiness probes, daemon startup log
//! examples — derives from these constants instead of hardcoding
//! `http://127.0.0.1:3737/api`.
//!
//! Adding a new "well-known" endpoint? Add a path constant under
//! `paths` and call sites build the full URL via [`api_url`] rather
//! than inlining the string. The OpenAPI spec's `servers` URL is
//! still a literal in `api::docs` (utoipa requires it at macro time);
//! `tests::openapi_servers_url_matches` keeps it in sync with this
//! module.

/// Loopback host the daemon binds to. The daemon never listens on
/// anything else — this is local IPC over TCP, not a network service.
pub const HOST: &str = "127.0.0.1";

/// Default TCP port. WHSP in numbers (W=23, H=8, S=19, P=16 → 3737).
pub const DEFAULT_PORT: u16 = 3737;

/// Alternate loopback port for an isolated development installation. Both the
/// daemon and CLI validate this before starting, so a typo never targets the
/// normal installation accidentally.
pub fn port() -> anyhow::Result<u16> {
    parse_port(std::env::var("AUDETIC_PORT").ok().as_deref())
}

fn parse_port(value: Option<&str>) -> anyhow::Result<u16> {
    use anyhow::Context;
    let port = value
        .map(str::parse::<u16>)
        .transpose()
        .context("AUDETIC_PORT must be a TCP port between 1 and 65535")?
        .unwrap_or(DEFAULT_PORT);
    anyhow::ensure!(port != 0, "AUDETIC_PORT must be between 1 and 65535");
    Ok(port)
}

fn port_text() -> String {
    std::env::var("AUDETIC_PORT").unwrap_or_else(|_| DEFAULT_PORT.to_string())
}

/// Path prefix every API route is mounted under. Kept in sync with
/// the OpenAPI `servers` URL declared in `api::docs` so generated
/// clients hit the right path without translation.
pub const API_PREFIX: &str = "/api";

/// Well-known endpoint paths (server-relative — i.e. NOT including
/// the [`API_PREFIX`]). Use these when code needs to refer to a
/// specific endpoint, e.g. the hyprland keybind installer or the
/// readiness probe in `audetic install`.
pub mod paths {
    pub const VERSION: &str = "/version";
    pub const AUDIO_NOTES: &str = "/audio-notes";
    pub const AUDIO_NOTES_START: &str = "/audio-notes/start";
    pub const AUDIO_NOTES_TOGGLE: &str = "/audio-notes/toggle";
    pub const AUDIO_NOTES_STOP: &str = "/audio-notes/stop";
    pub const AUDIO_NOTES_CONFIRM: &str = "/audio-notes/confirm";
    pub const AUDIO_NOTES_CANCEL: &str = "/audio-notes/cancel";
    pub const AUDIO_NOTES_STATUS: &str = "/audio-notes/status";
    pub const AUDIO_NOTES_SETTINGS: &str = "/audio-notes/settings";
    pub const AUDIO_NOTES_IMPORT: &str = "/audio-notes/import";
    pub const AUDIO_NOTES_RECENT_TITLES: &str = "/audio-notes/recent-titles";
    pub const AGENT_PROFILES: &str = "/agent-profiles";
    pub const SUMMARY_TEMPLATES: &str = "/summary/templates";
    pub const POST_PROCESSING_JOBS: &str = "/post-processing/jobs";
    pub const POST_PROCESSING_EVENTS: &str = "/post-processing/events";
    pub const PROVIDER: &str = "/provider";
    pub const PROVIDER_STATUS: &str = "/provider/status";
    pub const PROVIDER_RUNTIME: &str = "/provider/runtime";
    pub const PROVIDER_CONFIG: &str = "/provider/config";
    pub const PROVIDER_VALIDATE: &str = "/provider/validate";
    pub const PROVIDER_RESET: &str = "/provider/reset";
    pub const PROVIDER_TEST: &str = "/provider/test";
    pub const LOGS: &str = "/logs";
    pub const MODELS: &str = "/models";
    pub const SETUP: &str = "/setup";
    pub const SYSTEM_RESTART: &str = "/system/restart";
    pub const KEYBIND_STATUS: &str = "/keybind/status";
    pub const KEYBIND_INSTALL: &str = "/keybind/install";
    pub const KEYBIND: &str = "/keybind";
}

/// Path to one agent profile test endpoint: `AGENT_PROFILES/{id}/test`.
pub fn agent_profile_test_path(id: i64) -> String {
    format!("{}/{id}/test", paths::AGENT_PROFILES)
}

pub fn audio_note_path(id: i64) -> String {
    format!("{}/{id}", paths::AUDIO_NOTES)
}

pub fn audio_note_audio_path(id: i64) -> String {
    format!("{}/audio", audio_note_path(id))
}

pub fn audio_note_retry_path(id: i64) -> String {
    format!("{}/retry", audio_note_path(id))
}

pub fn audio_note_process_path(id: i64) -> String {
    format!("{}/process", audio_note_path(id))
}

pub fn audio_note_title_path(id: i64) -> String {
    format!("{}/title", audio_note_path(id))
}

pub fn audio_note_regenerate_title_path(id: i64) -> String {
    format!("{}/regenerate-title", audio_note_path(id))
}

pub fn audio_note_artifacts_path(id: i64) -> String {
    format!("{}/artifacts", audio_note_path(id))
}

pub fn audio_note_artifact_path(id: i64, artifact_id: i64) -> String {
    format!("{}/{artifact_id}", audio_note_artifacts_path(id))
}

/// Path to one model's status: `MODELS/{id}`.
pub fn model_path(id: &str) -> String {
    format!("{}/{id}", paths::MODELS)
}

/// Path to start a model download: `MODELS/{id}/download`.
pub fn model_download_path(id: &str) -> String {
    format!("{}/{id}/download", paths::MODELS)
}

/// Path to one job: `POST_PROCESSING_JOBS/{id}`.
pub fn post_processing_job_path(id: i64) -> String {
    format!("{}/{id}", paths::POST_PROCESSING_JOBS)
}

/// Path to a job's test endpoint: `POST_PROCESSING_JOBS/{id}/test`.
pub fn post_processing_job_test_path(id: i64) -> String {
    format!("{}/{id}/test", paths::POST_PROCESSING_JOBS)
}

/// Build a fully-qualified daemon API URL — e.g.
/// `api_url(paths::AUDIO_NOTES)` → `http://127.0.0.1:3737/api/audio-notes`.
pub fn api_url(path: &str) -> String {
    format!("http://{HOST}:{}{API_PREFIX}{path}", port_text())
}

/// Root URL serving the bundled SPA — `http://127.0.0.1:3737/`.
pub fn app_url() -> String {
    format!("http://{HOST}:{}/", port_text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_ports_are_validated_without_falling_back() {
        assert_eq!(parse_port(None).unwrap(), DEFAULT_PORT);
        assert_eq!(parse_port(Some("3837")).unwrap(), 3837);
        for value in ["0", "65536", "", "not-a-port", "3737/path"] {
            assert!(parse_port(Some(value)).is_err());
        }
    }

    #[test]
    fn api_url_formats_correctly() {
        assert_eq!(
            api_url(paths::AUDIO_NOTES_TOGGLE),
            "http://127.0.0.1:3737/api/audio-notes/toggle"
        );
        assert_eq!(api_url(paths::VERSION), "http://127.0.0.1:3737/api/version");
        assert_eq!(api_url(paths::SETUP), "http://127.0.0.1:3737/api/setup");
        assert_eq!(
            api_url(paths::KEYBIND_INSTALL),
            "http://127.0.0.1:3737/api/keybind/install"
        );
    }

    #[test]
    fn app_url_formats_correctly() {
        assert_eq!(app_url(), "http://127.0.0.1:3737/");
    }

    #[test]
    fn audio_note_resources_share_the_collection_path() {
        assert_eq!(audio_note_path(42), "/audio-notes/42");
        for (path, suffix) in [
            (audio_note_audio_path(42), "audio"),
            (audio_note_retry_path(42), "retry"),
            (audio_note_process_path(42), "process"),
            (audio_note_title_path(42), "title"),
            (audio_note_regenerate_title_path(42), "regenerate-title"),
            (audio_note_artifacts_path(42), "artifacts"),
        ] {
            assert_eq!(path, format!("/audio-notes/42/{suffix}"));
        }
        assert_eq!(
            audio_note_artifact_path(42, 7),
            "/audio-notes/42/artifacts/7"
        );
    }
}
