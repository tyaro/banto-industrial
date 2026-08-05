//! Embedded frontend assets (docs/tag-server-design.md §3.1: 管理 UI は
//! banto テンプレートの SvelteKit 静的ビルドを axum が配信する)。
//!
//! T0 のスコープには管理 UI フロントエンドの中身は含まれない（実装指示の
//! 「スコープ外」参照）— このモジュールは `embed-ui` feature の**枠だけ**を
//! chronogazer/relay-wright と同じ形で用意する。SvelteKit の静的ビルド
//! (`apps/banto-hub/build`) は将来追加されるまで存在しないため、埋め込みは
//! `embed-ui` cargo feature（既定 OFF）の背後に置く。feature が OFF の間は
//! [`FrontendAssets`] が最小のプレースホルダー HTML を返す - `banto-server`
//! 自身はこの feature の存在を知らない（`UiAssets` trait しか見ない）。
//!
//! 実フロントエンドが用意できたら: `pnpm --filter banto-hub build`
//! （`apps/banto-hub/build` を生成）→
//! `cargo build -p banto-hub-core --features embed-ui`。

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
    /// crate (`apps/banto-hub/core`), so `../build` is
    /// `apps/banto-hub/build`.
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
    "<title>banto-hub</title></head><body>",
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
