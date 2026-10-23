use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

/// PWA compilada (`web/dist`), embebida en el binario.
#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => asset(path, file),
        // Rutas de la SPA: sirven index.html.
        None if !path.contains('.') || path == "index.html" => match Assets::get("index.html") {
            Some(file) => asset("index.html", file),
            None => Html(
                "<h1>lrtodo</h1><p>La PWA no está compilada: <code>cd web &amp;&amp; npm ci &amp;&amp; npm run build</code> y recompilar.</p>",
            )
            .into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn asset(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Los assets con hash son inmutables; index y service worker, siempre frescos.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data,
    )
        .into_response()
}
