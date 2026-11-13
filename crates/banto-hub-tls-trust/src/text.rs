//! 理由の文言（純関数）。banto-hub-core の `profile_grant`・`sink_config_prepare` から移した
//! もので、Hub の `prepare-*`・サービスログと同じ文言を、この crate の確かめ
//! （[`crate::open_place`] など）とクライアントが使う。

use std::path::Path;

use banto_safe_root::acl::Exposure;
use banto_safe_root::guard::{RealPathRefusal, WellKnownRootRefusal};

/// [`Exposure`] の文言（純関数。`SafeRootError::Exposed` の Debug の表記を出さないため）。
/// banto-hub-sink の `config::place::exposure_text` と文言を揃える（片方を直したらもう一方も）。
pub fn exposure_text(found: &Exposure) -> String {
    match found {
        Exposure::ReparsePoint => {
            "ジャンクション・シンボリックリンクなど（reparse point）です".to_string()
        }
        Exposure::HardLinked(links) => {
            format!("ハードリンクです（名前が {links} 個あります）")
        }
        Exposure::ForeignOwner(owner) => {
            format!("所有者（{owner}）が SYSTEM・Administrators ではありません")
        }
        Exposure::NullDacl => "DACL が null です（だれでもフルアクセスできます）".to_string(),
        Exposure::AllowsOther { sid, mask, .. } => format!(
            "SYSTEM・Administrators 以外（{sid}）にアクセスを許しています（アクセス {mask:#x}）"
        ),
        Exposure::UnknownAce { ace_type, .. } => {
            format!("種類の分からない ACE（種類 {ace_type}）があります")
        }
        Exposure::UnreadableSid { ace_type, .. } => {
            format!("SID を読めない ACE（種類 {ace_type}）があります")
        }
    }
}

/// 見つけた [`Exposure`] をすべて「・」でつないだ文言。
pub fn reasons_text(found: &[Exposure]) -> String {
    found
        .iter()
        .map(exposure_text)
        .collect::<Vec<_>>()
        .join("・")
}

/// `banto_safe_root` の失敗の文言（`Exposed` は理由を 1 つずつ言い換え、それ以外は
/// `Display`）。
pub fn secret_error_text(error: &banto_safe_root::SafeRootError) -> String {
    match error {
        banto_safe_root::SafeRootError::Exposed(found) => {
            let reasons: Vec<String> = found.iter().map(exposure_text).collect();
            format!("書いた後も保護されていません（{}）", reasons.join("・"))
        }
        other => other.to_string(),
    }
}

/// ルートが既知のフォルダに重なるなどの文言。
pub fn well_known_text(root: &Path, refusal: &WellKnownRootRefusal) -> String {
    match refusal {
        WellKnownRootRefusal::NetworkOrSpecial => format!(
            "Hub のルート（{}）はネットワークの場所か特殊なパス（\\\\…）です。ローカルのドライブのフォルダにしてください",
            root.display()
        ),
        WellKnownRootRefusal::DriveRoot => format!(
            "Hub のルート（{}）はドライブのルートです。専用のフォルダ（例: D:\\BantoHub）にしてください",
            root.display()
        ),
        WellKnownRootRefusal::OverlapsProtected(folder) => format!(
            "Hub のルート（{}）は OS・プログラムの置き場か共有のフォルダ（{}）に重なります。専用のフォルダにしてください",
            root.display(),
            folder.display()
        ),
        WellKnownRootRefusal::ContainsProtected(folder) => format!(
            "Hub のルート（{}）は {} そのものかその上のフォルダです。その下の専用のフォルダにしてください",
            root.display(),
            folder.display()
        ),
    }
}

/// 本当の場所の食い違いの文言（`prepare-*` の文言。末尾は「止めます」）。
pub fn real_path_text(
    label: &str,
    expected: &Path,
    real: &Path,
    refusal: RealPathRefusal,
) -> String {
    match refusal {
        RealPathRefusal::Mismatch => format!(
            "{label}（{}）の本当の場所（{}）が期待と違います。途中のフォルダがジャンクションか短い名前の可能性があります。止めます",
            expected.display(),
            real.display()
        ),
        RealPathRefusal::WellKnown(refusal) => format!(
            "{label}（{}）の本当の場所が使えません: {}。止めます",
            real.display(),
            well_known_text(real, &refusal)
        ),
    }
}

/// 本当の場所の食い違いの理由（[`real_path_text`] から、末尾の「止めます」を除く。読む側の
/// 理由に使う）。
pub fn real_path_reason(
    label: &str,
    expected: &Path,
    real: &Path,
    refusal: RealPathRefusal,
) -> String {
    real_path_text(label, expected, real, refusal)
        .trim_end_matches("。止めます")
        .to_string()
}
