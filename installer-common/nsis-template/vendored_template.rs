//! tauri-bundler **2.9.4** の NSIS テンプレート `installer.nsi` の写しと、それに
//! 製品ごとの当て直しを当てる部品。Hub（`apps/banto-hub/installer`）と ChronoGazer
//! （`apps/chronogazer/installer`）の両 xtask が `#[path = "..."] mod` で**同じこの
//! ファイル**を取り込む（#973 PR-3b。#972 で ChronoGazer に入れた仕組みを共通にした。
//! 2 つの xtask はそれぞれ独立したワークスペースなので、クレートにせず 1 ファイルを
//! 共有する）。
//!
//! - [`VENDORED_TEMPLATE`]: 隣の `installer.nsi`（crates.io の tauri-bundler-2.9.4 の
//!   `src/bundle/windows/nsis/installer.nsi` を 2026-10-31 に byte 単位でそのまま写した。
//!   改行は LF。`.gitattributes` で `-text`）。手を入れずに置き、当て直しは各製品の
//!   `TEMPLATE_PATCHES` で行う。
//! - [`patch_template`]: 文字列の完全一致の置き換えを順に当てる。土台が写しの中に
//!   ちょうど 1 回なければ名前を挙げて止まる（tauri-bundler の版を上げて原本が変わった
//!   ときに黙って当たらないままにしない）。
//!
//! tauri-bundler の版を上げるときは、両 xtask の `Cargo.toml`（`=2.9.4` で固定）と写しを
//! 取り直し、[`VENDORED_TEMPLATE_SHA256`] を更新し、各製品の当て直しの土台がまだ 1 回だけ
//! 一致するかを両 xtask の試験で確かめる。tauri-bundler は自前テンプレートでも
//! `utils.nsh`・`FileAssociation.nsh` を同じ出力先に書くので、`!include` はそのまま通る
//! （`nsis/mod.rs` で確認済み）。

use anyhow::{bail, Result};

/// tauri-bundler 2.9.4 の `installer.nsi` の写し（モジュール doc 参照）。
pub const VENDORED_TEMPLATE: &str = include_str!("installer.nsi");

/// [`VENDORED_TEMPLATE`] の SHA-256（`sha256sum installer.nsi` の値）。写しが黙って
/// 変わっていない（= 2.9.4 の原本のまま）ことを試験で確かめる（試験だけが使う）。
#[cfg(test)]
pub const VENDORED_TEMPLATE_SHA256: &str =
    "20f4ecc730defb71f1342eaeaec4021df13be3d843abba0effe88ea5835fa079";

/// 当て直し 1 つ: `(名前, 土台, 置き換え後)`。土台は写しの中に**ちょうど 1 回**現れる
/// 文字列。置き換え後は NSIS のソースなので LF 改行・2 空白のインデント（原本に合わせる）。
pub type TemplatePatch = (&'static str, &'static str, &'static str);

/// `patches` を順に当てる。土台が 1 回も無い・2 回以上ある、のどちらも名前を挙げた
/// エラー（tauri-bundler の版を上げて原本が変わったときの合図）。写しそのものが原本と
/// 違うかは試験（`vendored_template_is_the_unmodified_2_9_4_original`）で見る。
pub fn patch_template(template: &str, patches: &[TemplatePatch]) -> Result<String> {
    let mut out = template.to_string();
    for (name, anchor, replacement) in patches {
        match out.matches(anchor).count() {
            1 => out = out.replacen(anchor, replacement, 1),
            n => bail!(
                "NSIS テンプレートの当て直し {name} の土台が {n} 回一致しました（ちょうど 1 回のはず）。\
                 tauri-bundler の installer.nsi が変わっていないか、\
                 installer-common/nsis-template/installer.nsi の写しと TEMPLATE_PATCHES を見直してください"
            ),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写しは tauri-bundler 2.9.4 の原本のまま（黙って手を入れない。当て直しは各製品の
    /// `TEMPLATE_PATCHES` で）。版を上げたら写しを取り直してこの定数も更新する。
    #[test]
    fn vendored_template_is_the_unmodified_2_9_4_original() {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(VENDORED_TEMPLATE.as_bytes()));
        assert_eq!(digest, VENDORED_TEMPLATE_SHA256);
        // 原本は LF。土台の文字列も LF で書いてあるので、CRLF に変わると当たらない。
        assert!(!VENDORED_TEMPLATE.contains('\r'));
    }

    /// 土台が 1 回だけでなければ名前を挙げて止まる（0 回・2 回の両方）。当たるときは
    /// その 1 回だけを置き換える。
    #[test]
    fn patch_template_rejects_a_template_whose_anchor_count_is_not_one() {
        let patch: TemplatePatch = ("P-test", "Section WebView2\n", "Section WebView2\n  Nop\n");
        assert_eq!(VENDORED_TEMPLATE.matches(patch.1).count(), 1);

        let out = patch_template(VENDORED_TEMPLATE, &[patch]).expect("1 回");
        assert_eq!(out.matches("Section WebView2\n  Nop\n").count(), 1);
        assert_eq!(out.replacen("  Nop\n", "", 1), VENDORED_TEMPLATE);

        let missing = VENDORED_TEMPLATE.replacen(patch.1, "", 1);
        let err = patch_template(&missing, &[patch]).expect_err("0 回");
        assert!(err.to_string().contains(patch.0), "{err}");
        assert!(err.to_string().contains("0 回"), "{err}");

        let doubled = format!("{VENDORED_TEMPLATE}\n{}", patch.1);
        let err = patch_template(&doubled, &[patch]).expect_err("2 回");
        assert!(err.to_string().contains("2 回"), "{err}");
    }
}
