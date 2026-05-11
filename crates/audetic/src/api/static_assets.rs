//! Serve the bundled apps/web-ui SPA from the daemon binary.
//!
//! `include_dir!` embeds `apps/web-ui/dist/` at compile time (see build.rs).
//! Routing rules:
//!   - exact path match → serve that file with a guessed content type
//!   - any other path → serve `index.html` so client-side routing works
//!     (e.g. `/dictations`, `/meetings/123`, `/settings/keybind`)

use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use include_dir::{include_dir, Dir};

static SPA_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../apps/web-ui/dist");

pub async fn serve_static(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');

    if let Some(file) = SPA_DIR.get_file(raw) {
        return file_response(file.path().to_string_lossy().as_ref(), file.contents());
    }

    // SPA history fallback. index.html is served `no-cache` so an updated
    // daemon can ship a new bundle without users hard-refreshing; hashed
    // assets under /assets/ are immutable and get a long cache.
    match SPA_DIR.get_file("index.html") {
        Some(file) => {
            let mut resp = file_response("index.html", file.contents());
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            resp
        }
        None => (StatusCode::NOT_FOUND, "UI bundle missing").into_response(),
    }
}

fn file_response(path: &str, contents: &'static [u8]) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut resp = Response::new(Body::from(contents));
    let header_value = HeaderValue::from_str(mime.as_ref())
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, header_value);

    // Hashed asset paths under /assets/ are content-addressed; safe to cache forever.
    if path.starts_with("assets/") {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    resp
}
