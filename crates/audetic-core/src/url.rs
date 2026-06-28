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
    pub const TOGGLE: &str = "/toggle";
    pub const MEETINGS_TOGGLE: &str = "/meetings/toggle";
    pub const MEETINGS_IMPORT: &str = "/meetings/import";
    pub const AGENT_PROFILES: &str = "/agent-profiles";
    pub const SUMMARY_TEMPLATES: &str = "/summary/templates";
    pub const POST_PROCESSING_JOBS: &str = "/post-processing/jobs";
    pub const POST_PROCESSING_EVENTS: &str = "/post-processing/events";
    pub const PROVIDER: &str = "/provider";
    pub const PROVIDER_STATUS: &str = "/provider/status";
    pub const PROVIDER_CONFIG: &str = "/provider/config";
    pub const PROVIDER_RESET: &str = "/provider/reset";
    pub const PROVIDER_TEST: &str = "/provider/test";
    pub const HISTORY: &str = "/history";
    pub const LOGS: &str = "/logs";
    pub const MODELS: &str = "/models";
    pub const TRANSCRIBE: &str = "/transcribe";
    pub const KEYBIND_STATUS: &str = "/keybind/status";
    pub const KEYBIND_INSTALL: &str = "/keybind/install";
    pub const KEYBIND: &str = "/keybind";
    pub const UPDATE_CHECK: &str = "/update/check";
    pub const UPDATE_INSTALL: &str = "/update/install";
    pub const UPDATE_AUTO: &str = "/update/auto";
}

/// Path to one agent profile test endpoint: `AGENT_PROFILES/{id}/test`.
pub fn agent_profile_test_path(id: i64) -> String {
    format!("{}/{id}/test", paths::AGENT_PROFILES)
}

/// Path to a meeting's generated artifacts: `/meetings/{id}/artifacts`.
pub fn meeting_artifacts_path(id: i64) -> String {
    format!("/meetings/{id}/artifacts")
}

/// Path to one generated meeting artifact: `/meetings/{id}/artifacts/{artifact_id}`.
pub fn meeting_artifact_path(id: i64, artifact_id: i64) -> String {
    format!("/meetings/{id}/artifacts/{artifact_id}")
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
/// `api_url(paths::TOGGLE)` → `http://127.0.0.1:3737/api/toggle`.
pub fn api_url(path: &str) -> String {
    format!("http://{HOST}:{DEFAULT_PORT}{API_PREFIX}{path}")
}

/// Root URL serving the bundled SPA — `http://127.0.0.1:3737/`.
pub fn app_url() -> String {
    format!("http://{HOST}:{DEFAULT_PORT}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_formats_correctly() {
        assert_eq!(api_url(paths::TOGGLE), "http://127.0.0.1:3737/api/toggle");
        assert_eq!(
            api_url(paths::MEETINGS_TOGGLE),
            "http://127.0.0.1:3737/api/meetings/toggle"
        );
        assert_eq!(api_url(paths::VERSION), "http://127.0.0.1:3737/api/version");
    }

    #[test]
    fn app_url_formats_correctly() {
        assert_eq!(app_url(), "http://127.0.0.1:3737/");
    }
}
