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
    pub const POST_PROCESSING_JOBS: &str = "/post-processing/jobs";
    pub const POST_PROCESSING_EVENTS: &str = "/post-processing/events";
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

    /// utoipa requires a literal in the `servers(url = ...)` macro,
    /// so we can't reference [`API_PREFIX`] there directly. This test
    /// catches the case where the two drift apart.
    #[test]
    fn openapi_servers_url_matches_api_url() {
        use crate::api::docs::ApiDoc;
        use utoipa::OpenApi;

        let doc = ApiDoc::openapi();
        let server_url = doc
            .servers
            .as_ref()
            .and_then(|s| s.first())
            .map(|s| s.url.clone())
            .expect("OpenAPI doc must declare at least one server");

        // Server URL is the base (no path suffix), so we compare
        // against `api_url("")`.
        assert_eq!(
            server_url,
            api_url(""),
            "OpenAPI servers URL drifted from api::url::api_url(\"\"). \
             Update either api/docs.rs servers() or api::url to match."
        );
    }

    /// Every `paths::*` constant must correspond to an operation in
    /// the OpenAPI spec. If you rename a route or drop a path const
    /// without updating the other side, this fails loudly.
    #[test]
    fn well_known_paths_exist_in_openapi_spec() {
        use crate::api::docs::ApiDoc;
        use utoipa::OpenApi;

        let doc = ApiDoc::openapi();
        let spec_paths: std::collections::HashSet<String> =
            doc.paths.paths.keys().cloned().collect();

        for known in [
            paths::VERSION,
            paths::TOGGLE,
            paths::MEETINGS_TOGGLE,
            paths::POST_PROCESSING_JOBS,
            paths::POST_PROCESSING_EVENTS,
        ] {
            assert!(
                spec_paths.contains(known),
                "api::url::paths references \"{known}\" but the OpenAPI \
                 spec has no such operation. Spec paths: {spec_paths:?}"
            );
        }
    }
}
