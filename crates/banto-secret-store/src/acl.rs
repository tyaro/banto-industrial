//! 保存先ディレクトリの DACL（決定 4、計画書 §3.1(c)・§3.3）。
//!
//! # 期待する形
//!
//! 継承しない（保護された）DACL に、明示の許可 ACE だけがちょうど次のとおり並ぶ:
//!
//! | SID                                    | アクセス                        | 継承フラグ |
//! | -------------------------------------- | ------------------------------- | ---------- |
//! | SYSTEM（`S-1-5-18`）                   | フル（`FILE_ALL_ACCESS`）       | OI CI      |
//! | Administrators（`S-1-5-32-544`）       | フル                            | OI CI      |
//! | インストール利用者（指定したときだけ） | 変更（`0x1301BF`、ACL の変更・所有権の取得は含まない） | OI CI |
//!
//! SDDL では `D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1301bf;;;<SID>)`。
//! Users・Everyone・Authenticated Users・Operators のグループには何も付けない。
//! 中のファイルは OI で同じ ACE を継承する（一時ファイルも作った時点で継承し、
//! 同じディレクトリの中の置換なのでそのまま本体になる）。
//!
//! 所有者は SYSTEM・Administrators・インストール利用者のどれかであることも
//! 確かめる（所有者は DACL に依らず ACL を書き換えられるため。例えば
//! `%ProgramData%` の下で一般の利用者が先に同じ名前のフォルダを作り、同じ形の
//! DACL を付けておく、という乗っ取りを拒む）。
//!
//! # 作るとき・既にあるとき（既存のディレクトリの ACL が違う場合の扱い）
//!
//! - **無ければ**: `CreateDirectoryW` に上の記述子を渡し、作るのと同時に DACL を
//!   付ける（作ってから付けるまでの間に、親から継承した広い権限で見える時間を
//!   作らない）。インストール利用者の SID は、作る前に `LookupAccountSidW` で
//!   **利用者のアカウント（`SidTypeUser`）であること**を確かめる（Domain Users の
//!   ような S-1-5-21 のグループを渡されても広げない）。解決できない SID も拒む
//!   （fail closed。Microsoft Entra ID の利用者 `S-1-12-1-…` を含む - 下の
//!   [`InstallUserSid`] の doc）。
//! - **既にあれば**: DACL・所有者を読み戻して上の形と**完全に一致する**ことを
//!   確かめ、違えば [`SecretStoreError::AclMismatch`] で失敗する（fail closed）。
//!   **自動では直さない**（黙って広げることも狭めることもしない。違う形には、
//!   管理者が意図して変えた・別の製品が使っている・乗っ取りの準備、のどれも
//!   ありうるため）。直すのは [`reset_dir_acl`] の明示的な呼び出しだけで、
//!   B3-3 の昇格ヘルパ（管理者の操作）から使う想定。
//! - [`crate::FileSecretStore`] は開くときだけでなく、**読む・書く・消すたびに**
//!   この確認をする（途中で ACL が変えられたら、それ以降は使わない）。
//!
//! # インストール利用者の SID の検証
//!
//! `LookupAccountSidW` で利用者のアカウント（`SidTypeUser`）であることを確かめる
//! のは、**ディレクトリが無い（作る）経路と既にある（確かめる）経路の分岐より
//! 前**（[`ensure_dir`]・[`verify_dir`]・[`reset_dir_acl`] の頭）。既にある
//! ディレクトリの ACL がグループの SID に「変更」を付けていて、所有者・保護属性も
//! 検査に通る場合でも、グループの SID を受け入れないため（#595 のレビュー P2:
//! 以前は作る経路でだけ確かめていた）。
//!
//! [`crate::FileSecretStore`] は [`crate::FileSecretStore::open`] で 1 回だけ
//! 確かめ、読む・書く・消すたびの ACL の再確認では確かめ直さない。SID は open の
//! 時点で [`DirAccess`] に固定され、その後は変わらない（ACL の再確認は「その固定
//! した SID の形と一致するか」だけを見る）ため。名前の解決（ドメインでは
//! ネットワークに出うる）を書き込みのたびに行わない、という理由もある。
//!
//! ファイル 1 つ 1 つの DACL は確かめない（ディレクトリから継承したものの
//! はず。書き込みはファイルを丸ごと置き換えるので、書くたびに継承した形に戻る）。
//!
//! # 無いときだけ作る（[`create_dir_if_absent`]、#392 B2-4a）
//!
//! 秘密の保存先とは別に、**データのルート**（ChronoGazer の
//! `%ProgramData%\ChronoGazer`。2026-10-14 オーナー決定 O6）を同じ形の保護された DACL
//! で作るための口。[`ensure_dir`] と違い、**既にあれば何もしない**（DACL を読まず、
//! 確かめず、直さない - 既にあるルートは前の版・管理者・インストーラが作ったもので、
//! 形を問わずそのまま使う）。無いときだけ、[`ensure_dir`] と同じ記述子を
//! `CreateDirectoryW` に渡して作るのと同時に付ける。インストール利用者の SID の
//! 検証（利用者のアカウントであること）は**作る経路でだけ**行う（既にあるときは
//! SID を使わないので、検証の失敗で既存のルートを使えなくしない）。確かめてから
//! 作るまでの間に誰かが作った（`ERROR_ALREADY_EXISTS`）ときも、その形を問わず
//! 「作らなかった」を返す。親ディレクトリは無ければ普通に作る（親の ACL は
//! 継承のまま）。記述子・SID の検証の unsafe なコードをこの crate の外へ複製しない
//! ため、ここに置いた。
//!
//! # 既にあるディレクトリを直すとき（`pin_dir`・[`reset_dir_acl`]、#392 B3-3b）
//!
//! ChronoGazer の昇格ヘルパ（`chronogazer-elev.exe grant-data-acl`）は、管理者として
//! データのルート・`secrets\`・`secrets\hub-keys\` の DACL を [`reset_dir_acl`] で
//! 書き直す。インストール利用者はルートの中に「変更」を持つので、中に
//! ジャンクション・シンボリックリンク・ハードリンクを置ける。次を確かめた
//! （2026-10-16、Windows 11。下の `windows` テストが固定する）:
//!
//! - **書き直す対象そのものがリンクか**は、呼ぶ側が `pin_dir` で確かめる。
//!   `pin_dir` はパスを `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`
//!   で開き（リンクを辿らない）、属性（ディレクトリか・reparse point か）と所有者を
//!   **開いたハンドルから**読む。ハンドルは `FILE_LIST_DIRECTORY` を求め
//!   （データのアクセスを求めないハンドルは共有の確かめに加わらない）、
//!   `FILE_SHARE_DELETE` を渡さずに開くので、持っている間は誰もそのディレクトリを消す・名前を変える（= 確かめた後に
//!   ジャンクションへすり替える）ことができない。さらに `WRITE_DAC | WRITE_OWNER` を
//!   求め、`PinnedDir::reset_acl` は**そのハンドルで** `SetKernelObjectSecurity`（DACL と
//!   所有者。**自動の継承の伝播をしない** API）を書く（監査 B2・[A]。パスで
//!   `SetNamedSecurityInfoW` を呼ぶと、名前の変更・削除は止められても、
//!   `FILE_WRITE_ATTRIBUTES` は共有の確かめに加わらないので、確かめた後に**その場で**
//!   `FSCTL_SET_REPARSE_POINT` でジャンクションにされ、書き直しがその先へ向きうる。
//!   ハンドルに書けば、その場でリンクにされても書き換わるのはリンクそのものだけ -
//!   ハンドルは `OPEN_REPARSE_POINT` で開いているため。テスト
//!   `reset_acl_on_a_handle_turned_into_a_junction_stays_on_the_link`）。あわせて
//!   **所有者を `BUILTIN\Administrators` に移す**（監査 B5、2026-10-16 オーナー決定。所有者は
//!   DACL に関わらず ACL を変えられるので、インストール利用者が所有者のままだと書き
//!   直した DACL を本人が広げられる。昇格した管理者のトークンの Administrators は
//!   所有者にできる属性を持つので特権は要らない。昇格していなければ
//!   `ERROR_INVALID_OWNER`（1307）で何も変えずに失敗することを確かめた）。パスの版の
//!   [`reset_dir_acl`] は所有者を変えず、昇格していない呼び出し元（テストの片付け・
//!   `paths` のデスクトップ経路）だけが使う。
//! - **途中の要素のジャンクション・8.3 の短い名前も見分ける**: 開いた後に
//!   `GetFinalPathNameByHandleW`（[`PinnedDir::final_path`]）で本当のパスを取り、呼ぶ側が
//!   期待のパスと比べる（監査 Must 1。`FILE_FLAG_OPEN_REPARSE_POINT` が辿らないのは最後の
//!   要素だけなので、`D:\data\ChronoGazer` の `D:\data` をジャンクションにすり替えられる
//!   経路を、本当のパスで塞ぐ）。
//! - **継承は自前で伝え、中のリンクは辿らない**: `SetNamedSecurityInfoW` と
//!   `SetSecurityInfo`（ハンドル）の**自動の伝播**は、advapi がユーザーモードで木を歩いて
//!   既にある子へ継承の ACE を書くが、**その過程で子のジャンクション・ハードリンクの
//!   先にも書きうる**（Microsoft の仕様。#626 のオーナーレビュー P1）。そこで
//!   `set_security_tree` は自動の伝播を使わず、対象そのものは `SetKernelObjectSecurity`
//!   （伝播しない API）で書き、継承は自前で子へ伝える。**パスは一切使わない**（監査 [A]。
//!   パスで列挙・パスで開くと、確かめた後に途中の要素をジャンクションに変えられて先を
//!   辿る）: 子は、親のディレクトリの**ハンドル**を
//!   `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` で列挙してファイル ID を取り、
//!   `OpenFileById`（ヒント = 親のハンドル、`FILE_FLAG_OPEN_REPARSE_POINT`）で辿らずに開く。
//!   開いた後に属性をハンドルから確かめ、**reparse point は飛ばし**（[`ResetSkips::reparse`]）、
//!   **ハードリンク（リンク数 2 以上のファイル）も飛ばし**（[`ResetSkips::hardlink`]。記述子を
//!   共有するので書くと木の外の名前のアクセスを変える。#624）、それ以外だけに同じ記述子を
//!   書いて、フォルダならその子のハンドルを列挙元として再帰する。`.`・`..` は除き、列挙・
//!   開く途中の失敗は fail closed（消えた子だけ飛ばす）。
//!   - 【測った事実・自前の再帰】`PinnedDir::reset_acl` は、中の普通の子（孫まで）へ保護された
//!     DACL と Administrators 所有を書き、子のジャンクションとハードリンクは飛ばして
//!     `ResetSkips` に記録する。リンクの先（ルートの外）は変えない（テスト
//!     `self_recursion_writes_children_and_skips_links`・`reset_acl_does_not_follow_a_child_junction`・
//!     `reset_acl_does_not_rewrite_a_hardlink_target`・その場のすり替えは
//!     `reset_acl_on_a_handle_turned_into_a_junction_stays_on_the_link`。所有者を移すので
//!     昇格が要り、非管理者では 1307 で止まる - 書く側は CI で確かめる）。
//!   - 【測った事実・OS の自動伝播】パスの版 `reset_dir_acl`（`SetNamedSecurityInfoW`）は、
//!     既にある普通の子へ継承の ACE を OS が自動で伝える（テスト
//!     `os_auto_propagation_reaches_existing_children`）。これが**子のリンクの先にも書きうる**
//!     ので、昇格ヘルパーの書き直しでは使わない。
//!   - 【測った事実・ハンドル列挙】ハンドルを持ったままそのディレクトリをその場で
//!     ジャンクションに変えても、**元のハンドルの列挙はそのディレクトリ（空）そのものを返す**
//!     （リンクの先の中身は返らない。テスト
//!     `enumerating_a_handle_turned_into_a_junction_lists_the_empty_object`。管理者でなくても
//!     走る）。再帰の途中のすり替えも、ID とハンドルだけで辿るので先に向かない（テスト
//!     `a_child_swapped_to_a_junction_mid_recursion_does_not_leak`。子に書いた直後に
//!     フックですり替える。子まで進むのは管理者のとき = CI）。
//!   - 【仮説】ファイル ID は volume 内で安定で、`OpenFileById` はその object を開く - ID が
//!     別の object に再割り当てされる窓は無い前提（ID で開いた後も属性をハンドルから確かめる）。
//!
//! # 非 Windows
//!
//! DACL の概念が無いので、作る・確かめる・直すはどれも
//! [`SecretStoreError::UnsupportedPlatform`] を返す（黙って成功にしない）。
//! [`InstallUserSid`] の文字列の検査と [`check_dacl`] の判定は純関数で、全 OS で
//! テストする。

use crate::error::{AclMismatch, Result, SecretStoreError};

/// SYSTEM。
pub const SYSTEM_SID: &str = "S-1-5-18";
/// BUILTIN\Administrators。
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
/// `FILE_ALL_ACCESS`。
pub const FULL_ACCESS_MASK: u32 = 0x001F_01FF;
/// NTFS の「変更」= `FILE_GENERIC_READ | FILE_GENERIC_WRITE |
/// FILE_GENERIC_EXECUTE | DELETE`（`WRITE_DAC`・`WRITE_OWNER` を含まない。
/// banto-hub の `profile_acl::OWNER_MODIFY_ACCESS_MASK` と同じ値）。
pub const MODIFY_ACCESS_MASK: u32 = 0x0013_01BF;
/// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`。
pub const INHERIT_FLAGS: u8 = 0x03;
/// `INHERITED_ACE`。
const INHERITED_ACE_FLAG: u8 = 0x10;
/// `ACCESS_ALLOWED_ACE_TYPE`。
pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// インストール利用者の SID（文字列形式、例 `S-1-5-21-…-1001`）。
///
/// 受け付けるのは、利用者のアカウントが入る 2 つの領域の SID だけ:
///
/// - `S-1-5-21-…`: ローカル・Active Directory のアカウント。
/// - `S-1-12-1-…`: Microsoft Entra ID（Azure AD）に参加した PC の Entra の利用者。
///
/// Everyone（`S-1-1-0`）・Authenticated Users（`S-1-5-11`）・
/// BUILTIN の各グループ（`S-1-5-32-…`）・SYSTEM などはこの時点で拒む。文字は
/// 数字と `-` だけに限る（SDDL に組み込むので、`)` や `;` で ACE を足されない）。
/// 同じ領域のグループ（Domain Users など）は文字列では見分けられないので、
/// Windows でディレクトリを作る・直すときに `LookupAccountSidW` で利用者の
/// アカウントであることを確かめる（`SidTypeUser` 以外は拒む）。
///
/// Entra の利用者の SID は、その利用者でサインインしたことのある PC では
/// `LookupAccountSidW` が解決し（`AzureAD\<名前>`、種類は `SidTypeUser`）、
/// そのまま使える。**解決できないとき（その PC で一度もサインインしていない
/// 利用者・名前の解決に Entra へ届かない等）は、グループと見分けられないので
/// 拒む（fail closed、[`SecretStoreError::InvalidUserSid`]）**。インストールは
/// その利用者自身のサインイン中に行う（B3-3 の昇格ヘルパは呼び出した利用者の
/// SID を渡す）ので、通常は解決できる。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallUserSid(String);

impl InstallUserSid {
    pub fn parse(s: &str) -> Result<Self> {
        const PREFIXES: [&str; 2] = ["S-1-5-21-", "S-1-12-1-"];
        let Some(rest) = PREFIXES.iter().find_map(|prefix| s.strip_prefix(prefix)) else {
            return Err(SecretStoreError::InvalidUserSid(
                "利用者のアカウントの SID（S-1-5-21-… か S-1-12-1-…）ではありません",
            ));
        };
        let parts: Vec<&str> = rest.split('-').collect();
        let well_formed = s.len() <= 184
            && parts.len() >= 2
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()));
        if !well_formed {
            return Err(SecretStoreError::InvalidUserSid("SID の形式ではありません"));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 保存先ディレクトリに付ける権限の指定。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirAccess {
    install_user: Option<InstallUserSid>,
}

impl DirAccess {
    /// SYSTEM と Administrators だけ（サービスだけが使う構成）。
    pub fn service_only() -> Self {
        Self { install_user: None }
    }

    /// SYSTEM と Administrators に加えて、インストール利用者に「変更」（決定 4 -
    /// 昇格していないデスクトップの host が同じキーを読み書きするため）。
    pub fn with_install_user(sid: InstallUserSid) -> Self {
        Self {
            install_user: Some(sid),
        }
    }

    pub fn install_user(&self) -> Option<&InstallUserSid> {
        self.install_user.as_ref()
    }

    /// 期待する ACE（SID・アクセス）。
    fn expected_aces(&self) -> Vec<(String, u32)> {
        let mut aces = vec![
            (SYSTEM_SID.to_owned(), FULL_ACCESS_MASK),
            (ADMINISTRATORS_SID.to_owned(), FULL_ACCESS_MASK),
        ];
        if let Some(user) = &self.install_user {
            aces.push((user.0.clone(), MODIFY_ACCESS_MASK));
        }
        aces
    }

    /// 作るとき・直すときに使う SDDL（`D:P` = 保護された DACL）。
    pub fn sddl(&self) -> String {
        let mut sddl = format!("D:P(A;OICI;FA;;;{SYSTEM_SID})(A;OICI;FA;;;{ADMINISTRATORS_SID})");
        if let Some(user) = &self.install_user {
            sddl.push_str(&format!("(A;OICI;{MODIFY_ACCESS_MASK:#x};;;{})", user.0));
        }
        sddl
    }
}

/// 読み戻した ACE 1 つ。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedAce {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
    /// 許可・拒否以外の種類の ACE では空。
    pub sid: String,
}

/// 読み戻したディレクトリのセキュリティ記述子（必要な部分だけ）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedDacl {
    /// `SE_DACL_PROTECTED`。
    pub protected: bool,
    /// 所有者の SID。
    pub owner: String,
    /// `None` は null DACL（全員にフルアクセス）。
    pub aces: Option<Vec<ObservedAce>>,
}

/// 読み戻した記述子が `access` の期待どおりか（モジュール doc「期待する形」）。純関数。
pub fn check_dacl(
    observed: &ObservedDacl,
    access: &DirAccess,
) -> std::result::Result<(), AclMismatch> {
    let Some(aces) = &observed.aces else {
        return Err(AclMismatch::NoDacl);
    };
    if !observed.protected {
        return Err(AclMismatch::NotProtected);
    }
    let mut expected = access.expected_aces();
    for ace in aces {
        if ace.flags & INHERITED_ACE_FLAG != 0 {
            return Err(AclMismatch::InheritedAce {
                sid: ace.sid.clone(),
            });
        }
        let position = (ace.ace_type == ACCESS_ALLOWED_ACE_TYPE && ace.flags == INHERIT_FLAGS)
            .then(|| {
                expected
                    .iter()
                    .position(|(sid, mask)| *sid == ace.sid && *mask == ace.mask)
            })
            .flatten();
        match position {
            Some(i) => {
                expected.swap_remove(i);
            }
            None => {
                return Err(AclMismatch::UnexpectedAce {
                    ace_type: ace.ace_type,
                    flags: ace.flags,
                    mask: ace.mask,
                    sid: ace.sid.clone(),
                })
            }
        }
    }
    if let Some((sid, mask)) = expected.into_iter().next() {
        return Err(AclMismatch::MissingAce { sid, mask });
    }
    let owner_allowed = observed.owner == SYSTEM_SID
        || observed.owner == ADMINISTRATORS_SID
        || access
            .install_user
            .as_ref()
            .is_some_and(|user| user.0 == observed.owner);
    if !owner_allowed {
        return Err(AclMismatch::DisallowedOwner {
            sid: observed.owner.clone(),
        });
    }
    Ok(())
}

/// 無ければ期待の DACL で作り、あれば確かめる（モジュール doc「作るとき・
/// 既にあるとき」）。親ディレクトリは無ければ普通に作る（親の ACL は B3-3 の
/// 担当で、ここでは触らない）。
///
/// **作る・確かめるのどちらの経路でも、先に**インストール利用者の SID が利用者の
/// アカウントであることを `LookupAccountSidW` で確かめる（モジュール doc
/// 「インストール利用者の SID の検証」）。
pub fn ensure_dir(dir: &std::path::Path, access: &DirAccess) -> Result<()> {
    reject_dot_segments(dir)?;
    imp::ensure_user_account(access)?;
    imp::ensure_dir(dir, access)
}

/// 既にあるディレクトリを確かめる（作らない）。[`ensure_dir`] と同じく、先に
/// インストール利用者の SID を確かめる。
pub fn verify_dir(dir: &std::path::Path, access: &DirAccess) -> Result<()> {
    imp::ensure_user_account(access)?;
    imp::verify_dir(dir, access)
}

/// [`ensure_dir`] から SID の検証を除いたもの。`access` の SID を既に確かめた
/// 呼び出し側（[`crate::FileSecretStore`] の読む・書く・消すたびの確認）だけが使う。
pub(crate) fn ensure_dir_with_validated_sid(
    dir: &std::path::Path,
    access: &DirAccess,
) -> Result<()> {
    imp::ensure_dir(dir, access)
}

/// [`verify_dir`] から SID の検証を除いたもの（同上）。
pub(crate) fn verify_dir_with_validated_sid(
    dir: &std::path::Path,
    access: &DirAccess,
) -> Result<()> {
    imp::verify_dir(dir, access)
}

/// 無ければ期待の DACL で作り（`true`）、既にあれば何もしない（`false`。DACL を
/// 読まない・確かめない・直さない）。モジュール doc「無いときだけ作る」。
/// インストール利用者の SID は作る経路でだけ確かめる。既にあるパスがディレクトリで
/// なければ [`AclMismatch::NotADirectory`]。シンボリックリンク・ジャンクションは
/// 辿った先がディレクトリなら「既にある」。
pub fn create_dir_if_absent(dir: &std::path::Path, access: &DirAccess) -> Result<bool> {
    reject_dot_segments(dir)?;
    imp::create_dir_if_absent(dir, access)
}

/// `.`・`..`（と点だけの名前）の要素を含むパスを、何も作らずに断る（多重の防御。
/// PR #604 のレビュー）。親を `create_dir_all` で作ってから `CreateDirectoryW` が
/// 正規化すると、例えば `…\X\child\..` では親の作成が本当の `X` を継承の ACL で先に
/// 作り、`CreateDirectoryW` が「既にある」になって保護された DACL が付かない。
/// `Path::components` は途中の `.` を読み飛ばすので、文字列の要素で見る。
fn reject_dot_segments(dir: &std::path::Path) -> Result<()> {
    let text = dir.to_string_lossy();
    let is_separator = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    if text
        .split(is_separator)
        .any(|segment| !segment.is_empty() && segment.chars().all(|c| c == '.'))
    {
        return Err(SecretStoreError::DotSegmentInPath);
    }
    Ok(())
}

/// 既にあるディレクトリの DACL を、期待の形に**書き直す**（明示的な修復。
/// 管理者の操作から呼ぶ）。中のファイル・サブディレクトリへは OS が継承を
/// 伝える。**所有者は変えない**ので、所有者が許可外なら直した後も
/// [`verify_dir`] は失敗する（所有権を取り直すのは手作業）。
pub fn reset_dir_acl(dir: &std::path::Path, access: &DirAccess) -> Result<()> {
    imp::reset_dir_acl(dir, access)
}

/// ディレクトリの DACL と所有者を読み戻す（診断・テスト用）。
pub fn read_dir_dacl(dir: &std::path::Path) -> Result<ObservedDacl> {
    imp::read_dacl(dir)
}

/// インストール利用者の SID が、この PC で解決できる**利用者のアカウント**
/// （`LookupAccountSidW` の `SidTypeUser`）であることを確かめる（#392 B3-3b。
/// ディレクトリを作る・直す前と同じ確かめを、昇格ヘルパが SID を記録する前に
/// 使う）。非 Windows は [`SecretStoreError::UnsupportedPlatform`]。
pub fn check_user_account(sid: &InstallUserSid) -> Result<()> {
    imp::ensure_user_account(&DirAccess::with_install_user(sid.clone()))
}

/// `pin_dir` で開いたディレクトリ（パス）の、開いたハンドルから読んだ情報。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedDirInfo {
    /// ディレクトリか（`FILE_ATTRIBUTE_DIRECTORY`）。
    pub is_dir: bool,
    /// reparse point か（`FILE_ATTRIBUTE_REPARSE_POINT`。ジャンクション・
    /// シンボリックリンクのほか、クラウドのファイルなど種類を問わない）。
    pub is_reparse_point: bool,
    /// 所有者の SID。
    pub owner: String,
}

/// [`PinnedDir::reset_acl`] が**触らずに飛ばした**中の子（#392 B3-3b の監査 Must 2）。
/// 書くと木の外へ影響するので、報告だけして書き換えない。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResetSkips {
    /// reparse point（ジャンクション・シンボリックリンクなど）。辿らず・触らない。
    pub reparse: Vec<std::path::PathBuf>,
    /// ハードリンク（ファイルでリンク数 2 以上）。記述子を共有するので触らない（#624）。
    pub hardlink: Vec<std::path::PathBuf>,
}

/// パスを**辿らずに**開き、持っている間は消す・名前を変えることを誰にもさせない
/// ディレクトリ（モジュール doc「既にあるディレクトリを直すとき」）。drop で閉じる。
#[cfg(windows)]
pub struct PinnedDir {
    handle: imp::OwnedHandle,
    path: std::path::PathBuf,
    /// `GetFinalPathNameByHandleW` で解決した本当のパス（`\\?\` を外した DOS 形式。
    /// 途中の要素のジャンクション・シンボリックリンク・8.3 の短い名前を辿った後の
    /// 正規の姿）。呼ぶ側が期待のパスと比べる（#392 B3-3b の監査 Must 1）。
    final_path: std::path::PathBuf,
    info: PinnedDirInfo,
}

#[cfg(windows)]
impl std::fmt::Debug for PinnedDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedDir")
            .field("path", &self.path)
            .field("final_path", &self.final_path)
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
impl PinnedDir {
    /// 開いたパス。
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// `GetFinalPathNameByHandleW` で解決した本当のパス（`\\?\` を外した DOS 形式）。
    /// 途中の要素がジャンクション・シンボリックリンク・8.3 の短い名前でも、辿った後の
    /// 正規の姿になる。呼ぶ側が期待のパスと比べて、別の場所を開かされていないかを
    /// 確かめる（#392 B3-3b の監査 Must 1）。
    pub fn final_path(&self) -> &std::path::Path {
        &self.final_path
    }

    /// 開いたハンドルから読んだ属性と所有者。
    pub fn info(&self) -> &PinnedDirInfo {
        &self.info
    }

    /// 開いたままの**ハンドルで**、このフォルダと中の子へ、保護された DACL と所有者
    /// （`BUILTIN\Administrators`）を書く（#392 B3-3b の監査 Must 2・B5、2026-10-16 オーナー決定）。
    ///
    /// - 対象そのものは `SetKernelObjectSecurity`（**自動の継承の伝播をしない**）でハンドルに
    ///   書く。パスではなくハンドル（`OPEN_REPARSE_POINT` で開いた）に書くので、確かめてから
    ///   書くまでの間にその場で reparse point にされても、書き換わるのはリンクそのものだけ。
    /// - 継承は**自前で**中の子へ伝える（`SetNamedSecurityInfoW`・`SetSecurityInfo` の自動の
    ///   伝播は、advapi がユーザーモードで木を歩いて**子のジャンクション・ハードリンクの先にも
    ///   書く**ため使わない）。子は `OPEN_REPARSE_POINT` で辿らずに開き、reparse point は
    ///   飛ばし（[`ResetSkips::reparse`]）、ハードリンク（リンク数 2 以上のファイル）も
    ///   飛ばす（[`ResetSkips::hardlink`]。記述子を共有するので、書くと木の外の名前の
    ///   アクセスを変える。#624）。飛ばした子は返り値で報告する。
    ///
    /// 所有者を移すのは、所有者は DACL に関わらず ACL を変えられるため（インストール利用者が
    /// 所有者のままだと書き直した DACL を本人が広げられる。インストール利用者の「変更」は残る）。
    /// 昇格した管理者のプロセスから呼ぶ（昇格していなければ `ERROR_INVALID_OWNER` で失敗）。
    /// **開いた時点で reparse point・ディレクトリでないものには何もせずに断る**（呼ぶ側の
    /// 確かめに加えた多重の防御）。今の所有者を受け入れるかの確かめは呼ぶ側（製品の判断）。
    pub fn reset_acl(&self, access: &DirAccess) -> Result<ResetSkips> {
        if self.info.is_reparse_point {
            return Err(SecretStoreError::AclMismatch(AclMismatch::ReparsePoint));
        }
        if !self.info.is_dir {
            return Err(SecretStoreError::AclMismatch(AclMismatch::NotADirectory));
        }
        let mut skips = ResetSkips::default();
        imp::set_security_tree(self.handle.raw(), &self.final_path, access, &mut skips)?;
        Ok(skips)
    }
}

/// パスを辿らずに開いて [`PinnedDir`] を返す（モジュール doc「既にあるディレクトリを
/// 直すとき」）。無ければ `Ok(None)`。`.`・`..` を含むパスは開かずに断る。
#[cfg(windows)]
pub fn pin_dir(dir: &std::path::Path) -> Result<Option<PinnedDir>> {
    reject_dot_segments(dir)?;
    imp::pin_dir(dir)
}

/// 今のプロセスのトークンの利用者の SID（デスクトップ host が自分を
/// インストール利用者として渡すとき・テスト用）。
#[cfg(windows)]
pub fn current_user_sid() -> Result<InstallUserSid> {
    InstallUserSid::parse(&imp::current_user_sid_string()?)
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::{DirAccess, ObservedDacl};
    use crate::error::{Result, SecretStoreError};

    pub(super) fn ensure_user_account(_access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn ensure_dir(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn verify_dir(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn reset_dir_acl(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn create_dir_if_absent(_dir: &Path, _access: &DirAccess) -> Result<bool> {
        Err(SecretStoreError::UnsupportedPlatform)
    }

    pub(super) fn read_dacl(_dir: &Path) -> Result<ObservedDacl> {
        Err(SecretStoreError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SetNamedSecurityInfoW, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation,
        LookupAccountSidW, SidTypeUser, TokenUser, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_ATTRIBUTES, SE_DACL_PROTECTED, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::{check_dacl, DirAccess, ObservedAce, ObservedDacl, ACCESS_ALLOWED_ACE_TYPE};
    use crate::error::{AclMismatch, IoOp, Result, SecretStoreError};

    /// `ACCESS_DENIED_ACE_TYPE`（許可と同じ並び: ヘッダ・マスク・SID）。
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_str(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error() -> u32 {
        // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
        unsafe { GetLastError() }
    }

    fn api(call: &'static str, os_error: u32) -> SecretStoreError {
        SecretStoreError::AclApi { call, os_error }
    }

    /// `LocalAlloc` で確保された領域を drop で `LocalFree` する。
    struct LocalBox(*mut core::ffi::c_void);

    impl Drop for LocalBox {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: 各 Win32 API が LocalAlloc で返した領域（それぞれの呼び出し元の
                // コメント）。ここ以外では解放しない。
                unsafe {
                    LocalFree(self.0 as _);
                }
            }
        }
    }

    /// SDDL から自己相対形式のセキュリティ記述子を作る。
    fn descriptor(access: &DirAccess) -> Result<LocalBox> {
        let sddl = wide_str(&access.sddl());
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` は NUL 終端。成功すれば `sd` は LocalAlloc の領域（LocalBox が解放）。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(api(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                last_error(),
            ));
        }
        Ok(LocalBox(sd))
    }

    /// インストール利用者の SID が利用者のアカウントであること（グループではない）。
    pub(super) fn ensure_user_account(access: &DirAccess) -> Result<()> {
        let Some(user) = access.install_user() else {
            return Ok(());
        };
        let text = wide_str(user.as_str());
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `text` は NUL 終端。成功すれば `sid` は LocalAlloc の領域。
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == 0 {
            return Err(SecretStoreError::InvalidUserSid("SID の形式ではありません"));
        }
        let sid = LocalBox(sid);
        let mut name = [0u16; 256];
        let mut domain = [0u16; 256];
        let mut name_len = name.len() as u32;
        let mut domain_len = domain.len() as u32;
        let mut kind: SID_NAME_USE = 0;
        // SAFETY: バッファは長さとともに渡す。名前そのものは使わず、種類だけ見る。
        let ok = unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                sid.0,
                name.as_mut_ptr(),
                &mut name_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut kind,
            )
        };
        if ok == 0 {
            use windows_sys::Win32::Foundation::ERROR_NONE_MAPPED;
            let os_error = last_error();
            // `ERROR_NONE_MAPPED`（この SID に名前が無い = アカウントではない）だけを
            // 「アカウントでない」とみなし、それ以外（RPC 不達・アクセス拒否など）は
            // **一時的な失敗**として別の種類で返す（#392 B3-3b の監査 Nit 2。呼ぶ側が
            // 終了コードを 64 と 1 に分けられるように）。
            if os_error == ERROR_NONE_MAPPED {
                return Err(SecretStoreError::InvalidUserSid(
                    "この PC にこの SID のアカウントがありません",
                ));
            }
            return Err(api("LookupAccountSidW", os_error));
        }
        if kind != SidTypeUser {
            return Err(SecretStoreError::InvalidUserSid(
                "利用者のアカウントではありません（グループなど）",
            ));
        }
        Ok(())
    }

    fn inspect(dir: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(SecretStoreError::AclMismatch(AclMismatch::ReparsePoint));
                }
                if !meta.is_dir() {
                    return Err(SecretStoreError::AclMismatch(AclMismatch::NotADirectory));
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SecretStoreError::io(IoOp::InspectDir, &e)),
        }
    }

    pub(super) fn ensure_dir(dir: &Path, access: &DirAccess) -> Result<()> {
        // SID の検証は呼び出し側（公開の `ensure_dir`・`FileSecretStore::open`）が
        // 作る・確かめるの分岐より前に済ませている。
        if inspect(dir)? {
            return verify_dir(dir, access);
        }
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| SecretStoreError::io(IoOp::CreateDir, &e))?;
        }
        let sd = descriptor(access)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let path = wide(dir);
        // SAFETY: `path` は NUL 終端、`attributes` と記述子は呼び出しの間生きている。
        let ok = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
        if ok == 0 {
            let os_error = last_error();
            if os_error != ERROR_ALREADY_EXISTS {
                return Err(SecretStoreError::io(
                    IoOp::CreateDir,
                    &std::io::Error::from_raw_os_error(os_error as i32),
                ));
            }
            // 確かめてから作るまでの間に誰かが作った: 作った側の形を確かめる。
        }
        verify_dir(dir, access)
    }

    pub(super) fn create_dir_if_absent(dir: &Path, access: &DirAccess) -> Result<bool> {
        // 辿った先で判断する（既にあるルートがリンクでも、今までどおり使う）。
        match std::fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => return Ok(false),
            Ok(_) => return Err(SecretStoreError::AclMismatch(AclMismatch::NotADirectory)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SecretStoreError::io(IoOp::InspectDir, &e)),
        }
        // 作る経路でだけ SID を確かめる（モジュール doc「無いときだけ作る」）。
        ensure_user_account(access)?;
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| SecretStoreError::io(IoOp::CreateDir, &e))?;
        }
        let sd = descriptor(access)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let path = wide(dir);
        // SAFETY: `path` は NUL 終端、`attributes` と記述子は呼び出しの間生きている。
        let ok = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
        if ok == 0 {
            let os_error = last_error();
            if os_error == ERROR_ALREADY_EXISTS {
                // 確かめてから作るまでの間に誰かが作った: 形を問わずそのまま使う。
                return Ok(false);
            }
            return Err(SecretStoreError::io(
                IoOp::CreateDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        Ok(true)
    }

    pub(super) fn verify_dir(dir: &Path, access: &DirAccess) -> Result<()> {
        if !inspect(dir)? {
            return Err(SecretStoreError::io(
                IoOp::InspectDir,
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let observed = read_dacl(dir)?;
        check_dacl(&observed, access).map_err(SecretStoreError::AclMismatch)
    }

    /// 期待の保護された DACL を**パスで**書き直す（所有者は変えない）。昇格していない
    /// 呼び出し元（テストの片付け・`paths` のデスクトップ経路）も使う。`pin_dir` を
    /// 持たない分、確かめてから書き直すまでの間にその場でリンクにされる余地があるので、
    /// 昇格ヘルパーの書き直しは [`super::PinnedDir::reset_acl`]（ハンドルで書く）を使う。
    pub(super) fn reset_dir_acl(dir: &Path, access: &DirAccess) -> Result<()> {
        if !inspect(dir)? {
            return Err(SecretStoreError::io(
                IoOp::InspectDir,
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        ensure_user_account(access)?;
        let sd = descriptor(access)?;
        let dacl = dacl_of(&sd)?;
        let path = wide(dir);
        // SAFETY: `path` は NUL 終端、`dacl` は `sd` が生きている間有効。PROTECTED_DACL で
        // 親からの継承を切る。所有者・グループ・SACL は変えない。
        let status = unsafe {
            SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        if status != 0 {
            return Err(api("SetNamedSecurityInfoW", status));
        }
        Ok(())
    }

    /// `sd` の中の DACL のポインタ（`sd` が生きている間だけ有効。解放しない）。
    fn dacl_of(sd: &LocalBox) -> Result<*mut ACL> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        // SAFETY: `sd` は有効な記述子。`dacl` はその中を指す。
        let ok =
            unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 || present == 0 || dacl.is_null() {
            return Err(api("GetSecurityDescriptorDacl", last_error()));
        }
        Ok(dacl)
    }

    /// SDDL（所有者 `BUILTIN\\Administrators`・保護された DACL）から自己相対の記述子を作る。
    fn owned_protected_sd(access: &DirAccess) -> Result<LocalBox> {
        // `O:BA` で所有者を Administrators に、`access.sddl()` が `D:P(...)`（保護された DACL）。
        let sddl = wide_str(&format!("O:BA{}", access.sddl()));
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` は NUL 終端。成功すれば `sd` は LocalAlloc の領域（LocalBox が解放）。
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(api(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                last_error(),
            ));
        }
        Ok(LocalBox(sd))
    }

    /// 開いたハンドルの**そのオブジェクトだけ**に、所有者（Administrators）と保護された DACL を
    /// 書く（`SetKernelObjectSecurity`、#392 B3-3b の監査 Must 2）。`SetNamedSecurityInfoW` や
    /// `SetSecurityInfo` と違い、**自動の継承の伝播をしない**（advapi がユーザーモードで木を
    /// 歩いて子へ書くこともしない）ので、中のジャンクション・ハードリンクの先には決して
    /// 書かない。継承は呼ぶ側（[`set_security_tree`]）が、辿らずに開いた子へ自前で伝える。
    /// ハンドルは `OPEN_REPARSE_POINT` で開いているので、その場で reparse point にされても
    /// 書き換わるのはリンクそのものだけ。所有者を移すのは所有者が DACL に関わらず ACL を
    /// 変えられるため（昇格したトークンの Administrators は所有者にできるので特権は要らない。
    /// 昇格していなければ `ERROR_INVALID_OWNER`(1307) で失敗）。
    fn set_object_security(handle: HANDLE, sd: &LocalBox) -> Result<()> {
        use windows_sys::Win32::Security::SetKernelObjectSecurity;

        // SAFETY: `handle` は WRITE_DAC|WRITE_OWNER で開いた有効なハンドル。`sd` は自己相対の
        // 有効な記述子（所有者と保護された DACL を含む）。SACL は触らない。
        let ok = unsafe {
            SetKernelObjectSecurity(
                handle,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                sd.0,
            )
        };
        if ok == 0 {
            return Err(api("SetKernelObjectSecurity", last_error()));
        }
        Ok(())
    }

    /// 辿らずに開いたハンドルの情報（[`open_no_follow`]）。
    struct Opened {
        handle: OwnedHandle,
        is_dir: bool,
        is_reparse: bool,
        links: u32,
    }

    // `set_security_tree` が再帰のときに子へ書いた直後に呼ぶフック（テスト専用。
    // 子をその場で reparse point にすり替える反証 (d) に使う）。本番では `None`。
    #[cfg(test)]
    pub(super) type AfterChildHook = Box<dyn Fn(HANDLE)>;
    #[cfg(test)]
    thread_local! {
        pub(super) static AFTER_CHILD_WRITTEN: std::cell::RefCell<Option<AfterChildHook>> =
            const { std::cell::RefCell::new(None) };
    }

    /// 子を開くときの、DACL と所有者を書ける権利（`pin_dir` と同じ）。`FILE_LIST_DIRECTORY`
    /// はフォルダの列挙に要る（ファイルでは読み取りになるだけで害はない）。
    fn child_access() -> u32 {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
        };
        READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | WRITE_DAC | WRITE_OWNER
    }

    /// `handle` が指すオブジェクトの属性（ディレクトリか・reparse point か・リンク数）を
    /// ハンドルから読む。
    fn info_of(handle: HANDLE) -> Result<(bool, bool, u32)> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT,
        };
        // SAFETY: 全フィールドが整数の構造体。`handle` は有効。
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return Err(api("GetFileInformationByHandle", last_error()));
        }
        Ok((
            info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            info.nNumberOfLinks,
        ))
    }

    /// `parent`（ディレクトリのハンドル）の中の `file_id` を、**パスを使わず**・辿らずに
    /// 開く（`OpenFileById`、ヒント = 親のハンドル。監査 [A]: パスで開くと途中の要素の
    /// ジャンクションを辿るので、ID で相対に開く）。消えていれば `Ok(None)`。
    fn open_child_by_id(parent: HANDLE, file_id: i64) -> Result<Option<Opened>> {
        use windows_sys::Win32::Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            OpenFileById, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_TYPE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        // windows-sys は `FILE_ID_DESCRIPTOR` の union を公開するので、FileId を入れる。
        let descriptor = windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR>(
            ) as u32,
            Type: 0 as FILE_ID_TYPE, // FileIdType
            Anonymous: windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR_0 {
                FileId: file_id,
            },
        };
        // SAFETY: `parent` は有効なディレクトリのハンドル。`descriptor` は FileId。共有に
        // DELETE を含めない。リンクは辿らない（OPEN_REPARSE_POINT）。
        let handle = unsafe {
            OpenFileById(
                parent,
                &descriptor,
                child_access(),
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let os_error = last_error();
            if os_error == ERROR_FILE_NOT_FOUND || os_error == ERROR_PATH_NOT_FOUND {
                return Ok(None); // 列挙と開くの間に消えた。
            }
            return Err(SecretStoreError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        let handle = OwnedHandle(handle);
        let (is_dir, is_reparse, links) = info_of(handle.0)?;
        Ok(Some(Opened {
            handle,
            is_dir,
            is_reparse,
            links,
        }))
    }

    /// 開いた**ハンドルで**、そのオブジェクトと中の子へ、保護された DACL と所有者を書く
    /// （[`super::PinnedDir::reset_acl`]、#392 B3-3b の監査 Must 2・[A]）。
    ///
    /// - 対象そのもの: `handle`（呼ぶ側が辿らずに開き、reparse でないことを確かめ済み）に
    ///   [`set_object_security`]（自動の伝播をしない）で書く。
    /// - 中の子: **パスを一切使わず**、`handle` を列挙
    ///   （`GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`）して得たファイル ID を、
    ///   [`open_child_by_id`]（ヒント = 親のハンドル）で辿らずに開き、
    ///   - **reparse point** は触らず・辿らず、飛ばして [`super::ResetSkips::reparse`] に記録。
    ///   - **ハードリンク（ファイルでリンク数 2 以上）** は触らず、飛ばして
    ///     [`super::ResetSkips::hardlink`] に記録（記述子を共有するので、書くと外の名前の
    ///     アクセスを変える。#624 は「書き換えない」ことだけ解消。その名前の DACL は直らない）。
    ///   - それ以外は [`set_object_security`] で書き、フォルダなら**その子のハンドルを列挙元
    ///     として**再帰する。
    ///
    /// ハンドルとファイル ID だけで辿るので、確かめてから書くまでの間に子を**その場で**
    /// ジャンクションにされても、列挙はそのディレクトリ（空）そのものを返し、ID で開くのは
    /// その object そのもの - リンクの先には決して向かない。
    pub(super) fn set_security_tree(
        handle: HANDLE,
        real_dir: &Path,
        access: &DirAccess,
        skips: &mut super::ResetSkips,
    ) -> Result<()> {
        ensure_user_account(access)?;
        let sd = owned_protected_sd(access)?;
        set_object_security(handle, &sd)?;
        propagate(handle, real_dir, &sd, skips)
    }

    /// `dir_handle` の中を列挙して、各子にハンドルで書く・再帰する（上の doc）。`display` は
    /// 飛ばした子の報告のための表示用のパス（開くのには使わない）。
    fn propagate(
        dir_handle: HANDLE,
        display: &Path,
        sd: &LocalBox,
        skips: &mut super::ResetSkips,
    ) -> Result<()> {
        use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
            FILE_ID_BOTH_DIR_INFO,
        };

        // 8 バイト境界のバッファ（`FILE_ID_BOTH_DIR_INFO` はポインタ整列が要る）。
        let mut buffer = vec![0u64; 8 * 1024];
        let byte_len = (buffer.len() * 8) as u32;
        let mut class = FileIdBothDirectoryRestartInfo; // 1 回目は最初から。
        loop {
            // SAFETY: `dir_handle` は FILE_LIST_DIRECTORY を持つディレクトリのハンドル。
            // `buffer` は `byte_len` バイト。
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    dir_handle,
                    class,
                    buffer.as_mut_ptr().cast(),
                    byte_len,
                )
            };
            if ok == 0 {
                let e = last_error();
                if e == ERROR_NO_MORE_FILES {
                    return Ok(());
                }
                return Err(api("GetFileInformationByHandleEx", e));
            }
            class = FileIdBothDirectoryInfo; // 2 回目以降は続きから。
            let base = buffer.as_ptr() as *const u8;
            let mut offset = 0usize;
            loop {
                // SAFETY: `offset` は直前の `NextEntryOffset` で進めた、バッファ内の境界。
                let entry = unsafe { &*(base.add(offset) as *const FILE_ID_BOTH_DIR_INFO) };
                let name_len = entry.FileNameLength as usize / 2;
                // SAFETY: `FileName` は可変長で `FileNameLength` バイト続く（バッファ内）。
                let name_u16 =
                    unsafe { std::slice::from_raw_parts(entry.FileName.as_ptr(), name_len) };
                let name = String::from_utf16_lossy(name_u16);
                if name != "." && name != ".." {
                    handle_child(dir_handle, &display.join(&name), entry.FileId, sd, skips)?;
                }
                if entry.NextEntryOffset == 0 {
                    break;
                }
                offset += entry.NextEntryOffset as usize;
            }
        }
    }

    fn handle_child(
        parent: HANDLE,
        display_child: &Path,
        file_id: i64,
        sd: &LocalBox,
        skips: &mut super::ResetSkips,
    ) -> Result<()> {
        let Some(opened) = open_child_by_id(parent, file_id)? else {
            return Ok(()); // 列挙と開くの間に消えた。
        };
        if opened.is_reparse {
            skips.reparse.push(display_child.to_path_buf());
            return Ok(()); // 触らない・辿らない。
        }
        if !opened.is_dir && opened.links >= 2 {
            skips.hardlink.push(display_child.to_path_buf());
            return Ok(()); // 記述子を共有するので触らない（外の名前のアクセスを変えない）。
        }
        set_object_security(opened.handle.raw(), sd)?;
        #[cfg(test)]
        AFTER_CHILD_WRITTEN.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(opened.handle.raw());
            }
        });
        if opened.is_dir {
            // 子のハンドルを列挙元として再帰する（ID とハンドルだけ。パスは使わない）。
            propagate(opened.handle.raw(), display_child, sd, skips)?;
        }
        drop(opened);
        Ok(())
    }

    fn sid_string(sid: PSID) -> Result<String> {
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` は呼び出し元が有効な SID を指すことを保証する。成功すれば
        // `text` は LocalAlloc の NUL 終端文字列。
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return Err(api("ConvertSidToStringSidW", last_error()));
        }
        let text = LocalBox(text as _);
        let ptr = text.0 as *const u16;
        let mut len = 0;
        // SAFETY: NUL 終端まで読む。
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` 文字ぶんは上で読めた範囲。
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        Ok(String::from_utf16_lossy(slice))
    }

    pub(super) fn read_dacl(dir: &Path) -> Result<ObservedDacl> {
        let path = wide(dir);
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `path` は NUL 終端。成功すれば `sd` は LocalAlloc の領域で、
        // `owner`・`dacl` はその中を指す（`sd` を解放するまで有効）。
        let status = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if status != 0 {
            return Err(api("GetNamedSecurityInfoW", status));
        }
        let sd = LocalBox(sd);

        let mut control: u16 = 0;
        let mut revision: u32 = 0;
        // SAFETY: `sd` は有効な記述子。
        if unsafe { GetSecurityDescriptorControl(sd.0, &mut control, &mut revision) } == 0 {
            return Err(api("GetSecurityDescriptorControl", last_error()));
        }
        let owner = sid_string(owner)?;

        let aces = if dacl.is_null() {
            None
        } else {
            // SAFETY: `dacl` は `sd` の中の有効な ACL。
            let count = unsafe { (*dacl).AceCount };
            let mut aces = Vec::with_capacity(count as usize);
            for index in 0..u32::from(count) {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                // SAFETY: index < AceCount。`ace` は ACL の中を指す。
                if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                    return Err(api("GetAce", last_error()));
                }
                // SAFETY: どの ACE も ACE_HEADER で始まる。
                let header = unsafe { *(ace as *const ACE_HEADER) };
                let observed = if header.AceType == ACCESS_ALLOWED_ACE_TYPE
                    || header.AceType == ACCESS_DENIED_ACE_TYPE
                {
                    // 許可・拒否の ACE は ACE_HEADER（4 バイト）・マスク（4 バイト）・
                    // SID の並び（ACCESS_ALLOWED_ACE / ACCESS_DENIED_ACE）。
                    // SAFETY: 種類を確かめた上で、構造体の並びどおりに読む。
                    let mask = unsafe { *((ace as *const u8).add(4) as *const u32) };
                    let sid = unsafe { (ace as *mut u8).add(8) } as PSID;
                    ObservedAce {
                        ace_type: header.AceType,
                        flags: header.AceFlags,
                        mask,
                        sid: sid_string(sid)?,
                    }
                } else {
                    ObservedAce {
                        ace_type: header.AceType,
                        flags: header.AceFlags,
                        mask: 0,
                        sid: String::new(),
                    }
                };
                aces.push(observed);
            }
            Some(aces)
        };
        Ok(ObservedDacl {
            protected: control & SE_DACL_PROTECTED != 0,
            owner,
            aces,
        })
    }

    /// drop で `CloseHandle` するハンドル（[`super::PinnedDir`]）。
    pub(crate) struct OwnedHandle(HANDLE);

    impl OwnedHandle {
        /// 生のハンドル（開いている間だけ使う。閉じない）。
        pub(crate) fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `CreateFileW` が返した有効なハンドル。ここ以外では閉じない。
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// 開いたハンドルの本当のパス（`GetFinalPathNameByHandleW`、`VOLUME_NAME_DOS` +
    /// `FILE_NAME_NORMALIZED`）。先頭の `\\?\`（UNC は `\\?\UNC\` → `\\`）を外す。
    fn final_path_of(handle: HANDLE) -> Result<std::path::PathBuf> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
        };
        let flags = VOLUME_NAME_DOS | FILE_NAME_NORMALIZED;
        // 1 回目は長さを問い合わせる（NUL を含まない文字数を返す）。
        // SAFETY: `handle` は有効。長さ 0・NULL バッファは長さの問い合わせ。
        let needed = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, flags) };
        if needed == 0 {
            return Err(api("GetFinalPathNameByHandleW", last_error()));
        }
        let mut buffer = vec![0u16; needed as usize + 1];
        // SAFETY: `buffer` は `needed + 1` 文字ぶん。戻り値は NUL を含まない文字数。
        let written = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(api("GetFinalPathNameByHandleW", last_error()));
        }
        let text = String::from_utf16_lossy(&buffer[..written as usize]);
        let stripped = text
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| text.strip_prefix(r"\\?\").map(str::to_string))
            .unwrap_or(text);
        Ok(std::path::PathBuf::from(stripped))
    }

    pub(super) fn pin_dir(dir: &Path) -> Result<Option<super::PinnedDir>> {
        use windows_sys::Win32::Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
        };

        let path = wide(dir);
        // SAFETY: `path` は NUL 終端。リンクを辿らず（OPEN_REPARSE_POINT）、
        // ディレクトリも開けるように（BACKUP_SEMANTICS）。共有に DELETE を含めない
        // ので、このハンドルを持っている間は誰も消す・名前を変えることができない。
        // **データのアクセス（`FILE_LIST_DIRECTORY` = `FILE_READ_DATA`）を必ず求める**:
        // 読み取り・書き込み・削除のどれも求めないハンドルは共有の確かめに加わらず、
        // 共有に DELETE を含めなくても名前の変更を止められない（テスト
        // `a_pinned_dir_cannot_be_renamed_away_while_held` で確かめた）。**`WRITE_DAC |
        // WRITE_OWNER`** を求めるのは、このハンドルで `SetKernelObjectSecurity`（DACL と
        // 所有者）を書くため（監査 B2。パスで書くと、確かめた後にその場でリンクにされうる）。
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | WRITE_DAC | WRITE_OWNER,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let os_error = last_error();
            if os_error == ERROR_FILE_NOT_FOUND || os_error == ERROR_PATH_NOT_FOUND {
                return Ok(None);
            }
            return Err(SecretStoreError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        let handle = OwnedHandle(handle);

        // SAFETY: 全フィールドが整数の構造体（ゼロは有効な値）。
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `handle` は有効、`info` は書き込み先。
        if unsafe { GetFileInformationByHandle(handle.0, &mut info) } == 0 {
            return Err(api("GetFileInformationByHandle", last_error()));
        }

        let mut owner: PSID = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `handle` は READ_CONTROL で開いた有効なハンドル。成功すれば `sd` は
        // LocalAlloc の領域で、`owner` はその中を指す（`sd` を解放するまで有効）。
        let status = unsafe {
            GetSecurityInfo(
                handle.0,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if status != 0 {
            return Err(api("GetSecurityInfo", status));
        }
        let sd = LocalBox(sd);
        let owner = sid_string(owner)?;
        drop(sd);

        // 本当のパス（途中の要素のジャンクション・8.3 の短い名前を辿った後）。`VOLUME_NAME_DOS`
        // で `\\?\C:\…` を返すので、先頭の `\\?\`（UNC は `\\?\UNC\`）を外す。
        let final_path = final_path_of(handle.0)?;

        Ok(Some(super::PinnedDir {
            handle,
            path: dir.to_path_buf(),
            final_path,
            info: super::PinnedDirInfo {
                is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                is_reparse_point: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
                owner,
            },
        }))
    }

    pub(super) fn current_user_sid_string() -> Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: 今のプロセスの疑似ハンドル。成功すれば `token` を閉じる責任を持つ。
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(api("OpenProcessToken", last_error()));
        }
        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                // SAFETY: OpenProcessToken が返したハンドル。
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let token = Token(token);
        let mut needed = 0u32;
        // SAFETY: 1 回目は長さを問い合わせるだけ（失敗が正常）。
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            return Err(api("GetTokenInformation", last_error()));
        }
        // TOKEN_USER はポインタを含むので、8 バイト境界のバッファに読む。
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buffer` は `needed` バイト以上。
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr() as _,
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(api("GetTokenInformation", last_error()));
        }
        // SAFETY: 成功した GetTokenInformation(TokenUser) は先頭に TOKEN_USER を書く。
        // その SID は `buffer` の中を指す（`buffer` が生きている間有効）。
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        sid_string(user.User.Sid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    fn user() -> InstallUserSid {
        InstallUserSid::parse(USER).unwrap()
    }

    fn ace(sid: &str, mask: u32) -> ObservedAce {
        ObservedAce {
            ace_type: ACCESS_ALLOWED_ACE_TYPE,
            flags: INHERIT_FLAGS,
            mask,
            sid: sid.to_owned(),
        }
    }

    fn good(owner: &str) -> ObservedDacl {
        ObservedDacl {
            protected: true,
            owner: owner.to_owned(),
            aces: Some(vec![
                ace(SYSTEM_SID, FULL_ACCESS_MASK),
                ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
                ace(USER, MODIFY_ACCESS_MASK),
            ]),
        }
    }

    #[test]
    fn sid_parsing_accepts_only_account_sids_without_sddl_metacharacters() {
        assert_eq!(user().as_str(), USER);
        // Microsoft Entra ID の利用者。
        let entra = "S-1-12-1-1234567890-1234567890-1234567890-1234567890";
        assert_eq!(InstallUserSid::parse(entra).unwrap().as_str(), entra);
        assert!(
            DirAccess::with_install_user(InstallUserSid::parse(entra).unwrap())
                .sddl()
                .ends_with(&format!("(A;OICI;0x1301bf;;;{entra})"))
        );
        for bad in [
            "",
            "S-1-1-0",
            "S-1-5-11",
            "S-1-5-32-545",
            "S-1-5-18",
            "S-1-5-21-",
            "S-1-5-21-1",
            "S-1-5-21-1--2",
            "S-1-5-21-1-2)(A;OICI;FA;;;WD",
            "S-1-5-21-1-2;",
            "s-1-5-21-1-2",
            "S-1-5-21-1-12345678901",
            // 利用者のアカウント以外の領域。
            "S-1-5-32-544",
            "S-1-5-32-547",
            "S-1-5-19",
            "S-1-5-20",
            "S-1-5-4",
            "S-1-5-80-1-2",
            "S-1-12-",
            "S-1-12-1-",
            "S-1-12-1-1",
            "S-1-12-2-1-2",
            "S-1-12-1-1-2)(A;;FA;;;WD",
        ] {
            assert!(
                matches!(
                    InstallUserSid::parse(bad),
                    Err(SecretStoreError::InvalidUserSid(_))
                ),
                "{bad:?} を受け付けた"
            );
        }
    }

    #[test]
    fn sddl_is_protected_and_lists_exactly_the_expected_aces() {
        assert_eq!(
            DirAccess::service_only().sddl(),
            "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)"
        );
        assert_eq!(
            DirAccess::with_install_user(user()).sddl(),
            format!(
                "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1301bf;;;{USER})"
            )
        );
    }

    #[test]
    fn check_accepts_the_exact_shape_in_any_order_and_any_allowed_owner() {
        let access = DirAccess::with_install_user(user());
        for owner in [SYSTEM_SID, ADMINISTRATORS_SID, USER] {
            assert_eq!(check_dacl(&good(owner), &access), Ok(()));
        }
        let mut reordered = good(SYSTEM_SID);
        reordered.aces.as_mut().unwrap().reverse();
        assert_eq!(check_dacl(&reordered, &access), Ok(()));

        let service = ObservedDacl {
            protected: true,
            owner: ADMINISTRATORS_SID.to_owned(),
            aces: Some(vec![
                ace(SYSTEM_SID, FULL_ACCESS_MASK),
                ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
            ]),
        };
        assert_eq!(check_dacl(&service, &DirAccess::service_only()), Ok(()));
    }

    #[test]
    fn check_rejects_every_deviation() {
        let access = DirAccess::with_install_user(user());

        let mut null_dacl = good(SYSTEM_SID);
        null_dacl.aces = None;
        assert_eq!(check_dacl(&null_dacl, &access), Err(AclMismatch::NoDacl));

        let mut unprotected = good(SYSTEM_SID);
        unprotected.protected = false;
        assert_eq!(
            check_dacl(&unprotected, &access),
            Err(AclMismatch::NotProtected)
        );

        let mut inherited = good(SYSTEM_SID);
        inherited.aces.as_mut().unwrap()[0].flags |= 0x10;
        assert!(matches!(
            check_dacl(&inherited, &access),
            Err(AclMismatch::InheritedAce { .. })
        ));

        // Users・Everyone を足すと拒否。
        for extra in ["S-1-5-32-545", "S-1-1-0", "S-1-5-11"] {
            let mut widened = good(SYSTEM_SID);
            widened.aces.as_mut().unwrap().push(ace(extra, 0x0012_00A9));
            assert!(
                matches!(
                    check_dacl(&widened, &access),
                    Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == extra
                ),
                "{extra}"
            );
        }

        // 利用者にフルコントロール（WRITE_DAC を含む）は広すぎる。
        let mut full_user = good(SYSTEM_SID);
        full_user.aces.as_mut().unwrap()[2].mask = FULL_ACCESS_MASK;
        assert!(matches!(
            check_dacl(&full_user, &access),
            Err(AclMismatch::UnexpectedAce { .. })
        ));

        // 拒否の ACE も想定外（形を完全に一致させる）。
        let mut deny = good(SYSTEM_SID);
        deny.aces.as_mut().unwrap().push(ObservedAce {
            ace_type: 1,
            ..ace("S-1-5-32-545", FULL_ACCESS_MASK)
        });
        assert!(matches!(
            check_dacl(&deny, &access),
            Err(AclMismatch::UnexpectedAce { ace_type: 1, .. })
        ));

        // 継承フラグが違う（OI だけ）。
        let mut flags = good(SYSTEM_SID);
        flags.aces.as_mut().unwrap()[1].flags = 0x01;
        assert!(matches!(
            check_dacl(&flags, &access),
            Err(AclMismatch::UnexpectedAce { .. })
        ));

        // 利用者の ACE が無い / 重複している。
        let mut missing = good(SYSTEM_SID);
        missing.aces.as_mut().unwrap().pop();
        assert_eq!(
            check_dacl(&missing, &access),
            Err(AclMismatch::MissingAce {
                sid: USER.to_owned(),
                mask: MODIFY_ACCESS_MASK
            })
        );
        let mut duplicated = good(SYSTEM_SID);
        duplicated
            .aces
            .as_mut()
            .unwrap()
            .push(ace(SYSTEM_SID, FULL_ACCESS_MASK));
        assert!(matches!(
            check_dacl(&duplicated, &access),
            Err(AclMismatch::UnexpectedAce { .. })
        ));

        // サービスだけの構成に利用者の ACE があれば広すぎる。
        assert!(matches!(
            check_dacl(&good(SYSTEM_SID), &DirAccess::service_only()),
            Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == USER
        ));

        // 所有者が許可外（一般の利用者が先に作った）。
        let other = "S-1-5-21-1111111111-2222222222-3333333333-1002";
        assert_eq!(
            check_dacl(&good(other), &access),
            Err(AclMismatch::DisallowedOwner {
                sid: other.to_owned()
            })
        );
        // サービスだけの構成では、インストール利用者も所有者として許さない。
        let service_owned_by_user = ObservedDacl {
            protected: true,
            owner: USER.to_owned(),
            aces: Some(vec![
                ace(SYSTEM_SID, FULL_ACCESS_MASK),
                ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
            ]),
        };
        assert!(matches!(
            check_dacl(&service_owned_by_user, &DirAccess::service_only()),
            Err(AclMismatch::DisallowedOwner { .. })
        ));
    }

    #[test]
    fn modify_mask_excludes_acl_and_owner_rights() {
        const WRITE_DAC: u32 = 0x0004_0000;
        const WRITE_OWNER: u32 = 0x0008_0000;
        assert_eq!(MODIFY_ACCESS_MASK & (WRITE_DAC | WRITE_OWNER), 0);
        assert_eq!(
            FULL_ACCESS_MASK & (WRITE_DAC | WRITE_OWNER),
            WRITE_DAC | WRITE_OWNER
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_fails_closed() {
        let dir = std::path::Path::new("/tmp/banto-secret-store-unsupported");
        let access = DirAccess::service_only();
        assert_eq!(
            ensure_dir(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
        assert_eq!(
            verify_dir(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
        assert_eq!(
            reset_dir_acl(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
        assert_eq!(
            create_dir_if_absent(dir, &access),
            Err(SecretStoreError::UnsupportedPlatform)
        );
        assert_eq!(
            check_user_account(&user()),
            Err(SecretStoreError::UnsupportedPlatform)
        );
    }

    #[cfg(windows)]
    mod windows {
        use super::super::*;

        fn me() -> InstallUserSid {
            current_user_sid().expect("今の利用者の SID")
        }

        /// 昇格した管理者として走っているか（GitHub の Windows のランナーは管理者）。
        fn is_elevated_admin() -> bool {
            banto_service_elev::is_current_process_admin().unwrap_or(false)
        }

        fn has_no_broad_groups(dacl: &ObservedDacl) {
            for ace in dacl.aces.as_ref().unwrap() {
                for broad in ["S-1-5-32-545", "S-1-1-0", "S-1-5-11", "S-1-5-4"] {
                    assert_ne!(ace.sid, broad, "{dacl:?}");
                }
            }
        }

        #[test]
        fn a_created_directory_reads_back_with_the_exact_protected_dacl() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("secrets").join("hub-keys");
            let access = DirAccess::with_install_user(me());
            ensure_dir(&dir, &access).unwrap();

            let dacl = read_dir_dacl(&dir).unwrap();
            assert!(dacl.protected, "継承しない: {dacl:?}");
            let aces = dacl.aces.as_ref().unwrap();
            assert_eq!(aces.len(), 3, "{dacl:?}");
            for ace in aces {
                assert_eq!(ace.flags & 0x10, 0, "継承された ACE: {dacl:?}");
                assert_eq!(ace.ace_type, ACCESS_ALLOWED_ACE_TYPE);
                assert_eq!(ace.flags, INHERIT_FLAGS);
            }
            let find = |sid: &str| aces.iter().find(|a| a.sid == sid).map(|a| a.mask);
            assert_eq!(find(SYSTEM_SID), Some(FULL_ACCESS_MASK));
            assert_eq!(find(ADMINISTRATORS_SID), Some(FULL_ACCESS_MASK));
            assert_eq!(find(me().as_str()), Some(MODIFY_ACCESS_MASK));
            has_no_broad_groups(&dacl);

            // 2 回目は作らずに確かめるだけ（冪等）。
            ensure_dir(&dir, &access).unwrap();
            verify_dir(&dir, &access).unwrap();
        }

        #[test]
        fn a_service_only_directory_has_no_user_ace() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("svc");
            let access = DirAccess::service_only();
            // 一般の利用者として動くテストでは、作った後の確認（所有者 = 自分）は
            // 失敗する（サービスだけの構成を一般の利用者が作るのは想定外）。
            // ここでは作られた DACL の形だけを確かめる。
            let _ = ensure_dir(&dir, &access);
            let dacl = read_dir_dacl(&dir).unwrap();
            assert!(dacl.protected);
            let mut sids: Vec<_> = dacl
                .aces
                .as_ref()
                .unwrap()
                .iter()
                .map(|a| (a.sid.clone(), a.mask))
                .collect();
            sids.sort();
            assert_eq!(
                sids,
                vec![
                    (SYSTEM_SID.to_owned(), FULL_ACCESS_MASK),
                    (ADMINISTRATORS_SID.to_owned(), FULL_ACCESS_MASK),
                ]
            );
            has_no_broad_groups(&dacl);
            // 片付け: 所有者は ACL を書き換えられるので、自分を足してから消す。
            reset_dir_acl(&dir, &DirAccess::with_install_user(me())).unwrap();
        }

        #[test]
        fn an_existing_directory_with_another_acl_is_refused_until_reset() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("plain");
            std::fs::create_dir(&dir).unwrap();
            let access = DirAccess::with_install_user(me());

            let err = ensure_dir(&dir, &access).unwrap_err();
            assert!(
                matches!(
                    err,
                    SecretStoreError::AclMismatch(AclMismatch::NotProtected)
                ),
                "{err:?}"
            );
            // 黙って直さない。
            assert!(!read_dir_dacl(&dir).unwrap().protected);

            reset_dir_acl(&dir, &access).unwrap();
            verify_dir(&dir, &access).unwrap();
        }

        /// #595 P2: グループの SID に「変更」を付けた**既存の**ディレクトリも、
        /// ACL の形の確認より前に SID の検証で拒む。
        #[test]
        fn an_existing_directory_granting_a_group_sid_is_refused() {
            let me = me();
            let machine = me.as_str().rsplit_once('-').unwrap().0;
            let group = InstallUserSid::parse(&format!("{machine}-513")).unwrap();
            let group_access = DirAccess::with_install_user(group);
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("granted-to-group");
            // テストの中で SDDL の形どおりに作る（SID の検証を通らない経路）。
            ensure_dir_with_validated_sid(&dir, &group_access).ok();
            assert!(dir.exists());

            for err in [
                ensure_dir(&dir, &group_access).unwrap_err(),
                verify_dir(&dir, &group_access).unwrap_err(),
                crate::FileSecretStore::open(&dir, group_access.clone(), crate::PlainProtector)
                    .unwrap_err(),
            ] {
                assert!(
                    matches!(err, SecretStoreError::InvalidUserSid(_)),
                    "{err:?}"
                );
            }
            // ACL の形とは関係なく、既存の経路でも SID を先に確かめる（ACL が違う
            // フォルダでも、返るのは AclMismatch ではなく InvalidUserSid）。
            let plain = root.path().join("plain");
            std::fs::create_dir(&plain).unwrap();
            assert!(matches!(
                ensure_dir(&plain, &group_access),
                Err(SecretStoreError::InvalidUserSid(_))
            ));

            // 片付け（所有者は自分なので書き換えられる）。
            reset_dir_acl(&dir, &DirAccess::with_install_user(me)).unwrap();
        }

        /// #392 B2-4a（O6）: 無ければ保護された DACL で作り、既にあれば触らない。
        #[test]
        fn create_dir_if_absent_creates_protected_and_leaves_existing_untouched() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("parent").join("ChronoGazer");
            let access = DirAccess::with_install_user(me());
            assert!(create_dir_if_absent(&dir, &access).unwrap());
            let dacl = read_dir_dacl(&dir).unwrap();
            check_dacl(&dacl, &access).unwrap();
            has_no_broad_groups(&dacl);

            // 2 回目は何もしない（作った形のまま）。
            assert!(!create_dir_if_absent(&dir, &access).unwrap());
            assert_eq!(read_dir_dacl(&dir).unwrap(), dacl);

            // 既にある普通のフォルダ（継承の DACL）は、形が違っても触らない。
            let plain = root.path().join("plain");
            std::fs::create_dir(&plain).unwrap();
            let before = read_dir_dacl(&plain).unwrap();
            assert!(!create_dir_if_absent(&plain, &access).unwrap());
            assert_eq!(read_dir_dacl(&plain).unwrap(), before);
            assert!(!before.protected);

            // 既にあるパスがファイルなら作らずに断る。
            let file = root.path().join("file");
            std::fs::write(&file, b"x").unwrap();
            assert!(matches!(
                create_dir_if_absent(&file, &access),
                Err(SecretStoreError::AclMismatch(AclMismatch::NotADirectory))
            ));
        }

        /// PR #604 のレビュー: `.`・`..` を含むパスは、親も含めて何も作らずに断る
        /// （`ensure_dir` も同じ）。
        #[test]
        fn dot_segments_are_refused_without_creating_anything() {
            let root = tempfile::tempdir().unwrap();
            let real = root.path().join("X");
            let access = DirAccess::with_install_user(me());
            for dir in [
                real.join("child").join(".."),
                real.join("."),
                real.join("..").join("X"),
                std::path::PathBuf::from(format!("{}\\...", real.display())),
            ] {
                assert_eq!(
                    create_dir_if_absent(&dir, &access),
                    Err(SecretStoreError::DotSegmentInPath),
                    "{dir:?}"
                );
                assert_eq!(
                    ensure_dir(&dir, &access),
                    Err(SecretStoreError::DotSegmentInPath),
                    "{dir:?}"
                );
                assert!(!real.exists(), "何も作らない: {dir:?}");
                assert!(!real.join("child").exists());
            }
            // 名前の一部としての点は通す。
            assert!(create_dir_if_absent(&root.path().join("a.b..c"), &access).unwrap());
        }

        /// 作る経路でだけ SID を確かめる: グループの SID では作らない。既にある
        /// フォルダは SID を使わないので、グループの SID でも何もせず `false`。
        #[test]
        fn create_dir_if_absent_checks_the_sid_only_when_creating() {
            let me = me();
            let machine = me.as_str().rsplit_once('-').unwrap().0;
            let group = InstallUserSid::parse(&format!("{machine}-513")).unwrap();
            let group_access = DirAccess::with_install_user(group);
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("g");
            assert!(matches!(
                create_dir_if_absent(&dir, &group_access),
                Err(SecretStoreError::InvalidUserSid(_))
            ));
            assert!(!dir.exists(), "拒んだときはフォルダを作らない");
            assert!(!create_dir_if_absent(root.path(), &group_access).unwrap());
        }

        /// ジャンクション（`mklink /J`。管理者でなくても作れる）を作る。
        fn junction(link: &std::path::Path, target: &std::path::Path) {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .expect("cmd を起動できない");
            assert!(status.success(), "mklink /J に失敗した");
        }

        /// `pin_dir`: 無ければ `None`、普通のフォルダはディレクトリ・reparse point で
        /// ない・所有者は自分、ジャンクションは辿らずに reparse point として見え、
        /// `reset_acl` は何もせずに断る（先の DACL も変えない）。ファイルも断る。
        #[test]
        fn pin_dir_reads_the_object_itself_without_following_links() {
            let root = tempfile::tempdir().unwrap();
            assert!(pin_dir(&root.path().join("absent")).unwrap().is_none());

            let plain = root.path().join("plain");
            std::fs::create_dir(&plain).unwrap();
            let pinned = pin_dir(&plain).unwrap().unwrap();
            assert!(pinned.info().is_dir);
            assert!(!pinned.info().is_reparse_point);
            // 作った直後の所有者は、作った人（一般の利用者なら自分、昇格した管理者なら
            // 既定で Administrators）。本番の確かめ（`data_acl_plan`）はこの両方を許す。
            // Must 3: CI の管理者で `me()` と決めつけると落ちていた。
            assert!(
                pinned.info().owner == me().as_str() || pinned.info().owner == ADMINISTRATORS_SID,
                "作った直後の所有者が想定外: {:?}",
                pinned.info()
            );
            assert_eq!(pinned.path(), plain.as_path());
            // 本当のパス（`GetFinalPathNameByHandleW`）は、`canonicalize`（これも `\\?\` を
            // 付ける）と、`\\?\` を外せば一致する。
            assert_eq!(
                pinned.final_path(),
                std::fs::canonicalize(&plain)
                    .unwrap()
                    .to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .parse::<std::path::PathBuf>()
                    .unwrap()
            );
            let reset = pinned.reset_acl(&DirAccess::with_install_user(me()));
            drop(pinned);
            if is_elevated_admin() {
                // #392 B3-3b の監査 B5: 書き直すと所有者は Administrators に移り、
                // DACL は期待の形（インストール利用者の「変更」は残る）。子はいないので
                // 飛ばした記録は空。
                assert_eq!(reset.unwrap(), ResetSkips::default());
                let dacl = read_dir_dacl(&plain).unwrap();
                assert_eq!(dacl.owner, ADMINISTRATORS_SID, "{dacl:?}");
                check_dacl(&dacl, &DirAccess::with_install_user(me())).unwrap();
            } else {
                // 昇格していなければ所有者を Administrators にできず、何も変えずに失敗する
                // （所有者を移す確かめは管理者で走る CI で行う）。
                eprintln!(
                    "skip: 昇格していないので、所有者を Administrators に移す確かめは CI で行う"
                );
                assert_eq!(
                    reset,
                    Err(SecretStoreError::AclApi {
                        call: "SetKernelObjectSecurity",
                        os_error: 1307
                    })
                );
                let dacl = read_dir_dacl(&plain).unwrap();
                assert!(!dacl.protected, "失敗したのに DACL が変わった: {dacl:?}");
            }

            let outside = tempfile::tempdir().unwrap();
            let link = root.path().join("link");
            junction(&link, outside.path());
            let before = read_dir_dacl(outside.path()).unwrap();
            let pinned = pin_dir(&link).unwrap().unwrap();
            assert!(pinned.info().is_reparse_point, "{:?}", pinned.info());
            assert_eq!(
                pinned.reset_acl(&DirAccess::with_install_user(me())),
                Err(SecretStoreError::AclMismatch(AclMismatch::ReparsePoint))
            );
            drop(pinned);
            assert_eq!(read_dir_dacl(outside.path()).unwrap(), before);
            std::fs::remove_dir(&link).unwrap();

            let file = root.path().join("file");
            std::fs::write(&file, b"x").unwrap();
            let pinned = pin_dir(&file).unwrap().unwrap();
            assert!(!pinned.info().is_dir);
            assert_eq!(
                pinned.reset_acl(&DirAccess::with_install_user(me())),
                Err(SecretStoreError::AclMismatch(AclMismatch::NotADirectory))
            );
            assert_eq!(
                pin_dir(&root.path().join("x").join("..").join("plain")).unwrap_err(),
                SecretStoreError::DotSegmentInPath
            );
        }

        /// `pin_dir` の反証（監査 Must 1）: 途中の要素がジャンクションでも、`final_path` は
        /// 辿った後の本当のパスを返す。呼ぶ側はこれを期待のパスと比べて、別の場所を
        /// 開かされていないか分かる。
        #[test]
        fn final_path_resolves_a_junction_in_the_middle() {
            let base = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::create_dir(outside.path().join("ChronoGazer")).unwrap();
            // base\link → outside。開くのは base\link\ChronoGazer（最後の要素ではなく
            // 途中の link がジャンクション）。
            junction(&base.path().join("link"), outside.path());
            let opened = base.path().join("link").join("ChronoGazer");
            let pinned = pin_dir(&opened).unwrap().unwrap();
            // 開いたパス（見かけ）は base\link\ChronoGazer のまま。
            assert_eq!(pinned.path(), opened.as_path());
            // 本当のパスは outside\ChronoGazer（link を辿った後）。
            let real = std::fs::canonicalize(outside.path().join("ChronoGazer")).unwrap();
            assert_eq!(
                pinned.final_path().to_string_lossy().to_lowercase(),
                real.to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_lowercase()
            );
            assert_ne!(
                pinned.final_path().to_string_lossy().to_lowercase(),
                opened.to_string_lossy().to_lowercase()
            );
            drop(pinned);
            std::fs::remove_dir(base.path().join("link")).unwrap();
        }

        /// 【測った事実・OS の自動伝播】パスの版 `reset_dir_acl`（`SetNamedSecurityInfoW`）は、
        /// 書き直すと**既にある**中の普通の子へ継承の ACE を OS が自動で伝える（所有者は
        /// 変えないので昇格は要らない）。これは `PinnedDir::reset_acl` が**使わない**経路だが、
        /// 「OS の自動伝播は実在する（だから自前の再帰では使わない）」という前提の確認。
        #[test]
        fn os_auto_propagation_reaches_existing_children() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let child = dir.join("child");
            std::fs::create_dir(&child).unwrap();
            let file = dir.join("f.txt");
            std::fs::write(&file, b"x").unwrap();
            reset_dir_acl(&dir, &DirAccess::with_install_user(me())).unwrap();
            for path in [&child, &file] {
                let dacl = read_dir_dacl(path).unwrap();
                let aces = dacl.aces.as_ref().unwrap();
                assert!(
                    aces.iter()
                        .any(|a| a.sid == SYSTEM_SID && a.flags & 0x10 != 0),
                    "{}: 継承された ACE が伝わっていない: {dacl:?}",
                    path.display()
                );
                assert!(aces.iter().any(|a| a.sid == me().as_str()));
            }
        }

        /// 【測った事実・自前の再帰】`PinnedDir::reset_acl`（ハンドルの `SetKernelObjectSecurity`
        /// ＋自前の再帰）は、中の普通の子へ保護された DACL と Administrators 所有を書き、
        /// **子のジャンクションとハードリンクは触らず飛ばす**（`ResetSkips` に記録）。所有者を
        /// 移すので昇格が要る。非管理者では 1307 で止まり（子を書く前）、CI で確かめる。
        #[test]
        fn self_recursion_writes_children_and_skips_links() {
            if !is_elevated_admin() {
                eprintln!(
                    "skip self_recursion_writes_children_and_skips_links: 昇格していない（CI で確かめる）"
                );
                return;
            }
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let normal = dir.join("normal");
            std::fs::create_dir(&normal).unwrap();
            let deep = normal.join("deep.txt");
            std::fs::write(&deep, b"x").unwrap();
            let outside = tempfile::tempdir().unwrap();
            let outside_before = read_dir_dacl(outside.path()).unwrap();
            junction(&dir.join("jlink"), outside.path());
            let outside_file = outside.path().join("shared.txt");
            std::fs::write(&outside_file, b"x").unwrap();
            let outside_file_before = read_dir_dacl(&outside_file).unwrap();
            std::fs::hard_link(&outside_file, dir.join("hard.txt")).unwrap();

            let pinned = pin_dir(&dir).unwrap().unwrap();
            let real_dir = pinned.final_path().to_path_buf();
            let access = DirAccess::with_install_user(me());
            let skips = pinned.reset_acl(&access).unwrap();
            drop(pinned);

            // 普通の子（孫まで）は保護された DACL ＋ Administrators 所有。
            for path in [&dir, &normal, &deep] {
                let dacl = read_dir_dacl(path).unwrap();
                assert_eq!(
                    dacl.owner,
                    ADMINISTRATORS_SID,
                    "{}: {dacl:?}",
                    path.display()
                );
                check_dacl(&dacl, &access).unwrap();
            }
            // ジャンクション・ハードリンクは飛ばして記録。先は変えない。
            // 記録は本当のパス（TEMP が 8.3 の短い名前でも長い名前で返る）。
            assert_eq!(skips.reparse, vec![real_dir.join("jlink")]);
            assert_eq!(skips.hardlink, vec![real_dir.join("hard.txt")]);
            assert_eq!(read_dir_dacl(outside.path()).unwrap(), outside_before);
            assert_eq!(read_dir_dacl(&outside_file).unwrap(), outside_file_before);

            std::fs::remove_dir(dir.join("jlink")).unwrap();
        }

        /// 監査 Must 2・受入 (a): ルートの中の子のジャンクションの先（outside）の DACL は、
        /// `reset_acl` で変わらない（昇格していれば飛ばして記録、していなければ 1307。どちらでも
        /// outside は不変）。
        #[test]
        fn reset_acl_does_not_follow_a_child_junction() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("f.txt"), b"x").unwrap();
            junction(&dir.join("jlink"), outside.path());
            let before = read_dir_dacl(outside.path()).unwrap();
            let before_file = read_dir_dacl(&outside.path().join("f.txt")).unwrap();

            let pinned = pin_dir(&dir).unwrap().unwrap();
            let _ = pinned.reset_acl(&DirAccess::with_install_user(me()));
            drop(pinned);

            assert_eq!(read_dir_dacl(outside.path()).unwrap(), before);
            assert_eq!(
                read_dir_dacl(&outside.path().join("f.txt")).unwrap(),
                before_file
            );
            std::fs::remove_dir(dir.join("jlink")).unwrap();
        }

        /// 監査 Must 2・受入 (b)・#624: ルートの中のハードリンク（outside のファイルへ）の
        /// 先の DACL は、`reset_acl` で変わらない（ハードリンクは触らず飛ばす）。
        #[test]
        fn reset_acl_does_not_rewrite_a_hardlink_target() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("shared.txt");
            std::fs::write(&target, b"x").unwrap();
            let before = read_dir_dacl(&target).unwrap();
            std::fs::hard_link(&target, dir.join("hard.txt")).unwrap();

            let pinned = pin_dir(&dir).unwrap().unwrap();
            let _ = pinned.reset_acl(&DirAccess::with_install_user(me()));
            drop(pinned);

            assert_eq!(
                read_dir_dacl(&target).unwrap(),
                before,
                "ハードリンクの先が変わった"
            );
        }

        /// 監査 Must 2・受入 (c)（強化）: pin を持ったまま、ルートを**その場で**
        /// `FSCTL_SET_REPARSE_POINT` でジャンクションにしても、`reset_acl` はハンドルとファイル
        /// ID だけで辿るので、リンクの先（outside）の**中身（ファイル・サブフォルダ）の DACL と
        /// 所有者**は変わらない。
        #[test]
        fn reset_acl_on_a_handle_turned_into_a_junction_stays_on_the_link() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let outside = tempfile::tempdir().unwrap();
            let out_file = outside.path().join("f.txt");
            std::fs::write(&out_file, b"x").unwrap();
            let out_sub = outside.path().join("sub");
            std::fs::create_dir(&out_sub).unwrap();
            let watched = [
                outside.path().to_path_buf(),
                out_file.clone(),
                out_sub.clone(),
            ];
            let before: Vec<_> = watched.iter().map(|p| read_dir_dacl(p).unwrap()).collect();

            let pinned = pin_dir(&dir).unwrap().unwrap();
            // pin を持ったまま、別のハンドルで dir をジャンクション（→ outside）にする。
            match set_reparse_to(&dir, outside.path()) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("skip: FSCTL_SET_REPARSE_POINT に権限が無い（{e}）- CI で確かめる");
                    return;
                }
            }
            // 昇格していれば成功（dir の object そのものに書く）、していなければ 1307。
            // どちらでも outside の中身は不変。
            let _ = pinned.reset_acl(&DirAccess::with_install_user(me()));
            drop(pinned);
            let after: Vec<_> = watched.iter().map(|p| read_dir_dacl(p).unwrap()).collect();
            assert_eq!(
                after, before,
                "ジャンクションの先の中身の DACL/所有者が変わった"
            );
            std::fs::remove_dir(&dir).unwrap();
        }

        /// 監査 [A] の実測（管理者でなくても走る）: 空のフォルダのハンドルを開き、別ハンドルで
        /// その場でジャンクション（→ outside）に変えても、**元のハンドルの列挙**はそのフォルダ
        /// そのもの（空）を返す - リンクの先（outside）の中身は返らない。これが自前の再帰が
        /// 安全な根拠（列挙も開くもパスを使わずハンドル／ID だけ）。
        #[test]
        fn enumerating_a_handle_turned_into_a_junction_lists_the_empty_object() {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::{
                CloseHandle, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE,
            };
            use windows_sys::Win32::Storage::FileSystem::{
                CreateFileW, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
                FILE_LIST_DIRECTORY, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            };

            let root = tempfile::tempdir().unwrap();
            let empty = root.path().join("empty");
            std::fs::create_dir(&empty).unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.txt"), b"x").unwrap();

            let wide: Vec<u16> = empty
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: NUL 終端。列挙の権利で、辿らずに開く。
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(handle, INVALID_HANDLE_VALUE);

            // ハンドルを持ったまま、別ハンドルで empty をジャンクションに変える。
            if let Err(e) = set_reparse_to(&empty, outside.path()) {
                // SAFETY: 上で開いた有効なハンドル。
                unsafe { CloseHandle(handle) };
                eprintln!("skip: FSCTL に権限が無い（{e}）- CI で確かめる");
                return;
            }

            // 元のハンドルを列挙する。empty そのもの（空）が返り、outside の中身は返らない。
            let mut buffer = vec![0u64; 8 * 1024];
            // SAFETY: `handle` は FILE_LIST_DIRECTORY を持つ。`buffer` は十分大きい。
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileIdBothDirectoryRestartInfo,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 8) as u32,
                )
            };
            let mut names = Vec::new();
            if ok != 0 {
                let base = buffer.as_ptr() as *const u8;
                let mut offset = 0usize;
                loop {
                    // SAFETY: バッファ内の境界。
                    let entry = unsafe { &*(base.add(offset) as *const FILE_ID_BOTH_DIR_INFO) };
                    let len = entry.FileNameLength as usize / 2;
                    // SAFETY: FileName は FileNameLength バイト続く。
                    let n = unsafe { std::slice::from_raw_parts(entry.FileName.as_ptr(), len) };
                    names.push(String::from_utf16_lossy(n));
                    if entry.NextEntryOffset == 0 {
                        break;
                    }
                    offset += entry.NextEntryOffset as usize;
                }
            } else {
                assert_eq!(last_error_test(), ERROR_NO_MORE_FILES);
            }
            // SAFETY: 上で開いた有効なハンドル。
            unsafe { CloseHandle(handle) };
            let _ = std::fs::remove_dir(&empty);

            // `.`・`..` 以外は無い（outside の secret.txt は見えない）。
            let real: Vec<_> = names.iter().filter(|n| *n != "." && *n != "..").collect();
            assert!(
                real.is_empty(),
                "ジャンクションに変わったハンドルの列挙が中身を返した: {real:?}"
            );
            assert!(!names.iter().any(|n| n == "secret.txt"));
        }

        fn last_error_test() -> u32 {
            // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
            unsafe { windows_sys::Win32::Foundation::GetLastError() }
        }

        /// 監査 Must 2・受入 (d): **再帰の途中で**子（空のフォルダ）をその場でジャンクションに
        /// すり替えても、outside の中身は変わらない。`AFTER_CHILD_WRITTEN` フックで、子に書いた
        /// 直後に `empty` をジャンクション（→ outside）に変える。所有者を移すので、子まで進むのは
        /// 管理者のとき（CI）。非管理者では 1307 で子へ進まず、フックは発火しないが outside は
        /// 不変（どちらでも受入は満たす）。
        #[test]
        fn a_child_swapped_to_a_junction_mid_recursion_does_not_leak() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            // 普通のファイル（フックの発火のきっかけ）と、空のサブフォルダ `empty`。
            std::fs::write(dir.join("a.txt"), b"x").unwrap();
            let empty = dir.join("empty");
            std::fs::create_dir(&empty).unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret.txt"), b"x").unwrap();
            let watched = [
                outside.path().to_path_buf(),
                outside.path().join("secret.txt"),
            ];
            let before: Vec<_> = watched.iter().map(|p| read_dir_dacl(p).unwrap()).collect();

            // 子に書いた直後、まだ `empty` が空のうちにジャンクションへ変える（1 回だけ試みる）。
            let empty_for_hook = empty.clone();
            let outside_for_hook = outside.path().to_path_buf();
            let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fired2 = fired.clone();
            super::super::imp::AFTER_CHILD_WRITTEN.with(|h| {
                *h.borrow_mut() = Some(Box::new(move |_child_handle| {
                    if !fired2.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let _ = set_reparse_to(&empty_for_hook, &outside_for_hook);
                    }
                }));
            });
            let pinned = pin_dir(&dir).unwrap().unwrap();
            let result = pinned.reset_acl(&DirAccess::with_install_user(me()));
            drop(pinned);
            super::super::imp::AFTER_CHILD_WRITTEN.with(|h| *h.borrow_mut() = None);

            let admin = is_elevated_admin();
            if !admin {
                eprintln!(
                    "note: 非管理者なので子まで進まず（1307）、フックは発火していない - 書く側は CI"
                );
            }
            let after: Vec<_> = watched.iter().map(|p| read_dir_dacl(p).unwrap()).collect();
            assert_eq!(after, before, "再帰の途中のすり替えで outside が変わった");
            // 後片付け（`empty` がジャンクションなら外す）。
            let _ = (result, std::fs::remove_dir(&empty));
        }

        /// `dir` を `target` へのジャンクションにする（`FSCTL_SET_REPARSE_POINT`。`dir` は
        /// 空のフォルダ。テスト専用）。
        fn set_reparse_to(dir: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::{
                CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE,
            };
            use windows_sys::Win32::Storage::FileSystem::{
                CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
                FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            };
            use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
            use windows_sys::Win32::System::IO::DeviceIoControl;

            // `\??\<target>` を substitute name にした mount-point の reparse データ。
            let sub: Vec<u16> = std::ffi::OsStr::new(r"\??\")
                .encode_wide()
                .chain(target.as_os_str().encode_wide())
                .collect();
            let sub_bytes = (sub.len() * 2) as u16;
            let print_bytes = 0u16;
            // IO_REPARSE_TAG_MOUNT_POINT = 0xA0000003。
            let mut buf: Vec<u8> = Vec::new();
            buf.extend_from_slice(&0xA000_0003u32.to_le_bytes()); // ReparseTag
            let data_len = 8 + sub_bytes as usize + 2 + print_bytes as usize + 2;
            buf.extend_from_slice(&(data_len as u16).to_le_bytes()); // ReparseDataLength
            buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
            buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
            buf.extend_from_slice(&sub_bytes.to_le_bytes()); // SubstituteNameLength
            buf.extend_from_slice(&(sub_bytes + 2).to_le_bytes()); // PrintNameOffset
            buf.extend_from_slice(&print_bytes.to_le_bytes()); // PrintNameLength
            for w in &sub {
                buf.extend_from_slice(&w.to_le_bytes());
            }
            buf.extend_from_slice(&0u16.to_le_bytes()); // substitute NUL
            buf.extend_from_slice(&0u16.to_le_bytes()); // print NUL

            let path: Vec<u16> = dir
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `path` は NUL 終端。書き込みで開く（reparse を設定するため）。
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            let mut returned = 0u32;
            // SAFETY: `handle` は有効、`buf` は reparse データ。
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_SET_REPARSE_POINT,
                    buf.as_ptr().cast(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            let err = std::io::Error::last_os_error();
            // SAFETY: `handle` は CreateFileW が返したもの。
            unsafe {
                CloseHandle(handle);
            }
            if ok == 0 {
                return Err(err);
            }
            Ok(())
        }

        /// `pin_dir` の反証: ハンドルを持っている間は、そのフォルダの名前を変えて
        /// ジャンクションにすり替える・消すことができない（放せばできる）。
        #[test]
        fn a_pinned_dir_cannot_be_renamed_away_while_held() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("pinned");
            std::fs::create_dir(&dir).unwrap();
            let moved = root.path().join("moved");
            let pinned = pin_dir(&dir).unwrap().unwrap();
            assert!(
                std::fs::rename(&dir, &moved).is_err(),
                "持っている間に名前を変えられた"
            );
            assert!(std::fs::remove_dir(&dir).is_err(), "持っている間に消せた");
            drop(pinned);
            std::fs::rename(&dir, &moved).unwrap();
        }

        #[test]
        fn check_user_account_accepts_me_and_refuses_a_group() {
            check_user_account(&me()).unwrap();
            let me = me();
            let machine = me.as_str().rsplit_once('-').unwrap().0;
            let group = InstallUserSid::parse(&format!("{machine}-513")).unwrap();
            assert!(matches!(
                check_user_account(&group),
                Err(SecretStoreError::InvalidUserSid(_))
            ));
        }

        #[test]
        fn a_group_sid_is_refused_as_the_install_user() {
            // この PC の「None」/「Domain Users」等は S-1-5-21-…-513。
            let me = me();
            let machine = me.as_str().rsplit_once('-').unwrap().0;
            let group = InstallUserSid::parse(&format!("{machine}-513")).unwrap();
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("g");
            let err = ensure_dir(&dir, &DirAccess::with_install_user(group)).unwrap_err();
            assert!(
                matches!(err, SecretStoreError::InvalidUserSid(_)),
                "{err:?}"
            );
            assert!(!dir.exists(), "拒んだときはフォルダを作らない");
        }
    }
}
