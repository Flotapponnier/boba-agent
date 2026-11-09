//! The embedded web UI (`web/dist`), with SPA fallback to `index.html`.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

const FALLBACK_PAGE: &str =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Boba Agent</title></head>\
<body style=\"font-family:system-ui;padding:2rem\"><h1>Boba Agent</h1>\
<p>The server is running, but the web UI was not built into this binary.</p>\
<p>Run <code>cd web &amp;&amp; pnpm i &amp;&amp; pnpm build</code>, then rebuild the server, \
or use <code>pnpm dev</code> on port 5173.</p></body></html>";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || path == "api" {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(boba_core::api::ApiError {
                error: "no such endpoint".into(),
                code: "not_found".into(),
            }),
        )
            .into_response();
    }
    if !path.is_empty() {
        if let Some(file) = Assets::get(path) {
            // Vite fingerprints everything under assets/: cache it forever.
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            return asset(file, cache);
        }
        // A missing file with an extension is a real 404, not a client route.
        let last = path.rsplit('/').next().unwrap_or("");
        if last.contains('.') {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    match Assets::get("index.html") {
        Some(file) => asset(file, "no-cache"),
        None => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            FALLBACK_PAGE,
        )
            .into_response(),
    }
}

fn asset(file: rust_embed::EmbeddedFile, cache: &'static str) -> Response {
    let mime = file.metadata.mimetype().to_string();
    let mut res = Response::new(Body::from(file.data.into_owned()));
    if let Ok(v) = HeaderValue::from_str(&mime) {
        res.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}
