//! ルートの DACL の確かめ（純関数。banto-hub-core の `hub_tls_prepare` から移した）。

use banto_safe_root::acl::{exposure, Exposure, FileSecurity, ACCESS_ALLOWED_ACE_TYPE};

/// ルートの**オブジェクト自身**に効くと、中の `tls\` を消して差し替えられる・ACL や所有者を
/// 書き換えられる権利（#644 PR1a のレビュー P1）: `DELETE`・`WRITE_DAC`・`WRITE_OWNER`・
/// `FILE_DELETE_CHILD`・`GENERIC_ALL`・`GENERIC_WRITE`・`MAXIMUM_ALLOWED`。ファイル・フォルダの
/// 作成（`FILE_ADD_FILE`・`FILE_ADD_SUBDIRECTORY`）と属性の書き込みは含めない（ProgramData から
/// 継承する既定の `BUILTIN\Users:(CI)(WD,AD,WEA,WA)` を許すため。作れても既にある `tls\` は
/// 消せない）。
pub const ROOT_TAKEOVER_MASK: u32 =
    0x0001_0000 | 0x0004_0000 | 0x0008_0000 | 0x0000_0040 | 0x1000_0000 | 0x4000_0000 | 0x0200_0000;

/// `INHERIT_ONLY_ACE`（オブジェクト自身には効かず、子へ伝わるだけの ACE）。
const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;

/// ルート（`%ProgramData%\BantoHub`）の記述子が、一般の利用者に `tls\` を差し替えさせる形の
/// 理由をすべて返す（純関数、#644 PR1a のレビュー P1。空なら通す）。[`exposure`] から、
/// 普通の許可（`ACCESS_ALLOWED_ACE_TYPE`）で inherit-only の ACE と、オブジェクト自身に効くが
/// [`ROOT_TAKEOVER_MASK`] のどれも含まない ACE を除いたもの。残るのは:
///
/// - 所有者が SYSTEM・Administrators・TrustedInstaller・`elevated`（実行した管理者本人）以外
///   （所有者は DACL に関わらず ACL を書き換えられる）。本人は昇格できる管理者なので、sink の
///   `dir_acl_plan` と同じく信頼する。ルートを継承で作ると、作った本人が所有者になり、親の
///   `CREATOR OWNER` が本人へのフルの ACE になるため、これを断ると管理者の通常の実行が通らない。
///   **読む側（サービス・クライアント）は `None`**（crate doc「所有者に『実行した本人』を
///   含めない」）。
/// - null DACL・種類の分からない ACE・SID が読めない ACE。
/// - 信頼する相手（`elevated` を含む）以外への、条件付き・オブジェクトの許可（中身を評価しない
///   ので、継承のフラグ・マスクを問わず断る側に倒す）。
/// - 信頼する相手以外への、オブジェクト自身に効く普通の許可で、[`ROOT_TAKEOVER_MASK`] を含む
///   もの。
///
/// 既定の形（ProgramData から継承: SYSTEM・Administrators にフル、`CREATOR OWNER` に
/// inherit-only のフル、`BUILTIN\Users` に読み取りと `(CI)(WD,AD,WEA,WA)`、所有者
/// Administrators）は通る。
pub fn root_dacl_problems(security: &FileSecurity, elevated: Option<&str>) -> Vec<Exposure> {
    exposure(security)
        .into_iter()
        .filter(|found| match found {
            // 実行した管理者本人（所有者としても、ACE の相手としても）は信頼する。
            Exposure::ForeignOwner(sid) => Some(sid.as_str()) != elevated,
            Exposure::AllowsOther { sid, .. } if Some(sid.as_str()) == elevated => false,
            Exposure::AllowsOther {
                ace_type,
                flags,
                mask,
                ..
            } if *ace_type == ACCESS_ALLOWED_ACE_TYPE => {
                flags & INHERIT_ONLY_ACE_FLAG == 0 && mask & ROOT_TAKEOVER_MASK != 0
            }
            _ => true,
        })
        .collect()
}
