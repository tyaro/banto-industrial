//! Embedded frontend assets (spec §11.1).
//!
//! The SvelteKit static build (`apps/chronogazer/build`) is gitignored
//! and may not exist on a fresh clone, so embedding it via `rust-embed` is
//! behind the `embed-ui` cargo feature (default OFF). With the feature
//! off, [`FrontendAssets`] serves a minimal built-in placeholder page
//! instead of failing to compile - `banto-server` itself never needs to
//! know this feature exists (it only sees the `UiAssets` trait).
//!
//! To build with the real frontend: `pnpm --filter chronogazer build`
//! first (produces `apps/chronogazer/build`), then
//! `cargo build -p chronogazer-core --features embed-ui`.

#[cfg(feature = "embed-ui")]
use banto_server::guess_mime;
use banto_server::UiAssets;
use std::borrow::Cow;

/// Injectable into `banto_server::static_files::static_router`.
pub struct FrontendAssets;

#[cfg(feature = "embed-ui")]
mod embedded {
    use rust_embed::RustEmbed;

    /// The SvelteKit `adapter-static` output. Path is relative to this
    /// crate (`apps/chronogazer/core`), so `../build` is
    /// `apps/chronogazer/build`.
    #[derive(RustEmbed)]
    #[folder = "../build"]
    pub struct Assets;
}

#[cfg(feature = "embed-ui")]
impl UiAssets for FrontendAssets {
    fn get(path: &str) -> Option<(String, Cow<'static, [u8]>)> {
        embedded::Assets::get(path).map(|file| (guess_mime(path).to_string(), file.data))
    }
}

#[cfg(not(feature = "embed-ui"))]
const PLACEHOLDER_HTML: &str = concat!(
    "<!doctype html>\n",
    "<html lang=\"ja\"><head><meta charset=\"utf-8\">",
    "<title>ChronoGazer</title></head><body>",
    "<p>フロントエンドが埋め込まれていません。",
    "`pnpm build` 後に `--features embed-ui` で再ビルドしてください。</p>",
    "</body></html>\n"
);

#[cfg(not(feature = "embed-ui"))]
impl UiAssets for FrontendAssets {
    fn get(path: &str) -> Option<(String, Cow<'static, [u8]>)> {
        if path.is_empty() || path == "index.html" {
            Some((
                "text/html; charset=utf-8".to_string(),
                Cow::Borrowed(PLACEHOLDER_HTML.as_bytes()),
            ))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "embed-ui"))]
    #[test]
    fn placeholder_serves_index_html_only() {
        let (mime, bytes) = FrontendAssets::get("index.html").expect("placeholder index.html");
        assert_eq!(mime, "text/html; charset=utf-8");
        assert!(String::from_utf8_lossy(&bytes).contains("フロントエンドが埋め込まれていません"));
        assert!(FrontendAssets::get("app.js").is_none());
    }

    #[cfg(feature = "embed-ui")]
    #[test]
    fn embedded_index_html_is_present() {
        let (mime, _bytes) = FrontendAssets::get("index.html").expect("embedded index.html");
        assert_eq!(mime, "text/html; charset=utf-8");
    }
}
