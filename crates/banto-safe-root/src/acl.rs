//! 保護するディレクトリの DACL（決定 4、計画書 §3.1(c)・§3.3）。
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
//! 利用者を複数にした [`DirAccess::with_install_users`]（#626 PR-A）では、最後の行が
//! 利用者ごとに 1 つずつ並び（重複はまとめる）、所有者はそのどの利用者でもよい。
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
//!   確かめ、違えば [`SafeRootError::AclMismatch`] で失敗する（fail closed）。
//!   **自動では直さない**（黙って広げることも狭めることもしない。違う形には、
//!   管理者が意図して変えた・別の製品が使っている・乗っ取りの準備、のどれも
//!   ありうるため）。直すのは [`reset_dir_acl`] の明示的な呼び出しだけで、
//!   B3-3 の昇格ヘルパ（管理者の操作）から使う想定。
//! - 保存の層（`banto-secret-store` の `FileSecretStore`）は開くときだけでなく、
//!   **読む・書く・消すたびに**この確認をする（途中で ACL が変えられたら、それ以降は
//!   使わない）。
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
//! 保存の層（`FileSecretStore::open`）は 1 回だけ
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
//! ディレクトリ単位の確認とは別に、**データのルート**（ChronoGazer の
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
//!   所有者。**自動の継承の伝播をしない** API）を書く（監査 B2・\[A\]。パスで
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
//!   既にある子へ継承の ACE を書く。**ハードリンクの子は記述子を共有するので、木の外の
//!   名前のアクセスも変わる**（#624）。子の**ジャンクション**の先には届かないと測定した
//!   （Windows 11 26300、2026-10-16。#626 PR-B の監査 C-5 と、banto-hub の旧実装での反証。
//!   書き換わったのはリンクそのものの DACL だけ）。**ディレクトリのシンボリックリンクは
//!   未測**（Microsoft の仕様は自動の伝播の範囲を明言していない - #626 のオーナーレビュー
//!   P1）。そこで
//!   `set_security_tree` は自動の伝播を使わず、対象そのものは `SetKernelObjectSecurity`
//!   （伝播しない API）で書き、継承は自前で子へ伝える。**パスは一切使わない**（監査 \[A\]。
//!   パスで列挙・パスで開くと、確かめた後に途中の要素をジャンクションに変えられて先を
//!   辿る）: 子は、親のディレクトリの**ハンドル**を
//!   `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)` で列挙して**128 ビットの**ファイル ID を取り（監査 Nit 2。ReFS・Dev Drive でも正しい）、
//!   `OpenFileById`（`ExtendedFileIdType`、ヒント = 親のハンドル、`FILE_FLAG_OPEN_REPARSE_POINT`）で辿らずに開く。
//!   開いた後に属性をハンドルから確かめ、**reparse point は飛ばし**（[`ResetSkips::reparse`]）、
//!   **ハードリンク（リンク数 2 以上のファイル）も飛ばし**（[`ResetSkips::hardlink`]。記述子を
//!   共有するので書くと木の外の名前のアクセスを変える。#624）、それ以外だけに同じ記述子を
//!   書いて、フォルダならその子のハンドルを列挙元として再帰する。`.`・`..` は除く。
//!   **開けない子**（利用者が Administrators を拒否した・所有権が無い。`ERROR_ACCESS_DENIED`）は
//!   特権を有効にせず、触らず飛ばして [`ResetSkips::denied`] に記録する（監査 Should 1。そのまま
//!   残る）。消えた子は飛ばし、それ以外の失敗は fail closed。ファイルには継承のフラグ（`OICI`）を
//!   外した記述子を書く（監査 Nit 4。フォルダは `OICI`）。再帰の深さは上限 64 で、超えたら
//!   1 で止める（監査 Nit 1。とても深いフォルダでスタックをあふれさせない）。
//!   - 【測った事実・自前の再帰】`PinnedDir::reset_acl` は、中の普通の子（孫まで）へ保護された
//!     DACL と Administrators 所有を書き、子のジャンクションとハードリンクは飛ばして
//!     `ResetSkips` に記録する。リンクの先（ルートの外）は変えない（テスト
//!     `self_recursion_writes_children_and_skips_links`・`reset_acl_does_not_follow_a_child_junction`・
//!     `reset_acl_does_not_rewrite_a_hardlink_target`・その場のすり替えは
//!     `reset_acl_on_a_handle_turned_into_a_junction_stays_on_the_link`。所有者を移すので
//!     昇格が要り、非管理者では 1307 で止まる - 書く側は CI で確かめる）。
//!   - 【測った事実・OS の自動伝播】パスの版 `reset_dir_acl`（`SetNamedSecurityInfoW`）は、
//!     既にある普通の子へ継承の ACE を OS が自動で伝える（テスト
//!     `os_auto_propagation_reaches_existing_children`）。ハードリンクの子では木の外の名前にも
//!     効き、ディレクトリのシンボリックリンクの先は未測なので、昇格ヘルパーの書き直しでは
//!     使わない（ジャンクションの先には届かないと測定 - 上）。
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
//! # 置き場とファイルが保護されているか・ハンドル基準の子のファイル（#630）
//!
//! banto-hub-sink の設定ファイル（Hub の API キー入り）を、SYSTEM・Administrators だけの
//! 置き場 `%ProgramData%\BantoHub\sink\` に置き、**保護されていなければ起動しない**
//! （2026-10-17 オーナー決定）ための部品。sink・`banto-hub-elev` への配線は #630 の PR-2。
//!
//! - **辿らずに読む**: [`inspect_file`] はパスを `FILE_FLAG_OPEN_REPARSE_POINT |
//!   FILE_FLAG_BACKUP_SEMANTICS` で開き（最後の要素がリンクでもそのもの。中身は求めない）、
//!   属性・リンク数・所有者・DACL を**開いたハンドルから**読んで [`FileSecurity`] で返す。
//!   ACE の読み方は `PinnedDir::read_dacl` と同じ（#641: 条件付き・オブジェクトの ACE の
//!   SID も読み、種類の分からない・形の壊れた ACE は SID を空にする）。確かめ済みのフォルダの
//!   `PinnedDir::security` も同じ形で返す。
//! - **判定は純関数**: [`exposure`] は [`FileSecurity`] から「保護されていない」理由を、
//!   1 条件 1 理由の [`Exposure`] ですべて返す（空なら保護されている）。reparse point・
//!   ファイルでリンク数 2 以上・所有者が SYSTEM・Administrators・TrustedInstaller 以外・
//!   null DACL・それら以外への**許可**の ACE（条件付き・オブジェクトの許可、継承・
//!   `INHERIT_ONLY` の ACE も）・種類の分からない ACE・SID が読めない ACE。拒否の ACE は
//!   狭める方向なので、それ自体では理由にしない。実効権限は計算せず、ACE の SID の
//!   許可リストで判定する（グループの入れ子・条件の評価をしない分、多めに断る側に倒す）。
//! - **ハンドル基準の子のファイル**: `PinnedDir::open_child_file` は確かめ済みのフォルダの
//!   ハンドルを基準に、名前 1 要素の子を辿らずに読み取りだけで開き（共有も読み取りだけ）、
//!   `PinnedFile::read_to_end` は**そのハンドルのまま**読む（確かめてから読むまでの間に
//!   すり替えられない）。`PinnedDir::create_child_file` は新しいファイルを、保護された DACL
//!   （SYSTEM・Administrators にフル、継承のフラグなし）と所有者 Administrators を**作るのと
//!   同時に**付けて作り、書いて書き出し、読み戻して [`exposure`] が空であることを確かめる。
//!   途中で失敗した・保護されていなかったときは、作ったハンドルで（`FileDispositionInfo`）
//!   消してから失敗を返す。どれも `PinnedDir::create_child_dir`・`PinnedDir::open_child_dir`
//!   と同じ `NtCreateFile` の相対の作り（`child_relative`）で、名前は
//!   [`guard::is_plain_child_name`] で確かめる。
//! - **ハンドル基準で整え直す・消す**（#630 PR-2。`banto-hub-elev prepare-sink-config`）:
//!   `PinnedDir::open_child_file_for_update` は同じ作りで、中身を求めずに記述子を書く・消す
//!   権利（`WRITE_DAC | WRITE_OWNER | DELETE`）で開く。`PinnedFile::reset_acl` はそのハンドルに
//!   ファイル用の保護された DACL と所有者 Administrators を書き（書く直前に属性を読み直し、
//!   reparse point・フォルダ・ハードリンクには書かない）、読み戻して [`exposure`] を確かめる。
//!   `PinnedFile::delete` はそのハンドルで（`FileDispositionInfo`）消す。
//! - 対象の利用者を持たない置き場の、作る・書き直す・断るの判断は
//!   [`guard::dir_acl_plan`]（`install_user` に `None`）。
//!
//! # 非 Windows
//!
//! DACL の概念が無いので、作る・確かめる・直す・[`inspect_file`] はどれも
//! [`SafeRootError::UnsupportedPlatform`] を返す（黙って成功にしない）。
//! [`InstallUserSid`] の文字列の検査と [`check_dacl`]・[`exposure`] の判定は純関数で、
//! 全 OS でテストする。

use crate::error::{AclMismatch, Result, SafeRootError};
use crate::guard;

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
/// NTFS の「読み取りと実行」= `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`（運用モードで
/// 利用者に残す権利。#623 PR-1c、設計 §2.8）。`FILE_WRITE_ATTRIBUTES` も `DELETE` も含まない
/// ので、属性の変更（`fsutil file setCaseSensitiveInfo` など）も名前の作成もできない。
pub const READ_EXECUTE_ACCESS_MASK: u32 = 0x0012_00A9;
/// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`。
pub const INHERIT_FLAGS: u8 = 0x03;
/// `INHERITED_ACE`。
pub(crate) const INHERITED_ACE_FLAG: u8 = 0x10;
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
/// 拒む（fail closed、[`SafeRootError::InvalidUserSid`]）**。インストールは
/// その利用者自身のサインイン中に行う（B3-3 の昇格ヘルパは呼び出した利用者の
/// SID を渡す）ので、通常は解決できる。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallUserSid(String);

impl InstallUserSid {
    pub fn parse(s: &str) -> Result<Self> {
        const PREFIXES: [&str; 2] = ["S-1-5-21-", "S-1-12-1-"];
        let Some(rest) = PREFIXES.iter().find_map(|prefix| s.strip_prefix(prefix)) else {
            return Err(SafeRootError::InvalidUserSid(
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
            return Err(SafeRootError::InvalidUserSid("SID の形式ではありません"));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 利用者に付ける権利（#623 PR-1c、設計 §2.2）。モードの状態そのものを ACL で表す:
/// 設定モード = 利用者に「変更」、運用モード = 利用者に「読み取りと実行」。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserRight {
    /// 「変更」[`MODIFY_ACCESS_MASK`]（設定モード。デスクトップのエンジンが書く）。
    Modify,
    /// 「読み取りと実行」[`READ_EXECUTE_ACCESS_MASK`]（運用モード。シェルが
    /// `run\endpoint.json`・`profile.lock`・ログを読むだけ。名前を作れない）。
    Read,
}

impl UserRight {
    /// この権利のアクセスマスク。
    pub fn mask(self) -> u32 {
        match self {
            Self::Modify => MODIFY_ACCESS_MASK,
            Self::Read => READ_EXECUTE_ACCESS_MASK,
        }
    }
}

/// ディレクトリに付ける権限の指定。
///
/// 利用者は 0 人（[`DirAccess::service_only`]）・1 人（[`DirAccess::with_install_user`]）・
/// 複数（[`DirAccess::with_install_users`]、#626 PR-A。banto-hub の profile は owner が複数で
/// ありうる）。利用者の権利は [`UserRight`] で、全員に同じものを付ける（「変更」=
/// 設定モード、「読み取りと実行」= 運用モード。[`DirAccess::with_readers`]・
/// [`DirAccess::to_right`]。#623 PR-1c）。利用者ごとに権利を違える形は作らない - 木が
/// 「半分は設定モード・半分は運用モード」になり、`classify_mode` で見分けられなくなるため。
/// どの形でも DACL は保護され、SYSTEM・Administrators のフルと、利用者ごとの ACE が
/// ちょうど 1 つずつ並ぶ。利用者が 0 人のとき権利は常に [`UserRight::Modify`] に揃える
/// （DACL は同じなので、値としても同じにする）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirAccess {
    /// 重複なし・指定の順。
    install_users: Vec<InstallUserSid>,
    /// `install_users` の全員に付ける権利。
    right: UserRight,
}

impl DirAccess {
    /// SYSTEM と Administrators だけ（サービスだけが使う構成）。
    pub fn service_only() -> Self {
        Self {
            install_users: Vec::new(),
            right: UserRight::Modify,
        }
    }

    /// SYSTEM と Administrators に加えて、インストール利用者に「変更」（決定 4 -
    /// 昇格していないデスクトップの host が同じキーを読み書きするため）。
    pub fn with_install_user(sid: InstallUserSid) -> Self {
        Self {
            install_users: vec![sid],
            right: UserRight::Modify,
        }
    }

    /// SYSTEM と Administrators に加えて、`sids` の**それぞれに**「変更」（#626 PR-A）。
    /// 同じ SID は 1 つにまとめる（順は最初に現れた順）。空なら [`DirAccess::service_only`]、
    /// 1 人なら [`DirAccess::with_install_user`] と同じ値になる。
    pub fn with_install_users(sids: impl IntoIterator<Item = InstallUserSid>) -> Self {
        Self::with_right(sids, UserRight::Modify)
    }

    /// SYSTEM と Administrators に加えて、`sids` の**それぞれに**「読み取りと実行」
    /// （運用モード。#623 PR-1c、設計 §2.2・§2.8）。重複のまとめ方は
    /// [`DirAccess::with_install_users`] と同じ。空なら [`DirAccess::service_only`]。
    pub fn with_readers(sids: impl IntoIterator<Item = InstallUserSid>) -> Self {
        Self::with_right(sids, UserRight::Read)
    }

    fn with_right(sids: impl IntoIterator<Item = InstallUserSid>, right: UserRight) -> Self {
        let mut install_users: Vec<InstallUserSid> = Vec::new();
        for sid in sids {
            if !install_users.contains(&sid) {
                install_users.push(sid);
            }
        }
        let right = if install_users.is_empty() {
            UserRight::Modify
        } else {
            right
        };
        Self {
            install_users,
            right,
        }
    }

    /// 同じ利用者で権利だけを替えた値（設定モード ⇔ 運用モードの対。#623 PR-1c）。
    pub fn to_right(&self, right: UserRight) -> Self {
        Self::with_right(self.install_users.iter().cloned(), right)
    }

    /// 利用者に付ける権利（利用者が 0 人なら [`UserRight::Modify`]）。
    pub fn right(&self) -> UserRight {
        self.right
    }

    /// 利用者が**ちょうど 1 人**のときだけ、その SID（0 人・複数なら `None`。複数を
    /// 扱うときは [`DirAccess::install_users`]）。
    pub fn install_user(&self) -> Option<&InstallUserSid> {
        match self.install_users.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// 「変更」を付ける利用者の全員（重複なし・指定の順）。
    pub fn install_users(&self) -> &[InstallUserSid] {
        &self.install_users
    }

    /// 期待する ACE（SID・アクセス）。
    fn expected_aces(&self) -> Vec<(String, u32)> {
        let mut aces = vec![
            (SYSTEM_SID.to_owned(), FULL_ACCESS_MASK),
            (ADMINISTRATORS_SID.to_owned(), FULL_ACCESS_MASK),
        ];
        for user in &self.install_users {
            aces.push((user.0.clone(), self.right.mask()));
        }
        aces
    }

    /// 作るとき・直すときに使う SDDL（`D:P` = 保護された DACL）。
    pub fn sddl(&self) -> String {
        let mut sddl = format!("D:P(A;OICI;FA;;;{SYSTEM_SID})(A;OICI;FA;;;{ADMINISTRATORS_SID})");
        let mask = self.right.mask();
        for user in &self.install_users {
            sddl.push_str(&format!("(A;OICI;{mask:#x};;;{})", user.0));
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
    // 所有者として許すのは SYSTEM・Administrators と、**「変更」の利用者だけ**（#623 PR-1c、
    // 設計 §2.2）。所有者は DACL に関わらず `WRITE_DAC` を持つので、「読み取り」の利用者が
    // 所有者のままだと運用モードの DACL を本人が戻せる。
    let owner_allowed = observed.owner == SYSTEM_SID
        || observed.owner == ADMINISTRATORS_SID
        || (access.right == UserRight::Modify
            && access
                .install_users
                .iter()
                .any(|user| user.0 == observed.owner));
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
    if has_dot_segment(dir) {
        return Err(SafeRootError::DotSegmentInPath);
    }
    Ok(())
}

/// パスに `.`・`..`（と点だけの名前）の要素があるか（文字列の要素で見る。区切りは
/// Windows では `\` と `/`、それ以外は `/`）。[`reject_dot_segments`] と
/// [`guard::resolve_machine_root`] が使う。
pub(crate) fn has_dot_segment(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    let is_separator = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    text.split(is_separator)
        .any(|segment| !segment.is_empty() && segment.chars().all(|c| c == '.'))
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
/// 使う）。非 Windows は [`SafeRootError::UnsupportedPlatform`]。
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

/// TrustedInstaller（`NT SERVICE\TrustedInstaller`）。OS の部品の所有者で、[`exposure`] は
/// SYSTEM・Administrators と同じく信頼する（Administrators より強い主体なので、許しても
/// 境界は広がらない）。
pub const TRUSTED_INSTALLER_SID: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// 辿らずに開いたファイル・フォルダの、**開いたハンドルから**読んだ属性と記述子（#630。
/// [`inspect_file`]・`PinnedDir::security`・`PinnedFile::security`）。[`exposure`] の入力。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSecurity {
    /// フォルダか（`FILE_ATTRIBUTE_DIRECTORY`）。偽ならファイル。
    pub is_dir: bool,
    /// reparse point か（ジャンクション・シンボリックリンクなど、種類を問わない）。
    pub is_reparse_point: bool,
    /// ハードリンクの数（`nNumberOfLinks`）。ファイルで 2 以上なら、記述子を共有する別の
    /// 名前がある。
    pub links: u32,
    /// 所有者・保護の有無（`SE_DACL_PROTECTED`）・DACL（null DACL は `aces: None`）。ACE の
    /// 読み方は [`PinnedDir::read_dacl`] と同じ（条件付き・オブジェクトの ACE の SID も読み、
    /// 種類の分からない ACE・形の壊れた ACE は SID が空）。
    pub dacl: ObservedDacl,
}

/// [`exposure`] が「保護されていない」と判定した理由（1 条件 1 理由。#630）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Exposure {
    /// reparse point（ジャンクション・シンボリックリンクなど。辿った先は確かめていない）。
    ReparsePoint,
    /// ファイルで、リンク数が 2 以上（記述子を共有する別の名前が、保護されていない場所に
    /// ありうる）。中身はリンク数。
    HardLinked(u32),
    /// 所有者が SYSTEM・Administrators・TrustedInstaller 以外（所有者は DACL に関わらず
    /// ACL を書き換えられる）。中身は所有者の SID。
    ForeignOwner(String),
    /// null DACL（全員にフルアクセス）。
    NullDacl,
    /// SYSTEM・Administrators・TrustedInstaller 以外への**許可**の ACE（普通の許可のほか、
    /// 条件付き・オブジェクトの許可も。継承された ACE・`INHERIT_ONLY` の ACE も - フォルダ
    /// ではこれから作るファイルへ伝わるため、区別せず数える）。
    AllowsOther {
        ace_type: u8,
        flags: u8,
        mask: u32,
        sid: String,
    },
    /// 種類の分からない ACE（許可か拒否か分からないので、安全側に数える）。
    UnknownAce { ace_type: u8, flags: u8, mask: u32 },
    /// SID が読めない ACE（形が壊れている。拒否の種類でも、相手が分からないので数える）。
    UnreadableSid { ace_type: u8, flags: u8, mask: u32 },
}

/// 信頼する主体（[`exposure`] の所有者と許可の ACE の相手）。
fn is_trusted_sid(sid: &str) -> bool {
    sid == SYSTEM_SID || sid == ADMINISTRATORS_SID || sid == TRUSTED_INSTALLER_SID
}

/// 置き場（フォルダ）・ファイルが**保護されていない**理由を、すべて返す（純関数、#630。空なら
/// 保護されている）。sink の設定ファイル（Hub の API キー入り）のように、SYSTEM と
/// Administrators だけが読み書きできるべきものの確かめに使う。見る順（返す順）:
///
/// 1. reparse point → [`Exposure::ReparsePoint`]。
/// 2. ファイルでリンク数 2 以上 → [`Exposure::HardLinked`]。
/// 3. 所有者が信頼する主体（SYSTEM・Administrators・TrustedInstaller）以外 →
///    [`Exposure::ForeignOwner`]。
/// 4. null DACL → [`Exposure::NullDacl`]（ACE は見ない）。
/// 5. DACL の ACE を順に（1 つの ACE から理由は高々 1 つ）:
///    - 種類が分からない → [`Exposure::UnknownAce`]。
///    - SID が読めない → [`Exposure::UnreadableSid`]。
///    - 拒否（条件付き・オブジェクトを含む）→ 理由にしない（狭める方向なので）。
///    - 許可（普通・条件付き・オブジェクト）で、相手が信頼する主体以外 →
///      [`Exposure::AllowsOther`]。マスク・継承のフラグは問わない（実効権限は計算しない。
///      ACE の SID の許可リストで判定し、多めに断る側に倒す）。
///
/// DACL が保護されているか（`protected`）はそれ自体では理由にしない（継承した ACE も、相手が
/// 信頼する主体だけなら保護されている。相手が違えば上の 5 で数える）。
pub fn exposure(security: &FileSecurity) -> Vec<Exposure> {
    let mut found = Vec::new();
    if security.is_reparse_point {
        found.push(Exposure::ReparsePoint);
    }
    if !security.is_dir && security.links >= 2 {
        found.push(Exposure::HardLinked(security.links));
    }
    if !is_trusted_sid(&security.dacl.owner) {
        found.push(Exposure::ForeignOwner(security.dacl.owner.clone()));
    }
    let Some(aces) = &security.dacl.aces else {
        found.push(Exposure::NullDacl);
        return found;
    };
    for ace in aces {
        let (ace_type, flags, mask) = (ace.ace_type, ace.flags, ace.mask);
        if !guard::is_known_dacl_ace_type(ace_type) {
            found.push(Exposure::UnknownAce {
                ace_type,
                flags,
                mask,
            });
        } else if ace.sid.is_empty() {
            found.push(Exposure::UnreadableSid {
                ace_type,
                flags,
                mask,
            });
        } else if guard::DENY_ACE_TYPES.contains(&ace_type) || is_trusted_sid(&ace.sid) {
            // 拒否は狭める方向。信頼する主体への許可は境界の内側。
        } else {
            found.push(Exposure::AllowsOther {
                ace_type,
                flags,
                mask,
                sid: ace.sid.clone(),
            });
        }
    }
    found
}

/// パスを**辿らずに**開き（最後の要素がリンクでもそのものを開く）、開いたハンドルから属性・
/// リンク数・所有者・DACL を読む（#630。ファイルでもフォルダでもよい）。無ければ `Ok(None)`。
/// 中身は読まない（`READ_CONTROL | FILE_READ_ATTRIBUTES` だけを求め、共有はすべて許す）。
/// 途中の要素は辿るので、場所の確かめが要るときは `pin_dir` の本当のパスを使い、中身を読む
/// ときは `PinnedDir::open_child_file`（確かめたハンドルのまま読む）を使う。`.`・`..` を
/// 含むパスは開かずに断る。非 Windows は [`SafeRootError::UnsupportedPlatform`]。
pub fn inspect_file(path: &std::path::Path) -> Result<Option<FileSecurity>> {
    reject_dot_segments(path)?;
    imp::inspect_file(path)
}

/// [`PinnedDir::reset_acl`] が**触らずに飛ばした**中の子（#392 B3-3b の監査 Must 2）。
/// 書くと木の外へ影響するので、報告だけして書き換えない。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResetSkips {
    /// reparse point（ジャンクション・シンボリックリンクなど）。辿らず・触らない。
    pub reparse: Vec<std::path::PathBuf>,
    /// ハードリンク（ファイルでリンク数 2 以上）。記述子を共有するので触らない（#624）。
    pub hardlink: Vec<std::path::PathBuf>,
    /// 開けなかった子（利用者が Administrators を拒否した・所有権が無いなど）。特権を
    /// 有効にせず、触らず飛ばした（#392 B3-3b の監査 Should 1）。そのまま残る。
    pub denied: Vec<std::path::PathBuf>,
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
    /// ディレクトリの列挙の位置は**ファイルオブジェクト（このハンドル）に付く**ので、同じ
    /// ハンドルの列挙（[`PinnedDir::reset_acl`] の再帰の最上段・`ManagedDir::list`）を直列化
    /// する（#623 PR-1b のレビュー S-1）。重なると列挙位置が混ざり、`reset_acl` が残りの子を
    /// 書き直さずに `ERROR_NO_MORE_FILES` で「成功」を返してしまう。子の再帰は ID で開いた
    /// 新しいハンドルなので、取るのは最上段だけでよい。
    enum_lock: std::sync::Mutex<()>,
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

    /// 握っている生のハンドル（crate の中の、ハンドル基準の操作だけが使う。閉じない。
    /// #623 PR-1b の `ManagedDir`）。
    pub(crate) fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle.raw()
    }

    /// このハンドルの列挙の直列化（[`PinnedDir`] の `enum_lock`）。`enumerate_dir` をこの
    /// ハンドルで呼ぶ側は、呼ぶ間これを持つ。
    pub(crate) fn enum_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.enum_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 開いたままの**ハンドルから**、今の DACL と所有者を読む（`GetSecurityInfo`。パスを
    /// 使わないので、開いた後に途中の要素をすり替えられても別の場所を読まない。#626 PR-A:
    /// banto-hub の `grant-profile-acl` が、書き直す前に引き継ぐ利用者を決めるため -
    /// [`guard::plan_carry_over`]）。読むだけで何も変えない。
    pub fn read_dacl(&self) -> Result<ObservedDacl> {
        imp::read_dacl_of_handle(self.handle.raw())
    }

    /// 開いたままの**ハンドルで**、このフォルダと中の子へ、保護された DACL と所有者
    /// （`BUILTIN\Administrators`）を書く（#392 B3-3b の監査 Must 2・B5、2026-10-16 オーナー決定）。
    ///
    /// - 対象そのものは `SetKernelObjectSecurity`（**自動の継承の伝播をしない**）でハンドルに
    ///   書く。パスではなくハンドル（`OPEN_REPARSE_POINT` で開いた）に書くので、確かめてから
    ///   書くまでの間にその場で reparse point にされても、書き換わるのはリンクそのものだけ。
    /// - 継承は**自前で**中の子へ伝える（`SetNamedSecurityInfoW`・`SetSecurityInfo` の自動の
    ///   伝播は、advapi がユーザーモードで木を歩いて**ハードリンクの子では木の外の名前にも
    ///   効く**ため使わない。子のジャンクションの先には届かないと測定、ディレクトリの
    ///   シンボリックリンクは未測 - モジュール doc）。子は `OPEN_REPARSE_POINT` で辿らずに開き、reparse point は
    ///   飛ばし（[`ResetSkips::reparse`]）、ハードリンク（リンク数 2 以上のファイル）も
    ///   飛ばす（[`ResetSkips::hardlink`]。記述子を共有するので、書くと木の外の名前の
    ///   アクセスを変える。#624）。飛ばした子は返り値で報告する。
    ///
    /// 所有者を移すのは、所有者は DACL に関わらず ACL を変えられるため（インストール利用者が
    /// 所有者のままだと書き直した DACL を本人が広げられる。インストール利用者の「変更」は残る）。
    /// 昇格した管理者のプロセスから呼ぶ（昇格していなければ `ERROR_INVALID_OWNER` で失敗）。
    /// **開いた時点で reparse point・ディレクトリでないものには何もせずに断る**（呼ぶ側の
    /// 確かめに加えた多重の防御）。今の所有者を受け入れるかの確かめは呼ぶ側（製品の判断）。
    /// `recurse` が偽なら、このフォルダそのものだけ書いて中へは下りない（監査 Nit 3:
    /// すでにルートの再帰で書いた `secrets`・`hub-keys` を、その層でもう一度書き直さない）。
    pub fn reset_acl(&self, access: &DirAccess, recurse: Recurse) -> Result<ResetSkips> {
        if self.info.is_reparse_point {
            return Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint));
        }
        if !self.info.is_dir {
            return Err(SafeRootError::AclMismatch(AclMismatch::NotADirectory));
        }
        let mut skips = ResetSkips::default();
        // 再帰の最上段はこのハンドルを列挙する。`ManagedDir::list` と重ならないようにする。
        let _serial = self.enum_guard();
        imp::set_security_tree(
            self.handle.raw(),
            &self.final_path,
            access,
            matches!(recurse, Recurse::Tree),
            &mut skips,
        )?;
        Ok(skips)
    }
}

/// モードの ACL（#623 PR-1c、[`crate::mode`] のモジュール doc）。どれも握ったハンドルを木の
/// ルートとして、パスを使わずに歩く。
#[cfg(windows)]
impl PinnedDir {
    /// 木のルートとして扱えるか（reparse point・フォルダでない物には何もしない）。
    fn as_tree_root(&self) -> Result<()> {
        if self.info.is_reparse_point {
            return Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint));
        }
        if !self.info.is_dir {
            return Err(SafeRootError::AclMismatch(AclMismatch::NotADirectory));
        }
        Ok(())
    }

    /// ルート自身が EFS で暗号化されていれば、その問題（1 周目・2 周目の入口）。
    fn root_encrypted(&self) -> Result<Option<crate::mode::TreeProblem>> {
        let info = imp::info_of(self.handle.raw())?;
        Ok(info.encrypted.then(|| crate::mode::TreeProblem::Encrypted {
            path: self.final_path.clone(),
        }))
    }

    /// **1 周目**（読むだけ。設計 §2.3）: 木の全体を辿らずに確かめ、締められない物
    /// （[`crate::mode::TreeProblem`]）が 1 つでもあれば**何も変えずに**
    /// [`crate::mode::ScanRefusal::Problems`] で断る（見つけた物を全部並べる）。子は 2 周目と
    /// 同じ権利で開く（`FILE_READ_ATTRIBUTES` だけだと親の `FILE_LIST_DIRECTORY` から暗黙に
    /// 開けてしまい、2 周目で開けない子を見落とす）。`stop` は各フォルダの境で見て、
    /// [`crate::mode::StopSignal::None`] 以外なら何も変えずに
    /// [`crate::mode::ScanRefusal::Stopped`]。`progress` は物 1 つごと。
    pub fn scan_tree(
        &self,
        stop: &dyn Fn() -> crate::mode::StopSignal,
        progress: &mut dyn FnMut(&crate::mode::Progress),
    ) -> std::result::Result<crate::mode::ScanReport, crate::mode::ScanRefusal> {
        use crate::mode::{Progress, ScanRefusal, ScanReport};
        self.as_tree_root().map_err(ScanRefusal::Io)?;
        let mut recorded = Vec::new();
        if let Some(problem) = self.root_encrypted().map_err(ScanRefusal::Io)? {
            recorded.push(problem);
        }
        let _serial = self.enum_guard();
        let mut walk = imp::Walk {
            within: &self.final_path,
            on_skip: imp::OnSkip::Record,
            order: imp::Order::RootFirst,
            write: None,
            strict: true,
            place_markers: false,
            stop,
            progress,
            counts: Progress {
                examined: 1,
                written: 0,
            },
            recorded,
        };
        match imp::walk_dir(&mut walk, self.handle.raw(), &self.final_path, 0) {
            Ok(()) => {}
            Err(imp::WalkStop::Stopped(signal)) => return Err(ScanRefusal::Stopped(signal)),
            Err(imp::WalkStop::Io(error)) => return Err(ScanRefusal::Io(error)),
            Err(imp::WalkStop::Problem(problem)) => {
                return Err(ScanRefusal::Problems(vec![problem]))
            }
        }
        if !walk.recorded.is_empty() {
            return Err(ScanRefusal::Problems(walk.recorded));
        }
        Ok(ScanReport {
            examined: walk.counts.examined,
        })
    }

    /// **2 周目**（締める。設計 §2.4）: 運用モードの形（`mode.operation()`: 利用者に
    /// 「読み取りと実行」、所有者 Administrators、保護された DACL）を、**上から順に、締めてから
    /// 中を列挙して**木の全体へ書く。締められない物に会ったら**その場で止めて**
    /// [`crate::mode::CloseAbort`]（`written` までは締めてある。呼ぶ側は `relax_tree` で緩め直す。
    /// PRESHUTDOWN なら緩めない）。印のファイルには印の記述子を書き直す。ID で開いた物は本当の
    /// パスが木の中であることを確かめてから書く。`stop` は各フォルダの境で見る（ルートに書く
    /// 前が最初）。成功したら進み具合（確かめた数・書いた数）を返す。
    ///
    /// 所有者を Administrators に移すので、LocalSystem・昇格した管理者から呼ぶ（非昇格は
    /// `ERROR_INVALID_OWNER`(1307) で、ルートにも書かずに止まる）。
    pub fn close_tree(
        &self,
        mode: &crate::mode::ModeAccess,
        stop: &dyn Fn() -> crate::mode::StopSignal,
        progress: &mut dyn FnMut(&crate::mode::Progress),
    ) -> std::result::Result<crate::mode::Progress, crate::mode::CloseAbort> {
        use crate::mode::{AbortReason, CloseAbort, Progress};
        let io = |error| CloseAbort {
            written: 0,
            reason: AbortReason::Io(error),
        };
        self.as_tree_root().map_err(io)?;
        let operation = mode.operation();
        imp::ensure_user_account(operation).map_err(io)?;
        if let Some(problem) = self.root_encrypted().map_err(io)? {
            return Err(CloseAbort {
                written: 0,
                reason: AbortReason::Problem(problem),
            });
        }
        let dir_sd = imp::owned_protected_sd(operation, true).map_err(io)?;
        let file_sd = imp::owned_protected_sd(operation, false).map_err(io)?;
        let marker_sd = imp::marker_descriptor().map_err(io)?;
        let _serial = self.enum_guard();
        let mut walk = imp::Walk {
            within: &self.final_path,
            on_skip: imp::OnSkip::Abort,
            order: imp::Order::RootFirst,
            write: Some(imp::WriteSet {
                dir_sd: &dir_sd,
                file_sd: &file_sd,
                marker_sd: &marker_sd,
            }),
            strict: true,
            place_markers: true,
            stop,
            progress,
            counts: Progress {
                examined: 1,
                written: 0,
            },
            recorded: Vec::new(),
        };
        match imp::walk_dir(&mut walk, self.handle.raw(), &self.final_path, 0) {
            Ok(()) => Ok(walk.counts),
            Err(stop) => Err(CloseAbort {
                written: walk.counts.written,
                reason: match stop {
                    imp::WalkStop::Problem(problem) => AbortReason::Problem(problem),
                    imp::WalkStop::Stopped(signal) => AbortReason::Stopped(signal),
                    imp::WalkStop::Io(error) => AbortReason::Io(error),
                },
            }),
        }
    }

    /// **緩める**（設計 §2.5）: 設定モードの形（`mode.setting()`: 利用者に「変更」、所有者
    /// Administrators、保護された DACL）を、**下から順に、ルートを最後に**木の全体へ書く。
    /// 会った reparse point・ハードリンク・開けない子・使用中の子・深すぎる木は**飛ばして記録し、
    /// 続ける**（[`crate::mode::RelaxReport::skipped`]。締まったまま残る = 安全側）。印のファイル
    /// には印の記述子のまま（利用者の ACE を付けない）。止める合図は見ない（途中で止めると
    /// 「ルートは締まっている・下の一部は緩い」が残るだけで得が無い）。緩い木に対しても冪等。
    ///
    /// 昇格ヘルパーの `install`・修復もこれを使う（木を書くすべての書き手が同じ順序を守る
    /// ことで「ルートが緩んでいる ⇔ 全体が緩み終わった」が成り立つ。設計 §3.5）。
    pub fn relax_tree(
        &self,
        mode: &crate::mode::ModeAccess,
        progress: &mut dyn FnMut(&crate::mode::Progress),
    ) -> Result<crate::mode::RelaxReport> {
        use crate::mode::{Progress, RelaxReport, StopSignal};
        self.as_tree_root()?;
        let setting = mode.setting();
        imp::ensure_user_account(setting)?;
        let dir_sd = imp::owned_protected_sd(setting, true)?;
        let file_sd = imp::owned_protected_sd(setting, false)?;
        let marker_sd = imp::marker_descriptor()?;
        let _serial = self.enum_guard();
        let mut walk = imp::Walk {
            within: &self.final_path,
            on_skip: imp::OnSkip::Record,
            order: imp::Order::RootLast,
            write: Some(imp::WriteSet {
                dir_sd: &dir_sd,
                file_sd: &file_sd,
                marker_sd: &marker_sd,
            }),
            strict: false,
            place_markers: false,
            stop: &|| StopSignal::None,
            progress,
            counts: Progress {
                examined: 1,
                written: 0,
            },
            recorded: Vec::new(),
        };
        match imp::walk_dir(&mut walk, self.handle.raw(), &self.final_path, 0) {
            Ok(()) => Ok(RelaxReport {
                written: walk.counts.written,
                skipped: walk.recorded,
            }),
            Err(imp::WalkStop::Io(error)) => Err(error),
            // `Record` は止めず、`stop` は常に続けるので来ない。
            Err(other) => Err(SafeRootError::io(
                crate::error::IoOp::InspectDir,
                &std::io::Error::other(format!("緩める途中で止まりました（{other:?}）")),
            )),
        }
    }

    /// 印のファイル（[`crate::mode::MARKER_NAME`]）を、このフォルダに**無ければ置く**（設計
    /// §2.9 (a)）: 空のファイルを、SYSTEM・Administrators だけ・所有者 Administrators・利用者の
    /// ACE なしの記述子を**作るのと同時に**付けて、握ったハンドル基準で作る。既にあれば、
    /// 共有 READ だけ・`DELETE` を求めて開き（利用者が書き込み・削除で握っていれば
    /// [`crate::mode::MarkerError::Busy`]）、ファイル・reparse point でない・リンク数 1 を確かめて
    /// 記述子を印の形に書き直す（設定モードで管理者・利用者が触った場合に揃える）。合わなければ
    /// [`crate::mode::MarkerError::Occupied`]。2 周目（`close_tree`）は木の**すべての**フォルダに
    /// 同じ印を置くので、締めた直後のフォルダでは `Existed` になる。
    ///
    /// 置いた後（既にあった後）に、**握ったこのハンドルで自分の reparse 属性を読み直し**、
    /// reparse point なら [`crate::mode::MarkerError::DirIsReparse`]（印を置く前に、締める前から
    /// 開いていたハンドルで `FSCTL_SET_REPARSE_POINT` されていた。改ざんとみなして呼ぶ側は
    /// エンジンを動かさない）。作れなかったときも同じ確かめをして、reparse ならそれを理由にする。
    /// 空でないフォルダは reparse point にできないので、置いた後は reparse 化されない。
    pub fn ensure_marker(
        &self,
    ) -> std::result::Result<crate::mode::MarkerOutcome, crate::mode::MarkerError> {
        use crate::mode::MarkerError;
        let own_reparse = || -> Option<MarkerError> {
            match crate::handle_io::facts(self.handle.raw()) {
                Ok(facts) if facts.is_reparse => Some(MarkerError::DirIsReparse {
                    tag: facts.reparse_tag,
                }),
                _ => None,
            }
        };
        let outcome = match imp::place_marker(self.handle.raw()) {
            Ok(outcome) => outcome,
            Err(failure) => {
                if let Some(error) = own_reparse() {
                    return Err(error);
                }
                return Err(match failure {
                    imp::MarkerFailure::Occupied(refusal) => MarkerError::Occupied(refusal),
                    imp::MarkerFailure::Busy(os_error) => MarkerError::Busy { os_error },
                    imp::MarkerFailure::Io(error) => MarkerError::Io(error),
                });
            }
        };
        let facts = crate::handle_io::facts(self.handle.raw()).map_err(MarkerError::Io)?;
        if facts.is_reparse {
            return Err(MarkerError::DirIsReparse {
                tag: facts.reparse_tag,
            });
        }
        Ok(outcome)
    }

    /// 確かめ直し（設計 §2.9 (b)(d)。パスで渡す直前）: 握ったこのハンドルで自分の属性を読み直し
    /// （reparse point なら [`crate::mode::RecheckFailure::ReparsePoint`]）、記述子を読み直して
    /// `expected`（運用モードの形）に [`check_dacl`] で通るか確かめる。何も変えない。
    pub fn recheck(
        &self,
        expected: &DirAccess,
    ) -> std::result::Result<(), crate::mode::RecheckFailure> {
        use crate::mode::RecheckFailure;
        let facts = crate::handle_io::facts(self.handle.raw()).map_err(RecheckFailure::Io)?;
        if facts.is_reparse {
            return Err(RecheckFailure::ReparsePoint {
                tag: facts.reparse_tag,
            });
        }
        if !facts.is_dir {
            return Err(RecheckFailure::NotADirectory);
        }
        let dacl = imp::read_dacl_of_handle(self.handle.raw()).map_err(RecheckFailure::Io)?;
        check_dacl(&dacl, expected).map_err(RecheckFailure::Acl)
    }
}

#[cfg(windows)]
impl PinnedDir {
    /// このフォルダ（確かめ済みの開いたハンドル）を基準に、名前 1 要素の子のフォルダを
    /// **相対で**作り、作ったハンドルをそのまま辿らずに開いたもの（[`PinnedDir`]）として
    /// 返す（#626 PR-B の再レビュー 1）。既にあれば `Ok(None)`（作らない）。
    ///
    /// `NtCreateFile` の `ObjectAttributes.RootDirectory` にこのハンドル、名前は `name`
    /// 1 要素（[`guard::is_plain_child_name`] が断るもの - 区切り・`:`・`.`/`..`・末尾の `.` と
    /// 空白・`* ? < > | "`・予約デバイス名 - は断る）、`FILE_CREATE | FILE_DIRECTORY_FILE |
    /// FILE_OPEN_REPARSE_POINT` で作る。**パスを解決し直さない**ので、このフォルダの
    /// 祖先をジャンクションにすり替えられても、作るのはこのフォルダの中だけ。
    /// `access` が `Some` なら作るのと同時にその保護された DACL を付け（作ってから付ける
    /// までの間に広い権限で見える時間を作らない）、`None` なら親から継承する。
    /// 返すハンドルは [`pin_dir`] と同じ権利（`WRITE_DAC | WRITE_OWNER` を含む）・共有
    /// （削除なし）で開いている。
    pub fn create_child_dir(
        &self,
        name: &str,
        access: Option<&DirAccess>,
    ) -> Result<Option<PinnedDir>> {
        imp::create_child_dir(self, name, access)
    }

    /// このフォルダ（確かめ済みの開いたハンドル）を基準に、名前 1 要素の子を**相対で辿らずに**
    /// 開く（#641 のオーナーレビュー Nit 5）。無ければ `Ok(None)`。
    ///
    /// [`PinnedDir::create_child_dir`] と同じ作り（`NtCreateFile` の
    /// `ObjectAttributes.RootDirectory` にこのハンドル、`FILE_OPEN | FILE_OPEN_REPARSE_POINT`、
    /// 名前の確かめも同じ）で、**パスを解決し直さない**。作ろうとしたら既にあった
    /// （`create_child_dir` が `Ok(None)`）子を、パスで開き直す代わりに使う（パスで開き直すと、
    /// その間に祖先をすり替えられたとき別の場所を開く）。子がジャンクション・ファイルでも
    /// 辿らずにそのものを開くので、呼ぶ側が [`PinnedDir::info`] で断る（[`pin_dir`] と同じ）。
    pub fn open_child_dir(&self, name: &str) -> Result<Option<PinnedDir>> {
        imp::open_child_dir(self, name)
    }

    /// 開いたままの**ハンドルから**、このフォルダの属性・リンク数・所有者・DACL を読む
    /// （#630。[`exposure`] で置き場が保護されているかを確かめるため）。読むだけ。
    pub fn security(&self) -> Result<FileSecurity> {
        imp::security_of_handle(self.handle.raw())
    }

    /// このフォルダ（確かめ済みの開いたハンドル）を基準に、名前 1 要素の子を**相対で辿らずに**
    /// 開く（#630。中身を読むため）。無ければ `Ok(None)`。
    ///
    /// [`PinnedDir::open_child_dir`] と同じ作り（`NtCreateFile` の `RootDirectory` にこの
    /// ハンドル、`FILE_OPEN | FILE_OPEN_REPARSE_POINT`、名前の確かめも
    /// [`guard::is_plain_child_name`]）で、**パスを解決し直さない**。子がフォルダ・リンクでも
    /// 辿らずにそのものを開くので、呼ぶ側が [`PinnedFile::security`] と [`exposure`] で確かめる
    /// （[`PinnedFile::read_to_end`] はファイルでないもの・reparse point を読まずに断る）。
    /// 求めるのは読み取り（`READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_DATA`）だけで、
    /// 共有は**読み取りだけ**（`FILE_SHARE_READ`）: 持っている間は、ほかの主体が消す・名前を
    /// 変える（すり替える）ことも、**中身を書き換える**ために書き込みで開くこともできない。
    /// 既に書き込み（または削除）で開いているハンドルがあれば、開くこと自体が共有違反で失敗する
    /// （ACL を締める前から書き込みで開いていた主体が、確かめた後に書き換えるのを防ぐ。#648）。
    /// バックアップの意図（`FILE_OPEN_FOR_BACKUP_INTENT`）は付けず、DACL だけを門にする。
    /// 開けない（アクセス拒否・共有違反など）は [`SafeRootError::Io`]（`IoOp::OpenFile`、
    /// `kind` に `PermissionDenied` など、`os_error` に `ERROR_SHARING_VIOLATION`(32) など）。
    pub fn open_child_file(&self, name: &str) -> Result<Option<PinnedFile>> {
        imp::open_child_file(self, name)
    }

    /// このフォルダ（確かめ済みの開いたハンドル）を基準に、名前 1 要素の**新しい**ファイルを
    /// 相対で作り、`contents` を書いてディスクへ書き出す（#630。PR-2 の `prepare-sink-config`
    /// が `.example` を置くため）。既にあれば `Ok(None)`（何も書かない・上書きしない）。
    ///
    /// - **作るのと同時に**、保護された DACL（SYSTEM・Administrators にフル、継承のフラグ
    ///   なし）と所有者 `BUILTIN\Administrators` を付ける（`NtCreateFile` に記述子を渡す。
    ///   作ってから付けるまでの間に、親から継承した権限で見える時間を作らない。所有者を
    ///   明示するのは、「作成者を所有者に」の PC で昇格した利用者が所有者にならないため）。
    ///   所有者を Administrators にできるのは昇格した管理者・SYSTEM だけ（それ以外は
    ///   `ERROR_INVALID_OWNER` で何も作らずに失敗する）。
    /// - 名前の確かめ・相対で作る作りは [`PinnedDir::create_child_dir`] と同じ
    ///   （`FILE_CREATE | FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT`）。
    /// - 書いた後、作ったハンドルから記述子を読み戻して [`exposure`] が空であることを
    ///   確かめる。書き込み・書き出し・読み戻しのどれかが失敗した、または保護されていなかった
    ///   （[`SafeRootError::Exposed`]）ときは、**作ったハンドルで**消してから（
    ///   `FileDispositionInfo`。名前で消さないので、別の物を消さない）失敗を返す。
    pub fn create_child_file(&self, name: &str, contents: &[u8]) -> Result<Option<PinnedFile>> {
        imp::create_child_file(self, name, contents)
    }

    /// このフォルダ（確かめ済みの開いたハンドル）を基準に、名前 1 要素の子を**相対で辿らずに**
    /// 開く（#630 PR-2。`banto-hub-elev prepare-sink-config` が、置き場の中の設定ファイルの
    /// 所有者・DACL を整え直し（[`PinnedFile::reset_acl`]）、`.example` を消して作り直す
    /// （[`PinnedFile::delete`]）ため）。無ければ `Ok(None)`。
    ///
    /// 作り・名前の確かめは [`PinnedDir::open_child_file`] と同じ（`FILE_OPEN |
    /// FILE_OPEN_REPARSE_POINT`、パスを解決し直さない）。違うのは求める権利で、**中身は求めず**
    /// （`FILE_READ_DATA` を含めない）、`READ_CONTROL | FILE_READ_ATTRIBUTES | WRITE_DAC |
    /// WRITE_OWNER | DELETE` を求める。`DELETE` を求めるので共有の確かめに加わり、共有は
    /// **読み取りだけ**なので、持っている間はほかの誰も書き込みに開く・消す・名前を変える・
    /// すり替えることができず、既に書き込みに開いているプロセスがあれば開くこと自体が共有違反
    /// （`SafeRootError::Io` の `os_error` が 32）で失敗する。
    /// 子がフォルダ・リンクでも辿らずにそのものを開くので、呼ぶ側が [`PinnedFile::security`]
    /// で確かめる（[`PinnedFile::reset_acl`]・[`PinnedFile::delete`] も自分で断る）。
    /// [`PinnedFile::read_to_end`] は使えない（中身の権利を持たない）。
    pub fn open_child_file_for_update(&self, name: &str) -> Result<Option<PinnedFile>> {
        imp::open_child_file_for_update(self, name)
    }
}

/// [`PinnedDir::open_child_file`]・[`PinnedDir::create_child_file`] で、確かめ済みのフォルダの
/// ハンドルを基準に辿らずに開いたファイル（#630）。持っている間は消す・名前を変える・中身を
/// 書き換える（書き込みで開く）ことを誰にもさせない（共有は読み取りだけ。#648）。drop で閉じる。
#[cfg(windows)]
pub struct PinnedFile {
    handle: imp::OwnedHandle,
    path: std::path::PathBuf,
    /// `GetFinalPathNameByHandleW` で解決した本当のパス（[`PinnedDir::final_path`] と同じ形）。
    final_path: std::path::PathBuf,
    /// 開いた時点でハンドルから読んだ属性と記述子。
    security: FileSecurity,
}

#[cfg(windows)]
impl std::fmt::Debug for PinnedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedFile")
            .field("path", &self.path)
            .field("final_path", &self.final_path)
            .field("security", &self.security)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
impl PinnedFile {
    /// 親の [`PinnedDir::path`] + 名前（表示用。開き直すのには使わない）。
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 開いたハンドルの本当のパス（`\\?\` を外した DOS 形式）。
    pub fn final_path(&self) -> &std::path::Path {
        &self.final_path
    }

    /// 開いた時点でハンドルから読んだ属性・リンク数・所有者・DACL（[`exposure`] の入力）。
    pub fn security(&self) -> &FileSecurity {
        &self.security
    }

    /// 開いたままの**ハンドルから**、先頭から全部を読む（#630。確かめたハンドルのまま読むので、
    /// 確かめてから読むまでの間に別の物へすり替えられない）。`max_len` バイトを超えるときは
    /// 読まずに断る（`io::ErrorKind::FileTooLarge`）。フォルダは `io::ErrorKind::IsADirectory`、
    /// reparse point は [`AclMismatch::ReparsePoint`] で、読まずに断る。
    pub fn read_to_end(&mut self, max_len: usize) -> Result<Vec<u8>> {
        if self.security.is_reparse_point {
            return Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint));
        }
        if self.security.is_dir {
            return Err(SafeRootError::io(
                crate::error::IoOp::ReadFile,
                &std::io::Error::from(std::io::ErrorKind::IsADirectory),
            ));
        }
        imp::read_handle_to_end(self.handle.raw(), max_len)
    }

    /// 開いたままの**ハンドルで**、このファイルに保護された DACL（SYSTEM・Administrators に
    /// フル、継承のフラグなし）と所有者 `BUILTIN\Administrators` を書き、読み戻した記述子を
    /// 返す（#630 PR-2。[`PinnedDir::open_child_file_for_update`] で開いたもの）。中身は
    /// 読まない・変えない。
    ///
    /// - **書く直前にハンドルから属性を読み直し**、reparse point（[`AclMismatch::ReparsePoint`]）・
    ///   フォルダ（`io::ErrorKind::IsADirectory`）・ハードリンク（リンク数 2 以上。記述子を
    ///   共有するので、書くと別の名前のアクセスも変わる - #624。[`SafeRootError::Exposed`] に
    ///   [`Exposure::HardLinked`]）には何も書かずに断る。
    /// - 書くのは `SetKernelObjectSecurity`（自動の継承の伝播をしない。[`PinnedDir::reset_acl`]
    ///   と同じ）。所有者を Administrators にできるのは昇格した管理者・SYSTEM だけ（それ以外は
    ///   `ERROR_INVALID_OWNER`(1307) で何も変えずに失敗する）。
    /// - 書いた後に読み戻して [`exposure`] が空でなければ [`SafeRootError::Exposed`]。
    pub fn reset_acl(&self) -> Result<FileSecurity> {
        imp::reset_file_acl(self.handle.raw())
    }

    /// 開いたままの**ハンドルで**このファイルを消す（#630 PR-2。`FileDispositionInfo` で消す印を
    /// 付け、このハンドルを閉じたときに消える。名前で消さないので、確かめた物と別の物を
    /// 消さない）。[`PinnedDir::open_child_file_for_update`] で開いたもの（`DELETE` を持つ）。
    /// フォルダは消さずに断る（`io::ErrorKind::IsADirectory`）。リンク（reparse point）は、
    /// リンクそのものを消す（`OPEN_REPARSE_POINT` で開いているので、先は消さない）。
    pub fn delete(self) -> Result<()> {
        if self.security.is_dir {
            return Err(SafeRootError::io(
                crate::error::IoOp::RemoveFile,
                &std::io::Error::from(std::io::ErrorKind::IsADirectory),
            ));
        }
        imp::delete_file_by_handle(self.handle.raw())
        // ここで `self` を落とし、ハンドルを閉じる（閉じたときに消える）。
    }
}

/// [`PinnedDir::reset_acl`] が、中の子まで書き直すか（監査 Nit 3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recurse {
    /// このフォルダと、中の普通の子（孫まで）。
    Tree,
    /// このフォルダそのものだけ。
    ObjectOnly,
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
    use crate::error::{Result, SafeRootError};

    pub(super) fn ensure_user_account(_access: &DirAccess) -> Result<()> {
        Err(SafeRootError::UnsupportedPlatform)
    }

    pub(super) fn ensure_dir(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SafeRootError::UnsupportedPlatform)
    }

    pub(super) fn verify_dir(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SafeRootError::UnsupportedPlatform)
    }

    pub(super) fn reset_dir_acl(_dir: &Path, _access: &DirAccess) -> Result<()> {
        Err(SafeRootError::UnsupportedPlatform)
    }

    pub(super) fn create_dir_if_absent(_dir: &Path, _access: &DirAccess) -> Result<bool> {
        Err(SafeRootError::UnsupportedPlatform)
    }

    pub(super) fn read_dacl(_dir: &Path) -> Result<ObservedDacl> {
        Err(SafeRootError::UnsupportedPlatform)
    }

    pub(super) fn inspect_file(_path: &Path) -> Result<Option<super::FileSecurity>> {
        Err(SafeRootError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
pub(crate) mod imp {
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

    use super::{check_dacl, DirAccess, ObservedAce, ObservedDacl};
    use crate::error::{AclMismatch, IoOp, Result, SafeRootError};
    use crate::mode::{Progress, StopSignal, TreeProblem};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_str(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(crate) fn last_error() -> u32 {
        // SAFETY: 直前の Win32 呼び出しの失敗の理由を読むだけ。
        unsafe { GetLastError() }
    }

    pub(crate) fn api(call: &'static str, os_error: u32) -> SafeRootError {
        SafeRootError::AclApi { call, os_error }
    }

    /// `LocalAlloc` で確保された領域を drop で `LocalFree` する。
    pub(crate) struct LocalBox(*mut core::ffi::c_void);

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
        // 複数の利用者（#626 PR-A）は全員を確かめる。1 人でも通らなければ何もしない。
        for user in access.install_users() {
            ensure_one_user_account(user)?;
        }
        Ok(())
    }

    fn ensure_one_user_account(user: &super::InstallUserSid) -> Result<()> {
        let text = wide_str(user.as_str());
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `text` は NUL 終端。成功すれば `sid` は LocalAlloc の領域。
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == 0 {
            return Err(SafeRootError::InvalidUserSid("SID の形式ではありません"));
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
                return Err(SafeRootError::InvalidUserSid(
                    "この PC にこの SID のアカウントがありません",
                ));
            }
            return Err(api("LookupAccountSidW", os_error));
        }
        if kind != SidTypeUser {
            return Err(SafeRootError::InvalidUserSid(
                "利用者のアカウントではありません（グループなど）",
            ));
        }
        Ok(())
    }

    fn inspect(dir: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint));
                }
                if !meta.is_dir() {
                    return Err(SafeRootError::AclMismatch(AclMismatch::NotADirectory));
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SafeRootError::io(IoOp::InspectDir, &e)),
        }
    }

    pub(super) fn ensure_dir(dir: &Path, access: &DirAccess) -> Result<()> {
        // SID の検証は呼び出し側（公開の `ensure_dir`・保存の層の `FileSecretStore::open`）が
        // 作る・確かめるの分岐より前に済ませている。
        if inspect(dir)? {
            return verify_dir(dir, access);
        }
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| SafeRootError::io(IoOp::CreateDir, &e))?;
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
                return Err(SafeRootError::io(
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
            Ok(_) => return Err(SafeRootError::AclMismatch(AclMismatch::NotADirectory)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SafeRootError::io(IoOp::InspectDir, &e)),
        }
        // 作る経路でだけ SID を確かめる（モジュール doc「無いときだけ作る」）。
        ensure_user_account(access)?;
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| SafeRootError::io(IoOp::CreateDir, &e))?;
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
            return Err(SafeRootError::io(
                IoOp::CreateDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        Ok(true)
    }

    pub(super) fn verify_dir(dir: &Path, access: &DirAccess) -> Result<()> {
        if !inspect(dir)? {
            return Err(SafeRootError::io(
                IoOp::InspectDir,
                &std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        }
        let observed = read_dacl(dir)?;
        check_dacl(&observed, access).map_err(SafeRootError::AclMismatch)
    }

    /// 期待の保護された DACL を**パスで**書き直す（所有者は変えない）。昇格していない
    /// 呼び出し元（テストの片付け・`paths` のデスクトップ経路）も使う。`pin_dir` を
    /// 持たない分、確かめてから書き直すまでの間にその場でリンクにされる余地があるので、
    /// 昇格ヘルパーの書き直しは [`super::PinnedDir::reset_acl`]（ハンドルで書く）を使う。
    pub(super) fn reset_dir_acl(dir: &Path, access: &DirAccess) -> Result<()> {
        if !inspect(dir)? {
            return Err(SafeRootError::io(
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
    /// 自己相対の記述子を SDDL から作る。`inheritable` が真なら ACE に `OICI`（フォルダ用）、
    /// 偽なら継承のフラグ無し（ファイル用。監査 Nit 4: ファイルに `(OI)(CI)` が付かない）。
    /// どちらも `O:BA`（所有者 Administrators）と `D:P`（保護された DACL）。
    pub(crate) fn owned_protected_sd(access: &DirAccess, inheritable: bool) -> Result<LocalBox> {
        // `access.sddl()` は `D:P(A;OICI;FA;;;SY)...`。ファイルは `OICI` を外す（ACE の
        // フラグ欄だけが OICI なので、文字列の `OICI` を取り除けば継承のフラグ無しになる）。
        let dacl = access.sddl();
        let dacl = if inheritable {
            dacl
        } else {
            dacl.replace("OICI", "")
        };
        let sddl = wide_str(&format!("O:BA{dacl}"));
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

    /// 辿らずに開いたハンドルの情報（[`open_child_by_id`]）。
    pub(crate) struct Opened {
        pub(crate) handle: OwnedHandle,
        pub(crate) is_dir: bool,
        pub(crate) is_reparse: bool,
        /// reparse の tag（reparse point でなければ 0）。
        pub(crate) reparse_tag: u32,
        /// `FILE_ATTRIBUTE_ENCRYPTED`（EFS）。
        pub(crate) encrypted: bool,
        pub(crate) links: u32,
    }

    // 木を歩く部品（[`walk_dir`]）が子へ書いた直後に呼ぶフック（テスト専用。子をその場で
    // reparse point にすり替える反証 (d)、まだ締めていない孫にジャンクションを植える反証、
    // 緩めるときにルートがまだ締まっていることの観測に使う）。本番では `None`。
    #[cfg(test)]
    pub(crate) type AfterChildHook = Box<dyn Fn(HANDLE)>;
    #[cfg(test)]
    thread_local! {
        pub(crate) static AFTER_CHILD_WRITTEN: std::cell::RefCell<Option<AfterChildHook>> =
            const { std::cell::RefCell::new(None) };
    }

    /// 子を開くときの、DACL と所有者を書ける権利（`pin_dir` と同じ）。`FILE_LIST_DIRECTORY`
    /// はフォルダの列挙に要る（ファイルでは読み取りになるだけで害はない）。1 周目（読むだけ）も
    /// 同じ権利で開く: `FILE_READ_ATTRIBUTES` だけだと親の `FILE_LIST_DIRECTORY` から暗黙に
    /// 許されて開けてしまい、2 周目で開けない子を見落とすため（設計 §2.3）。
    fn child_access() -> u32 {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
        };
        READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | WRITE_DAC | WRITE_OWNER
    }

    /// ハンドルから読んだ属性（[`info_of`]）。
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct ObjectInfo {
        pub(crate) is_dir: bool,
        pub(crate) is_reparse: bool,
        /// reparse の tag（reparse point でなければ 0。読めなければ 0 のまま）。
        pub(crate) reparse_tag: u32,
        /// `FILE_ATTRIBUTE_ENCRYPTED`。
        pub(crate) encrypted: bool,
        pub(crate) links: u32,
    }

    /// `handle` が指すオブジェクトの属性（ディレクトリか・reparse point か・tag・EFS・リンク数）を
    /// ハンドルから読む。
    pub(crate) fn info_of(handle: HANDLE) -> Result<ObjectInfo> {
        use windows_sys::Win32::Storage::FileSystem::{
            FileAttributeTagInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
        };
        // SAFETY: 全フィールドが整数の構造体。`handle` は有効。
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            return Err(api("GetFileInformationByHandle", last_error()));
        }
        let is_reparse = info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        let mut reparse_tag = 0;
        if is_reparse {
            let mut tag = FILE_ATTRIBUTE_TAG_INFO {
                FileAttributes: 0,
                ReparseTag: 0,
            };
            // SAFETY: `handle` は有効、`tag` は大きさの合う書き込み先。
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    FileAttributeTagInfo,
                    (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                    std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
                )
            };
            // tag はログで見分けるためだけ。読めなくても reparse point として扱う（0 のまま）。
            if ok != 0 {
                reparse_tag = tag.ReparseTag;
            }
        }
        Ok(ObjectInfo {
            is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            is_reparse,
            reparse_tag,
            encrypted: info.dwFileAttributes & FILE_ATTRIBUTE_ENCRYPTED != 0,
            links: info.nNumberOfLinks,
        })
    }

    /// [`open_child_by_id`] の結果。
    pub(crate) enum ChildOpen {
        Opened(Opened),
        /// 列挙と開くの間に消えた。**開いた物の本当のパスが木の外**（列挙と開くの間に木の外へ
        /// 移された）ときもこれにする（書かない・数えない。#623 PR-1b、設計 §2.4 の 3）。
        Vanished,
        /// 開けなかった（利用者が Administrators を拒否した・所有権が無いなど。
        /// `ERROR_ACCESS_DENIED`）。特権は有効にしない。
        Denied,
        /// 使用中で開けなかった（共有違反 32・ロック違反 33。利用者が `DELETE` を持つハンドルで
        /// 開いている。#623 PR-1c）。中身は OS のエラー番号。
        Busy(u32),
    }

    // 子を列挙の後・ID で開く前に呼ぶフック（テスト専用。子を木の外へ移す反証に使う。
    // #623 PR-1b）。本番では `None`。
    #[cfg(test)]
    pub(crate) type BeforeChildOpenHook = Box<dyn Fn(&Path)>;
    #[cfg(test)]
    thread_local! {
        pub(crate) static BEFORE_CHILD_OPEN: std::cell::RefCell<Option<BeforeChildOpenHook>> =
            const { std::cell::RefCell::new(None) };
    }

    /// `parent`（ディレクトリのハンドル）の中の 128 ビットのファイル ID を、**パスを使わず**・
    /// 辿らずに開く（`OpenFileById`、`ExtendedFileIdType`、ヒント = 親のハンドル。監査 [A]:
    /// パスで開くと途中の要素のジャンクションを辿るので ID で相対に開く。Nit 2: 128 ビットの
    /// ID で ReFS・Dev Drive でも正しく指す）。
    ///
    /// **開いた後に、そのハンドルの本当のパスが `within`（木のルートの本当のパス）の中かを
    /// 確かめ、外なら [`ChildOpen::Vanished`] を返す**（#623 PR-1b、設計 §2.4 の 3。Fable-2 A-3）。
    /// 【仕様・実測】`OpenFileById` のヒントのハンドルはボリュームを選ぶためだけに使われ、開いた
    /// 物が今もその親の中にあることは保証されない。列挙の後・開く前に利用者が子を木の外の
    /// 自分のフォルダへ移すと（子の `DELETE` と移し先の作成の権利だけで足りる）、確かめが無いと
    /// 木の外の物の記述子を書く（テスト `an_object_moved_out_after_enumeration_is_not_opened`）。
    /// 確かめは `GetFinalPathNameByHandleW` 1 回（`VOLUME_NAME_DOS`。ドライブ文字の無い
    /// ボリュームでは失敗して fail closed - 既知の制限）。
    pub(crate) fn open_child_by_id(
        parent: HANDLE,
        file_id: [u8; 16],
        within: &Path,
    ) -> Result<ChildOpen> {
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_LOCK_VIOLATION, ERROR_PATH_NOT_FOUND,
            ERROR_SHARING_VIOLATION, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            OpenFileById, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128,
            FILE_ID_TYPE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let descriptor = windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR>(
            ) as u32,
            Type: 2 as FILE_ID_TYPE, // ExtendedFileIdType
            Anonymous: windows_sys::Win32::Storage::FileSystem::FILE_ID_DESCRIPTOR_0 {
                ExtendedFileId: FILE_ID_128 {
                    Identifier: file_id,
                },
            },
        };
        // SAFETY: `parent` は有効なディレクトリのハンドル。`descriptor` は 128 ビットの ID。
        // 共有に DELETE を含めない。リンクは辿らない（OPEN_REPARSE_POINT）。
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
                return Ok(ChildOpen::Vanished);
            }
            if os_error == ERROR_ACCESS_DENIED {
                return Ok(ChildOpen::Denied);
            }
            if os_error == ERROR_SHARING_VIOLATION || os_error == ERROR_LOCK_VIOLATION {
                return Ok(ChildOpen::Busy(os_error));
            }
            return Err(SafeRootError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        let handle = OwnedHandle(handle);
        let info = info_of(handle.0)?;
        // ハードリンク（ファイルでリンク数 2 以上）は本当のパスに**どれか 1 つの名前**が返り、木の
        // 外の名前のこともあるので、ここでは確かめずにそのまま返す（呼ぶ側は必ず書かずに飛ばす・
        // 断る）。それ以外は木の中か確かめる。
        let hardlink = !info.is_dir && info.links >= 2;
        if !hardlink && !crate::guard::is_same_or_inside(&final_path_of(handle.0)?, within) {
            // 列挙と開くの間に木の外へ移された。書かない・数えない（消えたのと同じ）。
            return Ok(ChildOpen::Vanished);
        }
        Ok(ChildOpen::Opened(Opened {
            handle,
            is_dir: info.is_dir,
            is_reparse: info.is_reparse,
            reparse_tag: info.reparse_tag,
            encrypted: info.encrypted,
            links: info.links,
        }))
    }

    /// 印のファイル（[`crate::mode::MARKER_NAME`]）用の記述子: SYSTEM・Administrators だけ、
    /// 継承のフラグなし、所有者 Administrators（利用者の ACE なし。設計 §2.9 (a)）。
    pub(crate) fn marker_descriptor() -> Result<LocalBox> {
        owned_protected_sd(&DirAccess::service_only(), false)
    }

    /// `handle` が指す物の 128 ビットのファイル ID（`FileIdInfo`。列挙の `FileIdExtdDirectoryInfo`
    /// と同じ ID の空間）。
    pub(crate) fn file_id_of(handle: HANDLE) -> Result<[u8; 16]> {
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
        };
        // SAFETY: 全フィールドが整数の構造体（ゼロは有効な値）。
        let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: `handle` は有効、`info` は大きさの合う書き込み先。
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(api(
                "GetFileInformationByHandleEx(FileIdInfo)",
                last_error(),
            ));
        }
        Ok(info.FileId.Identifier)
    }

    /// `dir`（開いているフォルダのハンドル）の中で、OS が [`crate::mode::MARKER_NAME`] の名前に
    /// 解決する物（印の席。[`probe_marker`]）。
    pub(crate) struct MarkerSlot {
        /// 128 ビットのファイル ID（列挙の子と比べて「これが印の席か」を決める）。
        pub(crate) id: [u8; 16],
        pub(crate) is_dir: bool,
        /// 本当のパス（実際の綴り。理由の表示用）。
        pub(crate) path: std::path::PathBuf,
    }

    /// `dir` の中の印の席を、**名前の綴りではなく OS の名前の解決**で探す（Fable レビュー
    /// M-1）: `MARKER_NAME` を相対に属性だけで開き（`OBJ_CASE_INSENSITIVE`。大文字小文字の
    /// 折り畳みは NTFS に任せる - `eq_ignore_ascii_case` では非 ASCII の折り畳みを再現できない）、
    /// その物のファイル ID と種類を返す。無ければ `None`。木を歩く部品は、列挙した子の ID が
    /// この ID と同じかで「印の席」を見分ける（綴りが `.BANTO-SAFE-ROOT` でも同じ物なら印、
    /// 大文字小文字を区別するフォルダで綴りだけ同じ別の物なら印ではない）。
    pub(crate) fn probe_marker(dir: HANDLE) -> Result<MarkerProbe> {
        let handle =
            match child_relative_handle(dir, crate::mode::MARKER_NAME, ChildHow::OpenAttributes) {
                Ok(Some(handle)) => handle,
                Ok(None) => return Ok(MarkerProbe::Absent),
                // 開けない席は「その子の問題」（オーナーレビュー P2）。木を歩く側は、列挙でその子を
                // ID で開いたときの `Denied`／`Busy` として記録・中断の方針（`OnSkip`）に載せる。
                Err(SafeRootError::Io {
                    os_error: Some(5 | 32 | 33),
                    ..
                }) => return Ok(MarkerProbe::Unreadable),
                Err(error) => return Err(error),
            };
        let info = info_of(handle.raw())?;
        Ok(MarkerProbe::Found(MarkerSlot {
            id: file_id_of(handle.raw())?,
            is_dir: info.is_dir,
            path: final_path_of(handle.raw())?,
        }))
    }

    /// [`probe_marker`] の結果。
    pub(crate) enum MarkerProbe {
        /// 席に物が無い。
        Absent,
        /// 席の物（ID・種類・本当のパス）。
        Found(MarkerSlot),
        /// 席の物を開けない（拒否・使用中）。席の同一性は分からないので、木を歩く側は印として
        /// 扱わず、列挙でその子を開いたときの `Denied`／`Busy` に任せる（`OnSkip` のとおり記録か
        /// 中断。`relax_tree`・`reset_acl` は兄弟と親の処理を続ける）。
        Unreadable,
    }

    /// [`place_marker`] が置けなかった理由。
    pub(crate) enum MarkerFailure {
        /// [`crate::mode::MARKER_NAME`] の名前に印でない物（フォルダ・リンク・ハードリンク）がある。
        Occupied(crate::error::Refusal),
        /// 既にある印を、誰かが書き込み・削除の権利で開いている（共有違反 32／ロック違反 33。
        /// 利用者が設定モードで作った偽の印を DELETE のハンドルで握っている筋）。
        Busy(u32),
        Io(SafeRootError),
    }

    /// `parent`（開いているフォルダのハンドル）に印のファイルを置く（設計 §2.9 (a)。
    /// `PinnedDir::ensure_marker` と 2 周目の [`walk_dir`] が共有）:
    ///
    /// - 無ければ、SYSTEM・Administrators だけ・所有者 Administrators・利用者の ACE なしの
    ///   記述子（[`marker_descriptor`]）を**作るのと同時に**付けて、相対で作る。
    /// - 作れなかったら、名前がフォルダ・リンク・ハードリンクで塞がっていないかを属性だけで
    ///   開いて確かめ、塞がっていれば [`MarkerFailure::Occupied`]（`FILE_NON_DIRECTORY_FILE` の
    ///   作成は、既にあるフォルダでは衝突ではなく拒否（5）になるため）。
    /// - 既にあれば、**共有 READ だけ・`DELETE` を求めて**開く（利用者が書き込み・削除の権利で
    ///   握っていれば共有違反 → [`MarkerFailure::Busy`]。設定モードで利用者が置いた偽の印を古い
    ///   `DELETE` のハンドルで後から消し、フォルダを空にして reparse 化する筋を止める）。開いた
    ///   ハンドルで属性を読み直して印の形（ファイル・reparse でない・リンク数 1）を確かめ、
    ///   印の記述子を**そのハンドルで**書き直す（持っている間は誰も新しく握れない）。
    ///
    /// 置いた後のフォルダ自身の reparse 属性の読み直しは呼ぶ側。
    pub(crate) fn place_marker(
        parent: HANDLE,
    ) -> std::result::Result<crate::mode::MarkerOutcome, MarkerFailure> {
        use crate::handle_io::{facts, file_refusal};
        use crate::mode::{MarkerOutcome, MARKER_NAME};
        let sd = marker_descriptor().map_err(MarkerFailure::Io)?;
        match child_relative_handle(parent, MARKER_NAME, ChildHow::CreateFile(Some(&sd))) {
            Ok(Some(created)) => {
                drop(created);
                return Ok(MarkerOutcome::Created);
            }
            Ok(None) => {}
            Err(error) => {
                let occupied =
                    match child_relative_handle(parent, MARKER_NAME, ChildHow::OpenAttributes) {
                        Ok(Some(existing)) => {
                            facts(existing.raw()).ok().and_then(|f| file_refusal(&f))
                        }
                        _ => None,
                    };
                return Err(match occupied {
                    Some(refusal) => MarkerFailure::Occupied(refusal),
                    None => MarkerFailure::Io(error),
                });
            }
        }
        let existing = match child_relative_handle(parent, MARKER_NAME, ChildHow::OpenFileForUpdate)
        {
            Ok(Some(existing)) => existing,
            Ok(None) => {
                return Err(MarkerFailure::Io(SafeRootError::io(
                    IoOp::OpenFile,
                    &std::io::Error::from(std::io::ErrorKind::NotFound),
                )))
            }
            Err(SafeRootError::Io {
                os_error: Some(os_error @ (32 | 33)),
                ..
            }) => return Err(MarkerFailure::Busy(os_error as u32)),
            Err(error) => return Err(MarkerFailure::Io(error)),
        };
        let info = facts(existing.raw()).map_err(MarkerFailure::Io)?;
        if let Some(refusal) = file_refusal(&info) {
            return Err(MarkerFailure::Occupied(refusal));
        }
        set_object_security(existing.raw(), &sd).map_err(MarkerFailure::Io)?;
        drop(existing);
        Ok(MarkerOutcome::Existed)
    }

    /// 木を歩くときに、締められない・確かめられない物に会ったら止めるか記録して続けるか
    /// （設計 §2.4 の末尾。**既定を置かない** - 取り違えると締めたつもりで穴が残るため、
    /// 呼ぶ側が必ず選ぶ）。
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum OnSkip {
        /// その場で止める（2 周目 `close_tree`）。
        Abort,
        /// 記録して続ける（1 周目 `scan_tree`、緩める `relax_tree`、昇格ヘルパーの `reset_acl`）。
        Record,
    }

    /// フォルダ自身と中身のどちらを先に書くか（設計 §2.4・§2.5）。
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum Order {
        /// 上から: フォルダに書いてから中を列挙する（締める。締めた後は利用者が名前を作れない
        /// ので列挙の結果が増えない）。
        RootFirst,
        /// 下から: 中を先に、フォルダ自身を最後に書く（緩める。ルートが最後 - 「ルートが
        /// 緩んでいる ⇔ 全体が緩み終わった」）。
        RootLast,
    }

    /// 書く記述子の組（`None` なら読むだけ）。
    pub(crate) struct WriteSet<'a> {
        /// フォルダ用（`OICI`）。
        pub(crate) dir_sd: &'a LocalBox,
        /// ファイル用（継承のフラグなし）。
        pub(crate) file_sd: &'a LocalBox,
        /// 印のファイル用（[`marker_descriptor`]。どのモードでも変えない）。
        pub(crate) marker_sd: &'a LocalBox,
    }

    /// 木を歩く 1 回分の指定と状態（[`walk_dir`]）。
    pub(crate) struct Walk<'a> {
        /// 木のルートの本当のパス（ID で開いた物がその中かを確かめる）。
        pub(crate) within: &'a Path,
        pub(crate) on_skip: OnSkip,
        pub(crate) order: Order,
        pub(crate) write: Option<WriteSet<'a>>,
        /// 厳しい確かめ（1 周目・2 周目は真、緩める・修復は偽）: EFS で暗号化された物と、
        /// [`crate::mode::MARKER_NAME`] の名前のフォルダ（印を置けない）を問題にする。
        pub(crate) strict: bool,
        /// フォルダに書いた直後・列挙の前に印のファイルを置く（2 周目だけ。[`place_marker`]。
        /// 締めた直後に古い書き込みのハンドルで空のフォルダを reparse にされる筋（Codex P1）を、
        /// 「空のフォルダを残さない」ことで塞ぐ）。
        pub(crate) place_markers: bool,
        /// 止める合図（各フォルダの境で 1 回見る）。
        pub(crate) stop: &'a dyn Fn() -> StopSignal,
        /// 進み具合の報告（子を開いて数えるごとに 1 回。`examined` は単調に増える。`written` は
        /// その時点までに書いた数で、開いたばかりの子の分は含まない）。
        pub(crate) progress: &'a mut dyn FnMut(&Progress),
        pub(crate) counts: Progress,
        /// [`OnSkip::Record`] で記録した物（会った順）。
        pub(crate) recorded: Vec<TreeProblem>,
    }

    /// [`walk_dir`] が止まった理由。
    #[derive(Debug)]
    pub(crate) enum WalkStop {
        /// [`OnSkip::Abort`] で会った物。
        Problem(TreeProblem),
        /// 止める合図。
        Stopped(StopSignal),
        /// 列挙・開く・書くの失敗。
        Io(SafeRootError),
    }

    impl From<SafeRootError> for WalkStop {
        fn from(error: SafeRootError) -> Self {
            Self::Io(error)
        }
    }

    impl Walk<'_> {
        fn problem(&mut self, problem: TreeProblem) -> std::result::Result<(), WalkStop> {
            match self.on_skip {
                OnSkip::Abort => Err(WalkStop::Problem(problem)),
                OnSkip::Record => {
                    self.recorded.push(problem);
                    Ok(())
                }
            }
        }

        /// `handle` に `sd` を書く。`child` が真なら（ルート以外）書いた直後にテストのフック
        /// `AFTER_CHILD_WRITTEN` を呼ぶ。
        fn write(
            &mut self,
            handle: HANDLE,
            sd: &LocalBox,
            child: bool,
        ) -> std::result::Result<(), WalkStop> {
            set_object_security(handle, sd)?;
            self.counts.written += 1;
            #[cfg(not(test))]
            let _ = child;
            #[cfg(test)]
            if child {
                AFTER_CHILD_WRITTEN.with(|hook| {
                    if let Some(hook) = hook.borrow().as_ref() {
                        hook(handle);
                    }
                });
            }
            Ok(())
        }

        fn tick(&mut self) {
            (self.progress)(&self.counts);
        }
    }

    /// 自前の再帰の深さの上限（監査 Nit 1。超えた段は見ない・書かない。インストール利用者が
    /// とても深いフォルダを作ってスタックをあふれさせるのを防ぐ）。#623 PR-1c で 512 から 64 に
    /// 下げた: 1 段ごとに `walk_dir` → 列挙 → `visit_child` → `visit_opened` の 4 つのフレームを
    /// 重ねるので、512 段ではサービス・テストの主スレッド（Windows の既定 1 MiB）でデバッグ
    /// ビルドがあふれた。管理する木（ChronoGazer・Hub）は 4 段以下で、64 段に届く木は利用者が
    /// 作った入れ子だけ（1 周目が `TooDeep` で断る）。
    pub(crate) const MAX_DEPTH: u32 = 64;

    /// 列挙した子 1 件（[`enumerate_dir`]）。`.`・`..` は含めない。
    pub(crate) struct DirEntryRaw {
        pub(crate) name: String,
        /// 128 ビットのファイル ID（[`open_child_by_id`] に渡す）。
        pub(crate) file_id: [u8; 16],
        /// 列挙の時点の属性（`FILE_ATTRIBUTE_*`）。**判断には使わない**（列挙と開くの間に
        /// 差し替えられうる。開いた後にハンドルで読み直す）。表示・分類の下ごしらえ用。
        pub(crate) attributes: u32,
        /// 列挙の時点の reparse の tag（reparse point でなければ 0）。
        pub(crate) reparse_tag: u32,
        /// 列挙の時点の大きさ（`EndOfFile`）。
        pub(crate) len: u64,
    }

    /// `dir_handle`（`FILE_LIST_DIRECTORY` を持つフォルダのハンドル）の中を、**パスを使わずに**
    /// 最初から列挙して、子ごとに `each` を呼ぶ（`GetFileInformationByHandleEx` の
    /// `FileIdExtdDirectoryRestartInfo` → `FileIdExtdDirectoryInfo`。`.`・`..` は除く）。
    /// [`walk_dir`] と `ManagedDir::list`（#623 PR-1b）で共有する。列挙の位置はファイル
    /// オブジェクトに付くので、同じハンドルを同時に列挙しない（呼ぶ側が直列化する）。
    /// `each` の失敗の型 `E` はそのまま返す（API の失敗は `From<SafeRootError>` で写す）。
    pub(crate) fn enumerate_dir<E: From<SafeRootError>>(
        dir_handle: HANDLE,
        each: &mut dyn FnMut(DirEntryRaw) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, GetFileInformationByHandleEx,
            FILE_ID_EXTD_DIR_INFO,
        };

        // 8 バイト境界のバッファ（`FILE_ID_EXTD_DIR_INFO` はポインタ整列が要る）。
        let mut buffer = vec![0u64; 8 * 1024];
        let byte_len = (buffer.len() * 8) as u32;
        let mut class = FileIdExtdDirectoryRestartInfo; // 1 回目は最初から。
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
                return Err(api("GetFileInformationByHandleEx", e).into());
            }
            class = FileIdExtdDirectoryInfo; // 2 回目以降は続きから。
            let base = buffer.as_ptr() as *const u8;
            let mut offset = 0usize;
            loop {
                // SAFETY: `offset` は直前の `NextEntryOffset` で進めた、バッファ内の境界。
                let entry = unsafe { &*(base.add(offset) as *const FILE_ID_EXTD_DIR_INFO) };
                let name_len = entry.FileNameLength as usize / 2;
                // SAFETY: `FileName` は可変長で `FileNameLength` バイト続く（バッファ内）。
                let name_u16 =
                    unsafe { std::slice::from_raw_parts(entry.FileName.as_ptr(), name_len) };
                let name = String::from_utf16_lossy(name_u16);
                if name != "." && name != ".." {
                    each(DirEntryRaw {
                        name,
                        file_id: entry.FileId.Identifier,
                        attributes: entry.FileAttributes,
                        reparse_tag: entry.ReparsePointTag,
                        len: u64::try_from(entry.EndOfFile).unwrap_or(0),
                    })?;
                }
                if entry.NextEntryOffset == 0 {
                    break;
                }
                offset += entry.NextEntryOffset as usize;
            }
        }
    }

    /// `dir_handle`（辿らずに開き、reparse point でないフォルダと確かめ済み）の木を歩く
    /// （設計 §2.3〜§2.5 の共通の部品。`PinnedDir::{scan_tree, close_tree, relax_tree,
    /// reset_acl}`）。**パスは一切使わない**: 子は親のハンドルを列挙して得た 128 ビットの
    /// ファイル ID を [`open_child_by_id`] で辿らずに開き、開いた後にハンドルで属性を読み直し、
    /// 本当のパスが `within` の中であることを確かめてから扱う。フォルダなら**その子のハンドルを
    /// 列挙元として**再帰する。
    ///
    /// - 各フォルダの境（書く前・列挙する前）で `stop` を 1 回見る。
    /// - [`Order::RootFirst`] はフォルダ自身に書いてから中を列挙する（締める）。
    ///   [`Order::RootLast`] は中を先に、フォルダ自身を最後に書く（緩める。列挙はまだ締まって
    ///   いる間に行うので結果は確定している）。
    /// - reparse point（tag を問わず）・ハードリンク（ファイルでリンク数 2 以上）・開けない・
    ///   使用中・EFS（`refuse_encrypted`）・深すぎる（[`MAX_DEPTH`]）は [`OnSkip`] のとおり止める
    ///   か記録する。どれにも書かない・辿らない。消えた子・木の外へ移された子は飛ばす。
    /// - 書くときは、フォルダに `dir_sd`、ファイルに `file_sd`、印のファイル（名前が
    ///   [`crate::mode::MARKER_NAME`]・ファイル・リンク数 1）には `marker_sd`。
    /// - `display` は表示用のパス（開くのには使わない）。`depth` はこのフォルダの段（ルート 0）。
    ///   ルート自身は数えない（呼ぶ側が `counts.examined` に 1 を入れておく）。
    pub(crate) fn walk_dir(
        walk: &mut Walk<'_>,
        dir_handle: HANDLE,
        display: &Path,
        depth: u32,
    ) -> std::result::Result<(), WalkStop> {
        match (walk.stop)() {
            StopSignal::None => {}
            signal => return Err(WalkStop::Stopped(signal)),
        }
        if depth >= MAX_DEPTH {
            return walk.problem(TreeProblem::TooDeep {
                path: display.to_path_buf(),
            });
        }
        if walk.order == Order::RootFirst {
            if let Some(set) = &walk.write {
                let sd = set.dir_sd;
                walk.write(dir_handle, sd, depth > 0)?;
            }
            if walk.place_markers {
                place_marker_in_walk(walk, dir_handle, display)?;
            }
        }
        // 印の席を OS の名前の解決で探す（綴りでは比べない。Fable レビュー M-1）。厳しい確かめ
        // （1 周目・2 周目）では、席をフォルダが塞いでいれば印を置けないので断る。
        // 席をフォルダが塞いでいれば `MarkerOccupied`（厳しい確かめ）。`Record`（1 周目）では
        // 記録して**残りの子の列挙は続ける**（1 周目は問題を全部集める。オーナーレビュー P2）。
        // 席を開けないときは印として扱わず、列挙でその子を開いたときの `Denied`／`Busy` に任せる。
        let marker_id = match probe_marker(dir_handle)? {
            MarkerProbe::Found(slot) if walk.strict && slot.is_dir => {
                walk.problem(TreeProblem::MarkerOccupied { path: slot.path })?;
                None
            }
            MarkerProbe::Found(slot) => Some(slot.id),
            MarkerProbe::Absent | MarkerProbe::Unreadable => None,
        };
        enumerate_dir(dir_handle, &mut |entry| {
            visit_child(
                walk,
                dir_handle,
                &display.join(&entry.name),
                marker_id == Some(entry.file_id),
                entry.file_id,
                depth,
            )
        })?;
        if walk.order == Order::RootLast {
            if let Some(set) = &walk.write {
                let sd = set.dir_sd;
                walk.write(dir_handle, sd, depth > 0)?;
            }
        }
        Ok(())
    }

    /// 2 周目: フォルダに書いた直後・列挙の前に印を置き（[`place_marker`]）、置けた後に
    /// **そのフォルダの属性をハンドルで読み直す**（Codex P1）。締めた後は利用者が名前を作れないが、
    /// 締める前から開いていた書き込みのハンドルで**空の**フォルダは reparse にできる。印を置いた
    /// 後は空でないので reparse にできず、印は利用者に消せない。置けなかったときは、その間に
    /// reparse にされていればそれを理由に（[`TreeProblem::ReparsePoint`]）、印の名前が塞がって
    /// いれば [`TreeProblem::MarkerOccupied`]、偽の印が握られていれば [`TreeProblem::Busy`]。
    fn place_marker_in_walk(
        walk: &mut Walk<'_>,
        dir_handle: HANDLE,
        display: &Path,
    ) -> std::result::Result<(), WalkStop> {
        let reparse_now = |handle: HANDLE| -> Option<TreeProblem> {
            match info_of(handle) {
                Ok(info) if info.is_reparse => Some(TreeProblem::ReparsePoint {
                    path: display.to_path_buf(),
                    tag: info.reparse_tag,
                }),
                _ => None,
            }
        };
        if let Err(failure) = place_marker(dir_handle) {
            if let Some(problem) = reparse_now(dir_handle) {
                return walk.problem(problem);
            }
            return match failure {
                MarkerFailure::Occupied(_) => walk.problem(TreeProblem::MarkerOccupied {
                    path: display.join(crate::mode::MARKER_NAME),
                }),
                MarkerFailure::Busy(os_error) => walk.problem(TreeProblem::Busy {
                    path: display.join(crate::mode::MARKER_NAME),
                    os_error,
                }),
                MarkerFailure::Io(error) => Err(WalkStop::Io(error)),
            };
        }
        // 置けた後にも読み直す（置けた後は reparse 化できないはずだが、多重の防御）。
        let info = info_of(dir_handle)?;
        if info.is_reparse {
            return walk.problem(TreeProblem::ReparsePoint {
                path: display.to_path_buf(),
                tag: info.reparse_tag,
            });
        }
        Ok(())
    }

    /// `is_marker`: この子が親の印の席（[`probe_marker`] の ID と同じ物）か。
    fn visit_child(
        walk: &mut Walk<'_>,
        parent: HANDLE,
        display_child: &Path,
        is_marker: bool,
        file_id: [u8; 16],
        depth: u32,
    ) -> std::result::Result<(), WalkStop> {
        #[cfg(test)]
        BEFORE_CHILD_OPEN.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(display_child);
            }
        });
        let opened = match open_child_by_id(parent, file_id, walk.within)? {
            ChildOpen::Opened(opened) => opened,
            ChildOpen::Vanished => return Ok(()),
            ChildOpen::Denied => {
                return walk.problem(TreeProblem::Denied {
                    path: display_child.to_path_buf(),
                })
            }
            ChildOpen::Busy(os_error) => {
                return walk.problem(TreeProblem::Busy {
                    path: display_child.to_path_buf(),
                    os_error,
                })
            }
        };
        walk.counts.examined += 1;
        // 進み具合は開いて数えた直後に 1 回（フォルダなら中へ下りる前。`written` はこの子の
        // 分をまだ含まない）。再帰の後に重ねて出すと、同じ `examined` の報告が並ぶ。
        walk.tick();
        let result = visit_opened(walk, &opened, display_child, is_marker, depth);
        drop(opened);
        result
    }

    fn visit_opened(
        walk: &mut Walk<'_>,
        opened: &Opened,
        display_child: &Path,
        is_marker: bool,
        depth: u32,
    ) -> std::result::Result<(), WalkStop> {
        if opened.is_reparse {
            // 触らない・辿らない。
            return walk.problem(TreeProblem::ReparsePoint {
                path: display_child.to_path_buf(),
                tag: opened.reparse_tag,
            });
        }
        if !opened.is_dir && opened.links >= 2 {
            // 記述子を共有するので触らない（外の名前のアクセスを変えない。#624）。
            return walk.problem(TreeProblem::Hardlink {
                path: display_child.to_path_buf(),
                links: opened.links,
            });
        }
        if walk.strict && opened.encrypted {
            return walk.problem(TreeProblem::Encrypted {
                path: display_child.to_path_buf(),
            });
        }
        if opened.is_dir {
            // 子のハンドルを列挙元として再帰する（ID とハンドルだけ。パスは使わない）。書く順は
            // `walk_dir` の中（`Order`）。
            return walk_dir(walk, opened.handle.raw(), display_child, depth + 1);
        }
        if let Some(set) = &walk.write {
            // フォルダは継承つき（OICI）、ファイルは継承フラグ無し（Nit 4）。印の席の物（綴りで
            // なく ID で見分ける）はどのモードでも印の記述子のまま（設計 §2.5・§2.9 (a)）。
            let sd = if is_marker {
                set.marker_sd
            } else {
                set.file_sd
            };
            walk.write(opened.handle.raw(), sd, true)?;
        }
        Ok(())
    }

    /// 開いた**ハンドルで**、そのオブジェクトと中の子へ、保護された DACL と所有者を書く
    /// （[`super::PinnedDir::reset_acl`]、#392 B3-3b の監査 Must 2・[A]。昇格ヘルパーの修復）。
    /// [`walk_dir`] を [`OnSkip::Record`]・[`Order::RootFirst`] で使い、reparse point・
    /// ハードリンク・開けない子は飛ばして [`super::ResetSkips`] に記録する（#626 の「飛ばして
    /// 報告」）。使用中の子・深すぎる木は今までどおり失敗にする。`recurse` が偽なら対象そのもの
    /// だけ書く（監査 Nit 3）。
    pub(super) fn set_security_tree(
        handle: HANDLE,
        real_dir: &Path,
        access: &DirAccess,
        recurse: bool,
        skips: &mut super::ResetSkips,
    ) -> Result<()> {
        ensure_user_account(access)?;
        let dir_sd = owned_protected_sd(access, true)?;
        if !recurse {
            return set_object_security(handle, &dir_sd);
        }
        let file_sd = owned_protected_sd(access, false)?;
        let marker_sd = marker_descriptor()?;
        let mut walk = Walk {
            within: real_dir,
            on_skip: OnSkip::Record,
            order: Order::RootFirst,
            write: Some(WriteSet {
                dir_sd: &dir_sd,
                file_sd: &file_sd,
                marker_sd: &marker_sd,
            }),
            strict: false,
            place_markers: false,
            stop: &|| StopSignal::None,
            progress: &mut |_| {},
            counts: Progress::default(),
            recorded: Vec::new(),
        };
        match walk_dir(&mut walk, handle, real_dir, 0) {
            Ok(()) => {}
            Err(WalkStop::Io(error)) => return Err(error),
            // `Record` は止めず、`stop` は常に続けるので来ない。来たら失敗にする。
            Err(WalkStop::Problem(problem)) => {
                return Err(walk_problem_error(problem));
            }
            Err(WalkStop::Stopped(_)) => {
                return Err(SafeRootError::io(
                    IoOp::InspectDir,
                    &std::io::Error::other("木の書き直しが途中で止まりました"),
                ));
            }
        }
        for problem in walk.recorded {
            match problem {
                TreeProblem::ReparsePoint { path, .. } => skips.reparse.push(path),
                TreeProblem::Hardlink { path, .. } => skips.hardlink.push(path),
                TreeProblem::Denied { path } => skips.denied.push(path),
                other @ (TreeProblem::Busy { .. }
                | TreeProblem::Encrypted { .. }
                | TreeProblem::MarkerOccupied { .. }
                | TreeProblem::TooDeep { .. }) => return Err(walk_problem_error(other)),
            }
        }
        Ok(())
    }

    /// [`set_security_tree`] が失敗にする問題を `SafeRootError` に写す（使用中は OS の番号を
    /// 保つ）。
    fn walk_problem_error(problem: TreeProblem) -> SafeRootError {
        match problem {
            TreeProblem::Busy { os_error, .. } => SafeRootError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ),
            other => SafeRootError::io(IoOp::InspectDir, &std::io::Error::other(other.to_string())),
        }
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
        observe(LocalBox(sd), owner, dacl)
    }

    /// 開いたハンドルから DACL と所有者を読む（`GetSecurityInfo`。パスを使わない。
    /// [`super::PinnedDir::read_dacl`]、#626 PR-A）。`handle` は `READ_CONTROL` を持つこと。
    pub(crate) fn read_dacl_of_handle(handle: HANDLE) -> Result<ObservedDacl> {
        use windows_sys::Win32::Security::Authorization::GetSecurityInfo;

        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `handle` は READ_CONTROL で開いた有効なハンドル。成功すれば `sd` は
        // LocalAlloc の領域で、`owner`・`dacl` はその中を指す（`sd` を解放するまで有効）。
        let status = unsafe {
            GetSecurityInfo(
                handle,
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
            return Err(api("GetSecurityInfo", status));
        }
        observe(LocalBox(sd), owner, dacl)
    }

    /// 読んだ記述子（`sd`）の中の所有者と DACL を [`ObservedDacl`] にする。`owner`・`dacl` は
    /// `sd` の中を指す（`sd` はここで持ち、読み終わってから解放する）。
    fn observe(sd: LocalBox, owner: PSID, dacl: *mut ACL) -> Result<ObservedDacl> {
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
                // SAFETY: `GetAce` が返した ACE は `AceSize` バイトの連続した領域。
                let bytes = unsafe {
                    std::slice::from_raw_parts(ace as *const u8, usize::from(header.AceSize))
                };
                let (mask, sid) = match ace_mask_and_sid_offset(header.AceType, bytes) {
                    Some((mask, offset)) => (mask, sid_in(bytes, offset)?),
                    // 種類の分からない ACE・形の壊れた ACE は SID を空にする（呼ぶ側の
                    // `guard::plan_carry_over` は引き継ぎを決めずに断る）。
                    None => (0, String::new()),
                };
                let observed = ObservedAce {
                    ace_type: header.AceType,
                    flags: header.AceFlags,
                    mask,
                    sid,
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

    /// DACL に現れうる ACE の、マスクと SID の位置（#626 PR-B の再レビュー 2: 条件付き・
    /// オブジェクトの ACE の SID も読む。読み落とすと、条件付きの拒否のある利用者を
    /// 引き継いでしまう）。並びは winnt.h の構造体どおり:
    ///
    /// - 許可・拒否（0・1）と callback（9・10）: ヘッダ 4・マスク 4・SID（callback は SID の
    ///   後に条件のデータ）。
    /// - オブジェクト（5・6）と callback object（11・12）: ヘッダ 4・マスク 4・Flags 4・
    ///   `Flags` の 1 ビット目で ObjectType（16）・2 ビット目で InheritedObjectType（16）・SID。
    ///
    /// 種類が分からない・`AceSize` が足りないときは `None`（純関数）。
    pub(super) fn ace_mask_and_sid_offset(ace_type: u8, bytes: &[u8]) -> Option<(u32, usize)> {
        let read_u32 = |at: usize| -> Option<u32> {
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let mask = read_u32(4)?;
        let offset = match ace_type {
            0 | 1 | 9 | 10 => 8,
            5 | 6 | 11 | 12 => {
                let flags = read_u32(8)?;
                let mut offset = 12;
                if flags & 0x1 != 0 {
                    offset += 16;
                }
                if flags & 0x2 != 0 {
                    offset += 16;
                }
                offset
            }
            _ => return None,
        };
        sid_len_at(bytes, offset).map(|_| (mask, offset))
    }

    /// `bytes[offset..]` に収まる SID の長さ（Revision 1・SubAuthorityCount ≤ 15）。
    fn sid_len_at(bytes: &[u8], offset: usize) -> Option<usize> {
        let revision = *bytes.get(offset)?;
        let count = usize::from(*bytes.get(offset + 1)?);
        let len = 8 + 4 * count;
        (revision == 1 && count <= 15 && bytes.len() >= offset + len).then_some(len)
    }

    /// `bytes[offset..]` の SID を文字列にする（[`ace_mask_and_sid_offset`] で長さを確かめた後）。
    fn sid_in(bytes: &[u8], offset: usize) -> Result<String> {
        let len = sid_len_at(bytes, offset).ok_or_else(|| api("ParseAceSid", 1337))?;
        // `ConvertSidToStringSidW` へ渡すため、揃ったバッファにコピーする（ACE の中の SID は
        // 4 バイト境界に揃っているが、念のため）。
        let mut copy = vec![0u32; len.div_ceil(4)];
        // SAFETY: `copy` は `len` バイト以上。`bytes[offset..offset + len]` は上で確かめた範囲。
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes[offset..offset + len].as_ptr(),
                copy.as_mut_ptr() as *mut u8,
                len,
            );
        }
        sid_string(copy.as_mut_ptr() as PSID)
    }

    /// drop で `CloseHandle` するハンドル（[`super::PinnedDir`]）。
    pub(crate) struct OwnedHandle(pub(crate) HANDLE);

    // SAFETY: カーネルのハンドルはプロセス全体で有効な値で、どのスレッドから使っても・閉じても
    // よい（スレッドに結び付いた状態を持たない）。閉じるのは持ち主の `Drop` だけ。ディレクトリの
    // 列挙の位置のようにファイルオブジェクトに付いた状態を同時に動かす使い方は、呼ぶ側が直列化
    // する（`PinnedDir` の列挙は `enum_lock` で包む。#623 PR-1b: サービスがプロセスの終わりまで
    // 握ったフォルダをスレッドをまたいで使うため）。同じく**位置を持つ操作（ファイルの読み・
    // ディレクトリの列挙）はハンドルを共有して同時に使わない**（`PinnedFile::read_to_end` は
    // `&mut self`、列挙は `enum_lock`）。
    unsafe impl Send for OwnedHandle {}
    // SAFETY: 上と同じ。`&OwnedHandle` から出来るのは生のハンドルを読むことだけ。
    unsafe impl Sync for OwnedHandle {}

    impl OwnedHandle {
        /// 生のハンドル（開いている間だけ使う。閉じない）。
        pub(crate) fn raw(&self) -> HANDLE {
            self.0
        }

        /// 所有を手放して生のハンドルを返す（`std::fs::File` などへ移すとき。閉じる責任は
        /// 受け取った側に移る）。
        pub(crate) fn into_raw(self) -> HANDLE {
            let handle = self.0;
            std::mem::forget(self);
            handle
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
    pub(crate) fn final_path_of(handle: HANDLE) -> Result<std::path::PathBuf> {
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
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
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
            return Err(SafeRootError::io(
                IoOp::InspectDir,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        pinned_from_handle(OwnedHandle(handle), dir).map(Some)
    }

    /// 開いたハンドルから [`super::PinnedDir`] を組み立てる（属性・所有者・本当のパスを
    /// ハンドルから読む。[`pin_dir`] と [`create_child_dir`] で共通）。
    fn pinned_from_handle(handle: OwnedHandle, dir: &Path) -> Result<super::PinnedDir> {
        use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT,
        };
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

        Ok(super::PinnedDir {
            handle,
            path: dir.to_path_buf(),
            final_path,
            info: super::PinnedDirInfo {
                is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                is_reparse_point: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
                owner,
            },
            enum_lock: std::sync::Mutex::new(()),
        })
    }

    /// [`super::PinnedDir::create_child_dir`] の本体。
    pub(super) fn create_child_dir(
        parent: &super::PinnedDir,
        name: &str,
        access: Option<&DirAccess>,
    ) -> Result<Option<super::PinnedDir>> {
        let descriptor = access.map(descriptor).transpose()?;
        child_relative(parent, name, ChildHow::Create(descriptor.as_ref()))?
            .map(|handle| pinned_from_handle(handle, &parent.path.join(name)))
            .transpose()
    }

    /// [`super::PinnedDir::open_child_dir`] の本体。
    pub(super) fn open_child_dir(
        parent: &super::PinnedDir,
        name: &str,
    ) -> Result<Option<super::PinnedDir>> {
        child_relative(parent, name, ChildHow::Open)?
            .map(|handle| pinned_from_handle(handle, &parent.path.join(name)))
            .transpose()
    }

    /// [`super::PinnedDir::open_child_file`] の本体（#630）。
    pub(super) fn open_child_file(
        parent: &super::PinnedDir,
        name: &str,
    ) -> Result<Option<super::PinnedFile>> {
        child_relative(parent, name, ChildHow::OpenFile)?
            .map(|handle| pinned_file_from_handle(handle, &parent.path.join(name)))
            .transpose()
    }

    /// [`super::PinnedDir::open_child_file_for_update`] の本体（#630 PR-2）。
    pub(super) fn open_child_file_for_update(
        parent: &super::PinnedDir,
        name: &str,
    ) -> Result<Option<super::PinnedFile>> {
        child_relative(parent, name, ChildHow::OpenFileForUpdate)?
            .map(|handle| pinned_file_from_handle(handle, &parent.path.join(name)))
            .transpose()
    }

    /// [`super::PinnedFile::reset_acl`] の本体（#630 PR-2）: 属性を読み直してから、ファイル用の
    /// 保護された記述子（所有者 Administrators）をハンドルに書き、読み戻して確かめる。
    pub(super) fn reset_file_acl(handle: HANDLE) -> Result<super::FileSecurity> {
        let before = security_of_handle(handle)?;
        if before.is_reparse_point {
            return Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint));
        }
        if before.is_dir {
            return Err(SafeRootError::io(
                IoOp::OpenFile,
                &std::io::Error::from(std::io::ErrorKind::IsADirectory),
            ));
        }
        if before.links >= 2 {
            return Err(SafeRootError::Exposed(vec![super::Exposure::HardLinked(
                before.links,
            )]));
        }
        let descriptor = owned_protected_sd(&DirAccess::service_only(), false)?;
        set_object_security(handle, &descriptor)?;
        let after = security_of_handle(handle)?;
        let found = super::exposure(&after);
        if !found.is_empty() {
            return Err(SafeRootError::Exposed(found));
        }
        Ok(after)
    }

    /// [`super::PinnedFile::delete`] の本体（#630 PR-2）。
    pub(super) fn delete_file_by_handle(handle: HANDLE) -> Result<()> {
        delete_by_handle(handle)
    }

    /// [`super::PinnedDir::create_child_file`] の本体（#630）。
    pub(super) fn create_child_file(
        parent: &super::PinnedDir,
        name: &str,
        contents: &[u8],
    ) -> Result<Option<super::PinnedFile>> {
        // ファイル用（継承のフラグなし）・所有者 Administrators・保護された DACL。
        let descriptor = owned_protected_sd(&DirAccess::service_only(), false)?;
        let Some(handle) = child_relative(parent, name, ChildHow::CreateFile(Some(&descriptor)))?
        else {
            return Ok(None);
        };
        // ここから先の失敗は、作ったハンドルで消してから返す（名前で消さない）。
        let finish = || -> Result<(super::FileSecurity, std::path::PathBuf)> {
            write_all_and_flush(handle.raw(), contents)?;
            let security = security_of_handle(handle.raw())?;
            #[cfg_attr(not(test), allow(unused_mut))]
            let mut found = super::exposure(&security);
            #[cfg(test)]
            if FORCE_EXPOSED.with(std::cell::Cell::get) {
                found.push(super::Exposure::NullDacl);
            }
            if !found.is_empty() {
                return Err(SafeRootError::Exposed(found));
            }
            Ok((security, final_path_of(handle.raw())?))
        };
        match finish() {
            Ok((security, final_path)) => Ok(Some(super::PinnedFile {
                handle,
                path: parent.path.join(name),
                final_path,
                security,
            })),
            Err(error) => {
                // 消す印を付けられれば、閉じたときに消える。付けられなかったときは、作ったときの
                // 保護された DACL（SYSTEM・Administrators だけ、所有者 Administrators）のまま
                // 残る。どちらでも元の失敗を返す（消す失敗で理由を上書きしない）。
                let _ = delete_by_handle(handle.raw());
                drop(handle);
                Err(error)
            }
        }
    }

    // `create_child_file` の読み戻しの確かめに、理由を 1 つ足す（テスト専用。保護されて
    // いなかったときに作ったファイルを消す経路の確かめ）。本番では常に偽。
    #[cfg(test)]
    thread_local! {
        pub(super) static FORCE_EXPOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// 開いたハンドルのファイルを `FileDispositionInfo` で消す印を付ける（閉じたときに消える。
    /// ハンドルは `DELETE` を持つこと。名前で消さないので、別の物を消さない）。
    pub(crate) fn delete_by_handle(handle: HANDLE) -> Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
        };
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: `handle` は DELETE を持つ有効なハンドル。`info` は呼び出しの間生きている。
        let ok = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                (&info as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(SafeRootError::io(
                IoOp::RemoveFile,
                &std::io::Error::from_raw_os_error(last_error() as i32),
            ));
        }
        Ok(())
    }

    /// `handle`（`FILE_WRITE_DATA`、同期 I/O）へ `contents` を全部書いて `FlushFileBuffers`。
    pub(crate) fn write_all_and_flush(handle: HANDLE, contents: &[u8]) -> Result<()> {
        use std::io::Write;
        use std::os::windows::io::FromRawHandle;
        // 閉じるのは持ち主（OwnedHandle）なので、`File` には持たせない（ManuallyDrop）。
        // SAFETY: `handle` はこの呼び出しの間開いている有効なファイルのハンドル。
        let mut file =
            std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(handle as _) });
        file.write_all(contents)
            .map_err(|e| SafeRootError::io(IoOp::WriteFile, &e))?;
        file.sync_all()
            .map_err(|e| SafeRootError::io(IoOp::WriteFile, &e))
    }

    /// [`super::PinnedFile::read_to_end`] の本体: 先頭から `max_len` バイトまで読む（超えたら
    /// 断る）。
    pub(super) fn read_handle_to_end(handle: HANDLE, max_len: usize) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        use std::os::windows::io::FromRawHandle;
        // SAFETY: `handle` はこの呼び出しの間開いている有効なファイルのハンドル（閉じない）。
        let mut file =
            std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(handle as _) });
        let read_error = |e: std::io::Error| SafeRootError::io(IoOp::ReadFile, &e);
        file.seek(SeekFrom::Start(0)).map_err(read_error)?;
        let mut contents = Vec::new();
        (&mut *file)
            .take((max_len as u64).saturating_add(1))
            .read_to_end(&mut contents)
            .map_err(read_error)?;
        if contents.len() > max_len {
            return Err(read_error(std::io::Error::from(
                std::io::ErrorKind::FileTooLarge,
            )));
        }
        Ok(contents)
    }

    /// 開いたハンドルから [`super::FileSecurity`] を読む（属性・リンク数・所有者・DACL）。
    /// `handle` は `READ_CONTROL | FILE_READ_ATTRIBUTES` を持つこと。
    pub(super) fn security_of_handle(handle: HANDLE) -> Result<super::FileSecurity> {
        let info = info_of(handle)?;
        Ok(super::FileSecurity {
            is_dir: info.is_dir,
            is_reparse_point: info.is_reparse,
            links: info.links,
            dacl: read_dacl_of_handle(handle)?,
        })
    }

    /// 開いたハンドルから [`super::PinnedFile`] を組み立てる。
    fn pinned_file_from_handle(handle: OwnedHandle, path: &Path) -> Result<super::PinnedFile> {
        let security = security_of_handle(handle.raw())?;
        let final_path = final_path_of(handle.raw())?;
        Ok(super::PinnedFile {
            handle,
            path: path.to_path_buf(),
            final_path,
            security,
        })
    }

    /// [`super::inspect_file`] の本体（#630）。
    pub(super) fn inspect_file(path: &Path) -> Result<Option<super::FileSecurity>> {
        use windows_sys::Win32::Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING, READ_CONTROL,
        };
        let wide_path = wide(path);
        // SAFETY: `wide_path` は NUL 終端。リンクを辿らず（OPEN_REPARSE_POINT）、フォルダも
        // 開けるように（BACKUP_SEMANTICS）。中身は求めない（属性と記述子だけ）ので、共有は
        // すべて許す（ほかのプロセスの邪魔をしない）。
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
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
            return Err(SafeRootError::io(
                IoOp::OpenFile,
                &std::io::Error::from_raw_os_error(os_error as i32),
            ));
        }
        let handle = OwnedHandle(handle);
        security_of_handle(handle.raw()).map(Some)
    }

    /// [`child_relative`] が作るか開くか（権利・共有・作り方が決まる）。
    #[derive(Clone, Copy)]
    pub(crate) enum ChildHow<'a> {
        /// フォルダを `FILE_CREATE | FILE_DIRECTORY_FILE` で作る（既にあれば `Ok(None)`）。
        /// `Some` ならその記述子（保護された DACL）を作るのと同時に付ける。
        Create(Option<&'a LocalBox>),
        /// `FILE_OPEN`（無ければ `Ok(None)`）。フォルダに限らない（ファイルでも開いて、
        /// 呼ぶ側が `PinnedDirInfo::is_dir` で断る - [`pin_dir`] と同じ）。
        Open,
        /// `FILE_OPEN`（無ければ `Ok(None)`）で、読み取りだけの権利・読み取りだけの共有（#630・
        /// #648。中身を読む）。種類を問わず開き、呼ぶ側が属性で断る。
        OpenFile,
        /// `FILE_OPEN`（無ければ `Ok(None)`）で、記述子を書く・消すための権利（#630 PR-2）。
        /// 中身は求めない。種類を問わず開き、呼ぶ側が属性で断る。
        OpenFileForUpdate,
        /// ファイルを `FILE_CREATE | FILE_NON_DIRECTORY_FILE` で、この記述子を付けて作る
        /// （既にあれば `Ok(None)`。#630）。書き込みと、失敗したときに消すための `DELETE` を
        /// 求め、共有は読み取りだけ。`None` なら記述子を渡さず、親のフォルダから継承する
        /// （#623 PR-1b の `ManagedDir::create_new`・`write_atomic` の一時ファイル）。
        CreateFile(Option<&'a LocalBox>),
        /// 追記で開く（#623 PR-1b の `ManagedDir::open_append`）。`create` なら無ければ作る
        /// （`FILE_OPEN_IF`）、偽なら `FILE_OPEN`（無ければ `Ok(None)`）。権利は
        /// `FILE_APPEND_DATA` と属性・記述子の読み取りだけ（`FILE_WRITE_DATA` を持たないので、
        /// 書き込みは必ず末尾に付く）。共有は読み取りだけ。種類を問わず開き、呼ぶ側が属性で断る。
        OpenAppend { create: bool },
        /// 消すために開く（#623 PR-1b の `ManagedDir::remove`）。`DELETE` と属性の読み取り。
        /// 共有はすべて許す（読んでいる人がいても消せる。POSIX の削除で名前はすぐ消える）。
        /// 種類を問わず開き、呼ぶ側が**開いた後の**属性で断る。
        OpenForDelete,
        /// 属性だけを読むために開く（#623 PR-1b の `ManagedDir::reopen_path`）。中身の権利を
        /// 求めず、共有はすべて許す（相手の邪魔をしない）。
        OpenAttributes,
    }

    /// 開いている `parent` を `RootDirectory` に、名前 1 要素の子を `NtCreateFile` で作るか
    /// 開く（`FILE_OPEN_REPARSE_POINT`。パスを解決し直さず、子がリンクでも辿らない）。
    /// フォルダ（`Create`・`Open`）は [`pin_dir`] と同じ権利（`WRITE_DAC | WRITE_OWNER` を
    /// 含む）・共有（削除なし）・バックアップの意図つきで開く。ファイル（`OpenFile`・
    /// `OpenFileForUpdate`・`CreateFile`）は [`ChildHow`] のとおりで、どれも共有は読み取り
    /// だけ、バックアップの意図は付けない（DACL だけを門にする。#648）。
    pub(crate) fn child_relative(
        parent: &super::PinnedDir,
        name: &str,
        how: ChildHow<'_>,
    ) -> Result<Option<OwnedHandle>> {
        child_relative_handle(parent.handle.raw(), name, how)
    }

    /// [`child_relative`] の、親を生のハンドルで受ける形（木を歩く部品が、ID で開いた子の
    /// フォルダに印を置くため。#623 PR-1c の Codex P1）。`parent` は開いているフォルダのハンドル。
    pub(crate) fn child_relative_handle(
        parent: HANDLE,
        name: &str,
        how: ChildHow<'_>,
    ) -> Result<Option<OwnedHandle>> {
        use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
        use windows_sys::Wdk::Storage::FileSystem::FILE_OPEN_IF;
        use windows_sys::Wdk::Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        };
        use windows_sys::Win32::Foundation::{
            RtlNtStatusToDosError, OBJ_CASE_INSENSITIVE, STATUS_OBJECT_NAME_COLLISION,
            STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, UNICODE_STRING,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_LIST_DIRECTORY,
            FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_WRITE_DATA, READ_CONTROL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER,
        };
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

        /// `ERROR_INVALID_NAME`。
        const ERROR_INVALID_NAME: u32 = 123;
        let is_file = !matches!(how, ChildHow::Create(_) | ChildHow::Open);
        // 失敗の文言に出す API の名前。作る経路と開く経路を見分けられるようにする
        // （開く経路の名前の誤りを `CreateChildDir` と読ませない。#628 / #646 Nit 3）。
        let label = match how {
            ChildHow::Create(_) => "CreateChildDir(name)",
            ChildHow::Open => "OpenChildDir(name)",
            ChildHow::OpenFile
            | ChildHow::OpenFileForUpdate
            | ChildHow::OpenAppend { create: false }
            | ChildHow::OpenForDelete
            | ChildHow::OpenAttributes => "OpenChildFile(name)",
            ChildHow::CreateFile(_) | ChildHow::OpenAppend { create: true } => {
                "CreateChildFile(name)"
            }
        };
        // `NtCreateFile` は Win32 の正規化を通らないので、Win32 から見ると別の名前・別の
        // 物になるもの（末尾の `.` と空白・ワイルドカード・予約デバイス名など）も断る
        // （#641 のオーナーレビュー Nit 2）。
        if !crate::guard::is_plain_child_name(name) {
            return Err(api(label, ERROR_INVALID_NAME));
        }
        let wide_name: Vec<u16> = name.encode_utf16().collect();
        let bytes =
            u16::try_from(wide_name.len() * 2).map_err(|_| api(label, ERROR_INVALID_NAME))?;
        let unicode = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: wide_name.as_ptr() as *mut u16,
        };
        // フォルダは `pin_dir` と同じ権利（`SYNCHRONIZE` は `FILE_SYNCHRONOUS_IO_NONALERT` の
        // ため）。
        let dir_access = READ_CONTROL
            | FILE_READ_ATTRIBUTES
            | FILE_LIST_DIRECTORY
            | WRITE_DAC
            | WRITE_OWNER
            | SYNCHRONIZE;
        let read_access = READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_DATA | SYNCHRONIZE;
        let (descriptor, disposition, kind_option, desired, share) = match how {
            ChildHow::Create(descriptor) => (
                descriptor,
                FILE_CREATE,
                FILE_DIRECTORY_FILE,
                dir_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            ),
            ChildHow::Open => (
                None,
                FILE_OPEN,
                0,
                dir_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            ),
            // 共有は読み取りだけ（#648 の P1）: 書き込みで開いているハンドルが既にあれば、開く
            // こと自体が共有違反で失敗し、持っている間はほかの書き込みの開きも失敗する（ACL を
            // 締める前から書き込みで開いていた主体が、確かめた後に中身を書き換えられない）。
            ChildHow::OpenFile => (None, FILE_OPEN, 0, read_access, FILE_SHARE_READ),
            // #630 PR-2: 中身は求めず、記述子を書く・消す権利。共有は `OpenFile` と同じく
            // 読み取りだけ（書き込み中のファイルの所有者・DACL を書き換えない・消さない）。
            ChildHow::OpenFileForUpdate => (
                None,
                FILE_OPEN,
                0,
                READ_CONTROL
                    | FILE_READ_ATTRIBUTES
                    | WRITE_DAC
                    | WRITE_OWNER
                    | DELETE
                    | SYNCHRONIZE,
                FILE_SHARE_READ,
            ),
            ChildHow::CreateFile(descriptor) => (
                descriptor,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE,
                read_access | FILE_WRITE_DATA | DELETE,
                FILE_SHARE_READ,
            ),
            // #623 PR-1b: 作るときも開くときもファイルの指定（`FILE_NON_DIRECTORY_FILE`）を
            // 付けない - 付けると既にあるフォルダ・ジャンクションが「開けない」の失敗になり、
            // 呼ぶ側が理由（reparse point の tag）を返せない。属性は開いた後に呼ぶ側が読む。
            // 作るときに何も付けなければ普通のファイルができる。
            ChildHow::OpenAppend { create } => (
                None,
                if create { FILE_OPEN_IF } else { FILE_OPEN },
                0,
                READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_APPEND_DATA | SYNCHRONIZE,
                FILE_SHARE_READ,
            ),
            ChildHow::OpenForDelete => (
                None,
                FILE_OPEN,
                0,
                READ_CONTROL | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ),
            ChildHow::OpenAttributes => (
                None,
                FILE_OPEN,
                0,
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent,
            ObjectName: &unicode,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: descriptor.map_or(std::ptr::null(), |sd| sd.0 as *const _),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: 全フィールドが整数・ポインタの構造体（ゼロは有効な値）。
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: `attributes` が指す名前・記述子は呼び出しの間だけ生きていればよい
        // （`wide_name` はこの関数の終わりまで、記述子は呼ぶ側が持つ）。`RootDirectory` は
        // 開いている親のハンドル。
        // ファイル（#630）はバックアップの意図を付けない（#648 のオーナー Should 1。特権が
        // 有効なトークンで DACL を迂回させず、DACL だけを門にする）。フォルダは今のまま。
        let backup_intent = if is_file {
            0
        } else {
            FILE_OPEN_FOR_BACKUP_INTENT
        };
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                desired,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                share,
                disposition,
                kind_option
                    | FILE_OPEN_REPARSE_POINT
                    | FILE_SYNCHRONOUS_IO_NONALERT
                    | backup_intent,
                std::ptr::null(),
                0,
            )
        };
        let absent = match how {
            ChildHow::Create(_) | ChildHow::CreateFile(_) => status == STATUS_OBJECT_NAME_COLLISION,
            // 無ければ作るので「無い」は起きない。
            ChildHow::OpenAppend { create: true } => false,
            ChildHow::Open
            | ChildHow::OpenFile
            | ChildHow::OpenFileForUpdate
            | ChildHow::OpenAppend { create: false }
            | ChildHow::OpenForDelete
            | ChildHow::OpenAttributes => {
                status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_OBJECT_PATH_NOT_FOUND
            }
        };
        if absent {
            return Ok(None);
        }
        if status < 0 {
            // SAFETY: NTSTATUS を Win32 のエラーに写すだけ。
            let os_error = unsafe { RtlNtStatusToDosError(status) };
            // ファイル（#630）は `io::ErrorKind` を残す（sink がアクセス拒否を見分けて案内する
            // ため）。フォルダは今までどおり。
            return Err(match how {
                ChildHow::OpenFile
                | ChildHow::OpenFileForUpdate
                | ChildHow::OpenAppend { create: false }
                | ChildHow::OpenForDelete
                | ChildHow::OpenAttributes => SafeRootError::io(
                    IoOp::OpenFile,
                    &std::io::Error::from_raw_os_error(os_error as i32),
                ),
                ChildHow::CreateFile(_) | ChildHow::OpenAppend { create: true } => {
                    SafeRootError::io(
                        IoOp::CreateFile,
                        &std::io::Error::from_raw_os_error(os_error as i32),
                    )
                }
                ChildHow::Create(_) | ChildHow::Open => api("NtCreateFile", os_error),
            });
        }
        Ok(Some(OwnedHandle(handle)))
    }

    pub(crate) fn current_user_sid_string() -> Result<String> {
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
pub(crate) mod tests {
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
                    Err(SafeRootError::InvalidUserSid(_))
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

    /// #626 PR-A: 複数の利用者。0 人・1 人は既存の 2 つと同じ値・同じ SDDL で、重複は
    /// まとめ、`install_user()` は 1 人のときだけ返す。
    #[test]
    fn multi_user_access_keeps_the_single_forms_and_lists_every_user() {
        const OTHER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
        let other = InstallUserSid::parse(OTHER).unwrap();
        assert_eq!(
            DirAccess::with_install_users(Vec::new()),
            DirAccess::service_only()
        );
        assert_eq!(
            DirAccess::with_install_users([user()]),
            DirAccess::with_install_user(user())
        );
        assert_eq!(
            DirAccess::with_install_users([user(), user()]),
            DirAccess::with_install_user(user())
        );
        let multi = DirAccess::with_install_users([user(), other.clone(), user()]);
        assert_eq!(multi.install_users(), &[user(), other.clone()][..]);
        assert_eq!(multi.install_user(), None);
        assert_eq!(DirAccess::service_only().install_user(), None);
        assert_eq!(
            DirAccess::with_install_user(user()).install_user(),
            Some(&user())
        );
        assert_eq!(
            multi.sddl(),
            format!(
                "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1301bf;;;{USER})(A;OICI;0x1301bf;;;{OTHER})"
            )
        );
    }

    /// #626 PR-A: 複数の利用者の確かめ。全員の「変更」がちょうど 1 つずつあり、所有者は
    /// どの利用者でもよい。1 人欠けても・余分な利用者がいても・利用者が重なっても断る。
    #[test]
    fn check_accepts_exactly_every_user_of_a_multi_user_access() {
        const OTHER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
        const THIRD: &str = "S-1-5-21-1111111111-2222222222-3333333333-1003";
        let other = InstallUserSid::parse(OTHER).unwrap();
        let multi = DirAccess::with_install_users([user(), other.clone()]);
        let both = |owner: &str| {
            let mut dacl = good(owner);
            dacl.aces
                .as_mut()
                .unwrap()
                .push(ace(OTHER, MODIFY_ACCESS_MASK));
            dacl
        };
        for owner in [SYSTEM_SID, ADMINISTRATORS_SID, USER, OTHER] {
            assert_eq!(check_dacl(&both(owner), &multi), Ok(()), "{owner}");
        }
        let mut reordered = both(SYSTEM_SID);
        reordered.aces.as_mut().unwrap().reverse();
        assert_eq!(check_dacl(&reordered, &multi), Ok(()));
        // 1 人欠ける（今の 1 人の形）。
        assert_eq!(
            check_dacl(&good(SYSTEM_SID), &multi),
            Err(AclMismatch::MissingAce {
                sid: OTHER.to_owned(),
                mask: MODIFY_ACCESS_MASK
            })
        );
        // 余分な利用者（1 人の指定に 2 人の DACL）。
        assert!(matches!(
            check_dacl(&both(SYSTEM_SID), &DirAccess::with_install_user(user())),
            Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == OTHER
        ));
        // 3 人目・重なった ACE。
        let mut third = both(SYSTEM_SID);
        third
            .aces
            .as_mut()
            .unwrap()
            .push(ace(THIRD, MODIFY_ACCESS_MASK));
        assert!(matches!(
            check_dacl(&third, &multi),
            Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == THIRD
        ));
        let mut duplicated = both(SYSTEM_SID);
        duplicated
            .aces
            .as_mut()
            .unwrap()
            .push(ace(OTHER, MODIFY_ACCESS_MASK));
        assert!(matches!(
            check_dacl(&duplicated, &multi),
            Err(AclMismatch::UnexpectedAce { ref sid, .. }) if sid == OTHER
        ));
        // 利用者でない所有者。
        assert_eq!(
            check_dacl(&both(THIRD), &multi),
            Err(AclMismatch::DisallowedOwner {
                sid: THIRD.to_owned()
            })
        );
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

    // --- #630: 保護されているかの判定（exposure） ------------------------------------

    const USERS: &str = "S-1-5-32-545";

    fn any_ace(ace_type: u8, flags: u8, mask: u32, sid: &str) -> ObservedAce {
        ObservedAce {
            ace_type,
            flags,
            mask,
            sid: sid.to_owned(),
        }
    }

    /// SYSTEM・Administrators だけに許可、所有者 Administrators の普通のファイル（保護されている）。
    fn protected_file() -> FileSecurity {
        FileSecurity {
            is_dir: false,
            is_reparse_point: false,
            links: 1,
            dacl: ObservedDacl {
                protected: true,
                owner: ADMINISTRATORS_SID.to_owned(),
                aces: Some(vec![
                    any_ace(0, 0, FULL_ACCESS_MASK, SYSTEM_SID),
                    any_ace(0, 0, FULL_ACCESS_MASK, ADMINISTRATORS_SID),
                ]),
            },
        }
    }

    fn with_ace(ace: ObservedAce) -> FileSecurity {
        let mut security = protected_file();
        security.dacl.aces.as_mut().unwrap().push(ace);
        security
    }

    fn allows(ace: &ObservedAce) -> Vec<Exposure> {
        vec![Exposure::AllowsOther {
            ace_type: ace.ace_type,
            flags: ace.flags,
            mask: ace.mask,
            sid: ace.sid.clone(),
        }]
    }

    /// 判定の表（1 行 = 入力 1 つと、期待する理由のすべて）。
    #[test]
    fn exposure_table() {
        let trusted_owner_cases = [SYSTEM_SID, ADMINISTRATORS_SID, TRUSTED_INSTALLER_SID];
        // 1. 保護されている: 所有者が信頼する主体・許可の相手が信頼する主体だけ。
        for owner in trusted_owner_cases {
            let mut security = protected_file();
            security.dacl.owner = owner.to_owned();
            assert_eq!(exposure(&security), vec![], "{owner}");
        }
        // TrustedInstaller への許可・継承された信頼する主体の ACE・保護されていない DACL も
        // それ自体では理由にしない。
        let mut inherited_trusted =
            with_ace(any_ace(0, 0x10, FULL_ACCESS_MASK, TRUSTED_INSTALLER_SID));
        inherited_trusted.dacl.protected = false;
        inherited_trusted.dacl.aces.as_mut().unwrap()[0].flags = 0x10;
        assert_eq!(exposure(&inherited_trusted), vec![]);
        // フォルダはリンク数を見ない（フォルダのリンク数は意味が違う）。
        let mut dir = protected_file();
        dir.is_dir = true;
        dir.links = 3;
        assert_eq!(exposure(&dir), vec![]);

        // 2. 許可の ACE の相手が信頼する主体以外（種類・フラグ・マスクを問わない）。
        let others = [
            any_ace(0, 0x10, 0x0012_00A9, USERS), // Users の読み取り（継承）
            any_ace(0, 0, 0x0012_00A9, "S-1-1-0"), // Everyone
            any_ace(0, 0, 0x0012_00A9, "S-1-5-11"), // Authenticated Users
            any_ace(0, 0, 0x0012_00A9, "S-1-5-4"), // INTERACTIVE
            any_ace(0, 0, MODIFY_ACCESS_MASK, USER), // 個別の利用者
            any_ace(0, 0, 0x0000_0002, USER),     // 書き込みだけ
            any_ace(0, 0, 0x8000_0000, USER),     // GENERIC_READ だけ
            any_ace(0, 0, 0x1000_0000, USERS),    // GENERIC_ALL
            any_ace(0, 0x0b, 0x0012_00A9, "S-1-3-0"), // CREATOR OWNER（OI CI IO）
            any_ace(0, 0x08, 0x0012_00A9, USERS), // INHERIT_ONLY も数える
            any_ace(0, 0, 0, USERS),              // マスク 0 も数える
            any_ace(5, 0, 0x0012_00A9, USERS),    // オブジェクトの許可
            any_ace(9, 0, 0x0012_00A9, USER),     // 条件付きの許可
            any_ace(11, 0, 0x0012_00A9, USER),    // 条件付き・オブジェクトの許可
        ];
        for ace in &others {
            assert_eq!(exposure(&with_ace(ace.clone())), allows(ace), "{ace:?}");
        }

        // 3. 拒否の ACE は、それ自体では理由にしない（種類 1・6・10・12）。
        for ace_type in guard::DENY_ACE_TYPES {
            let ace = any_ace(ace_type, 0, FULL_ACCESS_MASK, USERS);
            assert_eq!(exposure(&with_ace(ace.clone())), vec![], "{ace:?}");
        }

        // 4. 種類の分からない ACE（許可の複合 4・監査 2・ラベル 0x11 など）は安全側に数える。
        for ace_type in [2u8, 3, 4, 7, 8, 13, 0x11, 0xff] {
            let ace = any_ace(ace_type, 0, 0x0012_00A9, "");
            assert_eq!(
                exposure(&with_ace(ace)),
                vec![Exposure::UnknownAce {
                    ace_type,
                    flags: 0,
                    mask: 0x0012_00A9
                }],
                "{ace_type}"
            );
        }

        // 5. SID が読めない ACE（許可でも拒否でも）は安全側に数える。
        for ace_type in [0u8, 1, 5, 6, 9, 10, 11, 12] {
            let ace = any_ace(ace_type, 0x03, 0x0012_00A9, "");
            assert_eq!(
                exposure(&with_ace(ace)),
                vec![Exposure::UnreadableSid {
                    ace_type,
                    flags: 0x03,
                    mask: 0x0012_00A9
                }],
                "{ace_type}"
            );
        }

        // 6. 所有者が信頼する主体以外。
        for owner in [USER, USERS, "S-1-1-0", "S-1-3-0", "S-1-5-19", ""] {
            let mut security = protected_file();
            security.dacl.owner = owner.to_owned();
            assert_eq!(
                exposure(&security),
                vec![Exposure::ForeignOwner(owner.to_owned())],
                "{owner:?}"
            );
        }

        // 7. null DACL（ACE は見ない）。
        let mut null = protected_file();
        null.dacl.aces = None;
        assert_eq!(exposure(&null), vec![Exposure::NullDacl]);

        // 8. reparse point（ファイルでもフォルダでも）。
        for is_dir in [false, true] {
            let mut link = protected_file();
            link.is_dir = is_dir;
            link.is_reparse_point = true;
            assert_eq!(exposure(&link), vec![Exposure::ReparsePoint], "{is_dir}");
        }

        // 9. ファイルでリンク数 2 以上。
        for links in [2u32, 3, 1024] {
            let mut linked = protected_file();
            linked.links = links;
            assert_eq!(exposure(&linked), vec![Exposure::HardLinked(links)]);
        }

        // 10. 重なれば全部を、決まった順に返す（1 つの ACE からは高々 1 つ）。
        let users = any_ace(0, 0x10, 0x0012_00A9, USERS);
        let mut worst = with_ace(users.clone());
        worst.is_reparse_point = true;
        worst.links = 2;
        worst.dacl.owner = USER.to_owned();
        worst
            .dacl
            .aces
            .as_mut()
            .unwrap()
            .extend([any_ace(1, 0, FULL_ACCESS_MASK, USER), any_ace(4, 0, 1, "")]);
        let mut expected = vec![
            Exposure::ReparsePoint,
            Exposure::HardLinked(2),
            Exposure::ForeignOwner(USER.to_owned()),
        ];
        expected.extend(allows(&users));
        expected.push(Exposure::UnknownAce {
            ace_type: 4,
            flags: 0,
            mask: 1,
        });
        assert_eq!(exposure(&worst), expected);
        worst.dacl.aces = None;
        assert_eq!(
            exposure(&worst),
            vec![
                Exposure::ReparsePoint,
                Exposure::HardLinked(2),
                Exposure::ForeignOwner(USER.to_owned()),
                Exposure::NullDacl,
            ]
        );
    }

    #[test]
    fn trusted_installer_sid_is_the_well_known_service_sid() {
        assert_eq!(
            TRUSTED_INSTALLER_SID,
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        );
        assert!(is_trusted_sid(TRUSTED_INSTALLER_SID));
        // 似た形のサービスの SID は信頼しない。
        assert!(!is_trusted_sid(
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478465"
        ));
        assert!(!is_trusted_sid("S-1-5-80-0"));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_fails_closed() {
        let dir = std::path::Path::new("/tmp/banto-secret-store-unsupported");
        let access = DirAccess::service_only();
        assert_eq!(
            ensure_dir(dir, &access),
            Err(SafeRootError::UnsupportedPlatform)
        );
        assert_eq!(
            verify_dir(dir, &access),
            Err(SafeRootError::UnsupportedPlatform)
        );
        assert_eq!(
            reset_dir_acl(dir, &access),
            Err(SafeRootError::UnsupportedPlatform)
        );
        assert_eq!(
            create_dir_if_absent(dir, &access),
            Err(SafeRootError::UnsupportedPlatform)
        );
        assert_eq!(
            check_user_account(&user()),
            Err(SafeRootError::UnsupportedPlatform)
        );
        assert_eq!(
            inspect_file(&dir.join("banto-hub-sink.toml")),
            Err(SafeRootError::UnsupportedPlatform)
        );
    }

    #[cfg(windows)]
    pub(crate) mod windows {
        use super::super::*;

        pub(crate) fn me() -> InstallUserSid {
            current_user_sid().expect("今の利用者の SID")
        }

        /// 昇格した管理者として走っているか（GitHub の Windows のランナーは管理者）。
        /// `CI`の環境変数があるのに管理者でなければ panic する（管理者でしか走らない反証が
        /// 黙って飛ばされないように。#626 PR-B の監査 B-3）。手元（`CI`なし）では`false`を
        /// 返し、呼ぶ側が skip する。
        pub(crate) fn is_elevated_admin() -> bool {
            let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
            if !admin && std::env::var_os("CI").is_some() {
                panic!("CI なのに昇格した管理者ではない（管理者でしか走らない反証が走らない）");
            }
            admin
        }

        /// ファイル用の確かめ。ファイルへは継承のフラグ（OICI）を外した記述子を書くので、
        /// 全 ACE の flags が 0 であることを確かめ、フォルダ用の `check_dacl` が期待する
        /// 形（flags = OICI）に読み替えて、所有者・PROTECTED・SID と mask の組を確かめる。
        fn check_file_dacl(observed: &ObservedDacl, access: &DirAccess) {
            let mut as_dir = observed.clone();
            for ace in as_dir.aces.as_mut().expect("DACL がある") {
                assert_eq!(ace.flags, 0, "ファイルの ACE に継承のフラグ: {observed:?}");
                ace.flags = INHERIT_FLAGS;
            }
            check_dacl(&as_dir, access).unwrap_or_else(|e| panic!("{e:?}: {observed:?}"));
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
                matches!(err, SafeRootError::AclMismatch(AclMismatch::NotProtected)),
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
            imp::ensure_dir(&dir, &group_access).ok();
            assert!(dir.exists());

            for err in [
                ensure_dir(&dir, &group_access).unwrap_err(),
                verify_dir(&dir, &group_access).unwrap_err(),
            ] {
                assert!(matches!(err, SafeRootError::InvalidUserSid(_)), "{err:?}");
            }
            // ACL の形とは関係なく、既存の経路でも SID を先に確かめる（ACL が違う
            // フォルダでも、返るのは AclMismatch ではなく InvalidUserSid）。
            let plain = root.path().join("plain");
            std::fs::create_dir(&plain).unwrap();
            assert!(matches!(
                ensure_dir(&plain, &group_access),
                Err(SafeRootError::InvalidUserSid(_))
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
                Err(SafeRootError::AclMismatch(AclMismatch::NotADirectory))
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
                    Err(SafeRootError::DotSegmentInPath),
                    "{dir:?}"
                );
                assert_eq!(
                    ensure_dir(&dir, &access),
                    Err(SafeRootError::DotSegmentInPath),
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
                Err(SafeRootError::InvalidUserSid(_))
            ));
            assert!(!dir.exists(), "拒んだときはフォルダを作らない");
            assert!(!create_dir_if_absent(root.path(), &group_access).unwrap());
        }

        /// ジャンクション（`mklink /J`。管理者でなくても作れる）を作る。
        pub(crate) fn junction(link: &std::path::Path, target: &std::path::Path) {
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
            let reset = pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree);
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
                    Err(SafeRootError::AclApi {
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
                pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree),
                Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint))
            );
            drop(pinned);
            assert_eq!(read_dir_dacl(outside.path()).unwrap(), before);
            std::fs::remove_dir(&link).unwrap();

            let file = root.path().join("file");
            std::fs::write(&file, b"x").unwrap();
            let pinned = pin_dir(&file).unwrap().unwrap();
            assert!(!pinned.info().is_dir);
            assert_eq!(
                pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree),
                Err(SafeRootError::AclMismatch(AclMismatch::NotADirectory))
            );
            assert_eq!(
                pin_dir(&root.path().join("x").join("..").join("plain")).unwrap_err(),
                SafeRootError::DotSegmentInPath
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
            let skips = pinned.reset_acl(&access, Recurse::Tree).unwrap();
            drop(pinned);

            // 普通の子（孫まで）は保護された DACL ＋ Administrators 所有。フォルダは継承の
            // フラグ（OICI）付き、ファイルは継承無し（flags 0）の記述子。
            for path in [&dir, &normal, &deep] {
                let dacl = read_dir_dacl(path).unwrap();
                assert_eq!(
                    dacl.owner,
                    ADMINISTRATORS_SID,
                    "{}: {dacl:?}",
                    path.display()
                );
                if path == &deep {
                    check_file_dacl(&dacl, &access);
                } else {
                    check_dacl(&dacl, &access).unwrap();
                }
            }
            // ジャンクション・ハードリンクは飛ばして記録。先は変えない。
            // 記録は本当のパス（TEMP が 8.3 の短い名前でも長い名前で返る）。
            assert_eq!(skips.reparse, vec![real_dir.join("jlink")]);
            assert_eq!(skips.hardlink, vec![real_dir.join("hard.txt")]);
            assert_eq!(read_dir_dacl(outside.path()).unwrap(), outside_before);
            assert_eq!(read_dir_dacl(&outside_file).unwrap(), outside_file_before);

            std::fs::remove_dir(dir.join("jlink")).unwrap();
        }

        /// 監査 Should 1: 開けない子（Administrators を拒否した DACL）があっても、全体を
        /// 止めず、飛ばして `ResetSkips::denied` に記録し、ほかの子は書く。特権は有効にしない。
        /// 所有者を移すので管理者のとき（CI）だけ走る。
        #[test]
        fn a_child_that_denies_administrators_is_skipped_and_the_rest_continue() {
            if !is_elevated_admin() {
                eprintln!(
                    "skip a_child_that_denies_administrators_is_skipped_and_the_rest_continue: 昇格していない（CI で確かめる）"
                );
                return;
            }
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let normal = dir.join("normal.txt");
            std::fs::write(&normal, b"x").unwrap();
            let denied = dir.join("denied.txt");
            std::fs::write(&denied, b"x").unwrap();
            // `denied` の DACL を明示の拒否で作る。BA（Administrators）への拒否は、昇格した
            // 利用者（CI）でも開けなくする。OW（OWNER RIGHTS）への拒否は、所有者が暗黙に持つ
            // READ_CONTROL・WRITE_DAC を取り上げる。WRITE_OWNER は特権を有効にしないので通らない。
            let me = me();
            deny_everyone_but_system(&denied);
            assert_eq!(
                open_for_write_dac_owner(&denied),
                Err(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED),
                "テストの前提: denied.txt は WRITE_DAC | WRITE_OWNER で開けない"
            );

            let access = DirAccess::with_install_user(me.clone());
            let pinned = pin_dir(&dir).unwrap().unwrap();
            let skips = pinned.reset_acl(&access, Recurse::Tree).unwrap();
            drop(pinned);

            let real_dir = read_dir_dacl(&dir).unwrap();
            assert_eq!(real_dir.owner, ADMINISTRATORS_SID);
            // 開けない子は飛ばして記録、ほかは書けている。
            assert_eq!(skips.denied.len(), 1, "{skips:?}");
            assert!(skips.denied[0].ends_with("denied.txt"), "{skips:?}");
            // ほかの子は書けている。所有者だけでなく ACE（SYSTEM・Administrators・インストール
            // 利用者の形、継承しない）まで確かめる（#628 の 7）。
            let normal_dacl = read_dir_dacl(&normal).unwrap();
            assert_eq!(normal_dacl.owner, ADMINISTRATORS_SID, "{normal_dacl:?}");
            check_file_dacl(&normal_dacl, &access);
            // 開けない子は消せないかもしれない。tempdir の drop の失敗は無視される。
            drop(root);
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
            let _ = pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree);
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
            let _ = pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree);
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
            let _ = pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree);
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

        /// `path` の DACL を `D:P(D;;FA;;;BA)(D;;WDWO;;;OW)(A;;FA;;;SY)` にする（Administrators と
        /// OWNER RIGHTS への明示の拒否）。
        pub(crate) fn deny_everyone_but_system(path: &std::path::Path) {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::LocalFree;
            use windows_sys::Win32::Security::Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            };
            use windows_sys::Win32::Security::{SetFileSecurityW, DACL_SECURITY_INFORMATION};

            let sddl: Vec<u16> = "D:P(D;;FA;;;BA)(D;;WDWO;;;OW)(A;;FA;;;SY)"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: NUL 終端の SDDL。確保された記述子は下で LocalFree する。
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "SDDL の変換: {}", last_error_test());
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: NUL 終端のパスと、上で作った有効な記述子。
            let ok =
                unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
            let error = last_error_test();
            // SAFETY: ConvertString...が LocalAlloc した記述子。
            unsafe { LocalFree(descriptor) };
            assert_ne!(ok, 0, "SetFileSecurityW: {error}");
        }

        /// `path` を `WRITE_DAC | WRITE_OWNER` で開く。開けなければ Win32 のエラー番号。
        fn open_for_write_dac_owner(path: &std::path::Path) -> std::result::Result<(), u32> {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::Storage::FileSystem::{
                CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
                OPEN_EXISTING, WRITE_DAC, WRITE_OWNER,
            };

            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: NUL 終端のパス。バックアップ権限は使わない（特権は有効にしない）。
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    WRITE_DAC | WRITE_OWNER,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(last_error_test());
            }
            // SAFETY: 上で開いた有効なハンドル。
            unsafe { CloseHandle(handle) };
            Ok(())
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
            // Codex の Nit 5: フックが発火したこと・FSCTL が成功したことを記録して assert する。
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;
            let empty_for_hook = empty.clone();
            let outside_for_hook = outside.path().to_path_buf();
            let fired = Arc::new(AtomicBool::new(false));
            let swap_ok = Arc::new(AtomicBool::new(false));
            let fired2 = fired.clone();
            let swap_ok2 = swap_ok.clone();
            super::super::imp::AFTER_CHILD_WRITTEN.with(|h| {
                *h.borrow_mut() = Some(Box::new(move |_child_handle| {
                    if !fired2.swap(true, Ordering::SeqCst) {
                        let ok = set_reparse_to(&empty_for_hook, &outside_for_hook).is_ok();
                        swap_ok2.store(ok, Ordering::SeqCst);
                    }
                }));
            });
            let pinned = pin_dir(&dir).unwrap().unwrap();
            let result = pinned.reset_acl(&DirAccess::with_install_user(me()), Recurse::Tree);
            drop(pinned);
            super::super::imp::AFTER_CHILD_WRITTEN.with(|h| *h.borrow_mut() = None);

            if is_elevated_admin() {
                // 管理者では子まで進むので、フックが発火し、すり替え（FSCTL）も成功したうえで、
                // outside が変わらないことを確かめる。
                assert!(result.is_ok(), "{result:?}");
                assert!(fired.load(Ordering::SeqCst), "フックが発火していない");
                assert!(
                    swap_ok.load(Ordering::SeqCst),
                    "FSCTL_SET_REPARSE_POINT が失敗した（反証にならない）"
                );
            } else {
                eprintln!(
                    "note: 非管理者なので子まで進まず（1307）、フックは発火していない - 書く側は CI"
                );
                assert!(!fired.load(Ordering::SeqCst));
            }
            let after: Vec<_> = watched.iter().map(|p| read_dir_dacl(p).unwrap()).collect();
            assert_eq!(after, before, "再帰の途中のすり替えで outside が変わった");
            // 後片付け（`empty` がジャンクションなら外す）。
            let _ = (result, std::fs::remove_dir(&empty));
        }

        /// `dir` を `target` へのジャンクションにする（`FSCTL_SET_REPARSE_POINT`。`dir` は
        /// 空のフォルダ。テスト専用）。
        pub(crate) fn set_reparse_to(
            dir: &std::path::Path,
            target: &std::path::Path,
        ) -> std::io::Result<()> {
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
                Err(SafeRootError::InvalidUserSid(_))
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
            assert!(matches!(err, SafeRootError::InvalidUserSid(_)), "{err:?}");
            assert!(!dir.exists(), "拒んだときはフォルダを作らない");
        }

        // --- #626 PR-A: 複数の利用者・ハンドルからの読み取り ----------------------------

        /// 2 人目の利用者のアカウント（同じ領域の組み込みの Guest・DefaultAccount・
        /// Administrator のどれか。どれも `SidTypeUser` で、無効でも解決できる）。この PC で
        /// 1 つも解決できなければ `None`（理由を出して skip する）。本物のアカウントは
        /// 変えない（DACL に SID を書くだけ）。
        fn another_user() -> Option<InstallUserSid> {
            let me = me();
            let domain = me.as_str().rsplit_once('-').unwrap().0;
            ["501", "503", "500"]
                .into_iter()
                .filter_map(|rid| InstallUserSid::parse(&format!("{domain}-{rid}")).ok())
                .find(|sid| *sid != me && check_user_account(sid).is_ok())
        }

        /// 作る・確かめる（昇格していなくても走る）: 複数の利用者の DACL で作り、読み
        /// 戻すと SY・BA のフルと、利用者ごとの「変更」がちょうど 1 つずつ並ぶ。1 人の
        /// 指定では確かめが通らない。
        #[test]
        fn a_multi_user_directory_is_created_and_verified() {
            let Some(other) = another_user() else {
                eprintln!("skip a_multi_user_directory_is_created_and_verified: 2 人目の利用者のアカウントが無い");
                return;
            };
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("multi");
            let access = DirAccess::with_install_users([me(), other.clone()]);
            ensure_dir(&dir, &access).unwrap();

            let dacl = read_dir_dacl(&dir).unwrap();
            assert!(dacl.protected, "{dacl:?}");
            let mut aces: Vec<_> = dacl
                .aces
                .as_ref()
                .unwrap()
                .iter()
                .map(|a| (a.ace_type, a.flags, a.sid.clone(), a.mask))
                .collect();
            aces.sort();
            let mut expected = vec![
                (0, INHERIT_FLAGS, SYSTEM_SID.to_owned(), FULL_ACCESS_MASK),
                (
                    0,
                    INHERIT_FLAGS,
                    ADMINISTRATORS_SID.to_owned(),
                    FULL_ACCESS_MASK,
                ),
                (
                    0,
                    INHERIT_FLAGS,
                    me().as_str().to_owned(),
                    MODIFY_ACCESS_MASK,
                ),
                (
                    0,
                    INHERIT_FLAGS,
                    other.as_str().to_owned(),
                    MODIFY_ACCESS_MASK,
                ),
            ];
            expected.sort();
            assert_eq!(aces, expected, "{dacl:?}");
            has_no_broad_groups(&dacl);

            verify_dir(&dir, &access).unwrap();
            // 利用者の順を変えても同じ形として通る。
            verify_dir(&dir, &DirAccess::with_install_users([other.clone(), me()])).unwrap();
            // 1 人だけの指定では、もう 1 人の ACE が余分。
            let err = verify_dir(&dir, &DirAccess::with_install_user(me())).unwrap_err();
            assert!(
                matches!(
                    err,
                    SafeRootError::AclMismatch(AclMismatch::UnexpectedAce { ref sid, .. })
                        if sid == other.as_str()
                ),
                "{err:?}"
            );
        }

        /// 読むだけ（昇格していなくても走る）: `PinnedDir::read_dacl` は開いたハンドルから、
        /// パスで読むのと同じ DACL と所有者を返す。
        #[test]
        fn read_dacl_reads_the_same_descriptor_from_the_handle() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("plain");
            std::fs::create_dir(&dir).unwrap();
            let pinned = pin_dir(&dir).unwrap().unwrap();
            let from_handle = pinned.read_dacl().unwrap();
            assert_eq!(from_handle, read_dir_dacl(&dir).unwrap());
            assert_eq!(from_handle.owner, pinned.info().owner);
            // 継承のフォルダ（tempdir の下）なので保護されておらず、継承の ACE がある。
            assert!(!from_handle.protected, "{from_handle:?}");
            assert!(from_handle
                .aces
                .as_ref()
                .unwrap()
                .iter()
                .any(|a| a.flags & INHERITED_ACE_FLAG != 0));
        }

        /// 書き直す（管理者のとき = CI）: 複数の利用者の `DirAccess` で `reset_acl` すると、
        /// フォルダ・中のフォルダ・ファイルのどれも、読み戻した DACL が期待どおり（所有者は
        /// Administrators）。書き直した後にハンドルから読んだ DACL を `plan_carry_over` に
        /// かけると、ちょうどその利用者たちを引き継ぎ、何も落とさない。
        #[test]
        fn a_multi_user_reset_writes_every_user_and_carries_them_over() {
            if !is_elevated_admin() {
                eprintln!(
                    "skip a_multi_user_reset_writes_every_user_and_carries_them_over: 昇格していない（CI で確かめる）"
                );
                return;
            }
            let Some(other) = another_user() else {
                eprintln!("skip a_multi_user_reset_writes_every_user_and_carries_them_over: 2 人目の利用者のアカウントが無い");
                return;
            };
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            let child = dir.join("child");
            std::fs::create_dir_all(&child).unwrap();
            let file = child.join("f.txt");
            std::fs::write(&file, b"x").unwrap();

            let access = DirAccess::with_install_users([me(), other.clone()]);
            let pinned = pin_dir(&dir).unwrap().unwrap();
            // 書き直す前の DACL（tempdir の継承）からは、利用者を 1 人も引き継がない
            // （継承の ACE は落とす側）。
            let before = guard::plan_carry_over(&pinned.read_dacl().unwrap(), guard::account_kind);
            assert!(before.carried.is_empty(), "{before:?}");
            assert!(before
                .dropped
                .iter()
                .all(|d| d.reason == guard::DropReason::Inherited));

            let skips = pinned.reset_acl(&access, Recurse::Tree).unwrap();
            assert_eq!(skips, ResetSkips::default());
            for path in [&dir, &child] {
                let dacl = read_dir_dacl(path).unwrap();
                assert_eq!(dacl.owner, ADMINISTRATORS_SID, "{}", path.display());
                check_dacl(&dacl, &access)
                    .unwrap_or_else(|e| panic!("{}: {e:?} {dacl:?}", path.display()));
            }
            let file_dacl = read_dir_dacl(&file).unwrap();
            assert_eq!(file_dacl.owner, ADMINISTRATORS_SID);
            check_file_dacl(&file_dacl, &access);

            let after = guard::plan_carry_over(&pinned.read_dacl().unwrap(), guard::account_kind);
            assert_eq!(after.carried, vec![me(), other]);
            assert!(after.dropped.is_empty(), "{after:?}");
            assert!(!after.null_dacl);
            drop(pinned);
        }

        // --- #626 PR-B の再レビュー -------------------------------------------------

        /// `path` の DACL を `sddl` にする（テスト専用。自分が所有者の一時フォルダだけ）。
        pub(crate) fn set_dacl_sddl(path: &std::path::Path, sddl: &str) {
            use std::os::windows::ffi::OsStrExt;
            use windows_sys::Win32::Foundation::LocalFree;
            use windows_sys::Win32::Security::Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            };
            use windows_sys::Win32::Security::{SetFileSecurityW, DACL_SECURITY_INFORMATION};

            let text: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: NUL 終端の SDDL。確保された記述子は下で LocalFree する。
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0, "SDDL の変換（{sddl}）: {}", last_error_test());
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: NUL 終端のパスと、上で作った有効な記述子。
            let ok =
                unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
            let error = last_error_test();
            // SAFETY: ConvertString...が LocalAlloc した記述子。
            unsafe { LocalFree(descriptor) };
            assert_ne!(ok, 0, "SetFileSecurityW: {error}");
        }

        /// 再レビュー 2: 実際の DACL に条件付きの拒否（`XD`、ACCESS_DENIED_CALLBACK）を付けて
        /// 読み戻すと、SID が読め、その利用者は引き継がれない（拒否として数える）。条件付きの
        /// 許可（`XA`）は引き継がない。管理者でなくても走る（自分の一時フォルダの DACL だけ）。
        #[test]
        fn conditional_aces_are_read_back_and_block_carry_over() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("cond");
            std::fs::create_dir(&dir).unwrap();
            let me = me();
            set_dacl_sddl(
                &dir,
                &format!(
                    "D:P(A;OICI;FA;;;SY)(A;OICI;0x1301bf;;;{me})(XD;OICI;0x40000;;;{me};(Member_of {{SID(BA)}}))",
                    me = me.as_str()
                ),
            );
            let dacl = read_dir_dacl(&dir).unwrap();
            let conditional = dacl
                .aces
                .as_ref()
                .unwrap()
                .iter()
                .find(|ace| ace.ace_type == 10)
                .expect("条件付きの拒否（種類 10）が読み戻せる");
            assert_eq!(conditional.sid, me.as_str(), "{dacl:?}");
            assert_eq!(conditional.mask, 0x0004_0000, "{dacl:?}");
            let plan = guard::plan_carry_over(&dacl, guard::account_kind);
            assert!(plan.carried.is_empty(), "{plan:?}");
            assert!(plan.blocked.is_empty(), "{plan:?}");
            assert!(plan
                .dropped
                .iter()
                .any(|d| d.reason == guard::DropReason::HasDeny));

            // 条件付きの許可（XA）だけの利用者は引き継がない。
            set_dacl_sddl(
                &dir,
                &format!(
                    "D:P(A;OICI;FA;;;SY)(XA;OICI;0x1301bf;;;{me};(Member_of {{SID(BA)}}))",
                    me = me.as_str()
                ),
            );
            let dacl = read_dir_dacl(&dir).unwrap();
            let plan = guard::plan_carry_over(&dacl, guard::account_kind);
            assert!(plan.carried.is_empty(), "{plan:?}");
            assert!(plan.dropped.iter().any(
                |d| d.reason == guard::DropReason::ConditionalAllow && d.ace.sid == me.as_str()
            ));
            // 片付け（読み書きできる形に戻す）。
            set_dacl_sddl(&dir, &format!("D:(A;OICI;FA;;;{})", me.as_str()));
        }

        /// 再レビュー 1: 確かめ済みの親のハンドルを基準に子を相対で作る。継承の子・保護された
        /// 子のどちらも、作ったハンドルの本当のパスは親の本当のパス + 名前で、既にあれば
        /// 作らない。名前 1 要素でないものは断る。
        #[test]
        fn create_child_dir_creates_relative_to_the_handle() {
            let root = tempfile::tempdir().unwrap();
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let child = parent.create_child_dir("plain", None).unwrap().unwrap();
            assert_eq!(child.final_path(), parent.final_path().join("plain"));
            assert!(child.info().is_dir && !child.info().is_reparse_point);
            assert!(!read_dir_dacl(&root.path().join("plain")).unwrap().protected);
            let service = DirAccess::service_only();
            let locked = parent
                .create_child_dir("locked", Some(&service))
                .unwrap()
                .unwrap();
            assert_eq!(locked.final_path(), parent.final_path().join("locked"));
            let dacl = read_dir_dacl(&root.path().join("locked")).unwrap();
            assert!(dacl.protected, "{dacl:?}");
            assert!(parent.create_child_dir("plain", None).unwrap().is_none());
            // #641 のオーナーレビュー Nit 2: `NtCreateFile` は Win32 の正規化を通らないので、
            // 末尾の `.` と空白・予約デバイス名（`COM0`・`LPT0` を含む）・Win32 で使えない
            // 文字も断る（確かめを外すと、`x.`・`nul` などは本当に作れてしまう）。
            for bad in [
                "", ".", "..", "a\\b", "a/b", "c:", "x:y", "x.", "x ", "x. ", "nul", "COM0",
                "lpt0", "con.txt", "a*", "a?", "a<b", "a>b", "a|b", "a\"b",
            ] {
                assert!(parent.create_child_dir(bad, None).is_err(), "{bad:?}");
                assert!(parent.open_child_dir(bad).is_err(), "{bad:?}");
            }
            assert_eq!(
                std::fs::read_dir(root.path())
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect::<std::collections::BTreeSet<_>>(),
                ["locked", "plain"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                "断った名前で何かが作られた"
            );
            drop((child, locked));
            // 片付け（保護された子は自分に権利が無いので、所有者として DACL を戻す）。
            set_dacl_sddl(
                &root.path().join("locked"),
                &format!("D:(A;OICI;FA;;;{})", me().as_str()),
            );
        }

        /// #641 のオーナーレビュー Nit 5: 確かめ済みの親のハンドルを基準に、既にある子を相対で
        /// 辿らずに開く。無ければ `None`、ファイル・ジャンクションでもそのものを開く（呼ぶ側が
        /// 属性で断る）。開いた子は親の本当のパス + 名前。
        #[test]
        fn open_child_dir_opens_relative_to_the_handle_without_following() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("sub")).unwrap();
            std::fs::write(root.path().join("file.txt"), b"x").unwrap();
            let outside = tempfile::tempdir().unwrap();
            junction(&root.path().join("link"), outside.path());
            let parent = pin_dir(root.path()).unwrap().unwrap();

            let sub = parent.open_child_dir("sub").unwrap().unwrap();
            assert_eq!(sub.final_path(), parent.final_path().join("sub"));
            assert!(sub.info().is_dir && !sub.info().is_reparse_point);
            assert!(parent.open_child_dir("missing").unwrap().is_none());
            let file = parent.open_child_dir("file.txt").unwrap().unwrap();
            assert!(!file.info().is_dir);
            let link = parent.open_child_dir("link").unwrap().unwrap();
            assert!(link.info().is_reparse_point, "{link:?}");
            assert_eq!(link.final_path(), parent.final_path().join("link"));
            // 開いている間は消せない（共有に削除を含めない）。
            assert!(std::fs::remove_dir(root.path().join("sub")).is_err());
            drop((sub, file, link, parent));
            std::fs::remove_dir(root.path().join("link")).unwrap();
        }

        /// 再レビュー 1: 親を開いて持ったまま、その場で FSCTL で外へのジャンクションに
        /// 変えても、親のハンドルを基準に作る子は外にできない（パスを解決し直さない）。
        #[test]
        fn create_child_dir_under_a_handle_turned_into_a_junction_does_not_leak() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("d");
            std::fs::create_dir(&dir).unwrap();
            let outside = tempfile::tempdir().unwrap();
            let before: Vec<_> = std::fs::read_dir(outside.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let pinned = pin_dir(&dir).unwrap().unwrap();
            if let Err(e) = set_reparse_to(&dir, outside.path()) {
                if std::env::var_os("CI").is_some() {
                    panic!("CI で FSCTL_SET_REPARSE_POINT が失敗した: {e}");
                }
                eprintln!("skip: FSCTL_SET_REPARSE_POINT に権限が無い（{e}）- CI で確かめる");
                return;
            }
            let result = pinned.create_child_dir("x", None);
            eprintln!("note: ジャンクションに変わったハンドルの下に作った結果: {result:?}");
            drop(result);
            drop(pinned);
            let after: Vec<_> = std::fs::read_dir(outside.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(after, before, "ジャンクションの先に子ができた");
            assert!(!outside.path().join("x").exists());
            let _ = std::fs::remove_dir(&dir);
        }

        // --- #630: ファイルの DACL を辿らずに読む・ハンドル基準の子のファイル -------------

        const USERS_SID: &str = "S-1-5-32-545";

        fn allows_to(found: &[Exposure], sid: &str) -> Option<u8> {
            found.iter().find_map(|e| match e {
                Exposure::AllowsOther { sid: s, flags, .. } if s == sid => Some(*flags),
                _ => None,
            })
        }

        /// 今のルート（`%ProgramData%\BantoHub`）の再現: Users に読み取りを継承で渡すフォルダの
        /// 中に作ったファイルは、[`inspect_file`] で読むと Users への許可（継承の ACE）が
        /// 理由になる。管理者でなくても走る（自分の一時フォルダだけ）。
        #[test]
        fn inspect_file_reports_an_inherited_users_read() {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("root");
            std::fs::create_dir(&dir).unwrap();
            let me = me();
            set_dacl_sddl(
                &dir,
                &format!(
                    "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{me})(A;OICI;0x1200a9;;;BU)",
                    me = me.as_str()
                ),
            );
            let file = dir.join("banto-hub-sink.toml");
            std::fs::write(&file, b"api_key = \"dummy\"").unwrap();

            let security = inspect_file(&file).unwrap().unwrap();
            assert!(
                !security.is_dir && !security.is_reparse_point,
                "{security:?}"
            );
            assert_eq!(security.links, 1);
            assert!(!security.dacl.protected, "継承した DACL: {security:?}");
            let found = exposure(&security);
            let users = allows_to(&found, USERS_SID).expect("Users への許可が理由になる");
            assert_ne!(users & INHERITED_ACE_FLAG, 0, "継承の ACE: {found:?}");
            assert!(allows_to(&found, me.as_str()).is_some(), "{found:?}");
            if security.dacl.owner == me.as_str() {
                assert!(found.contains(&Exposure::ForeignOwner(me.as_str().to_owned())));
            }
            // 無いもの・`.`/`..` を含むパス。
            assert_eq!(inspect_file(&dir.join("missing.toml")), Ok(None));
            assert_eq!(
                inspect_file(&dir.join("..").join("root").join("banto-hub-sink.toml")),
                Err(SafeRootError::DotSegmentInPath)
            );
        }

        /// [`inspect_file`] はリンクを辿らず、そのものの属性を読む（ジャンクション・ハードリンク・
        /// フォルダ）。null DACL・条件付きの許可・拒否の ACE も実物で読み戻して判定する。
        #[test]
        fn inspect_file_reads_links_dirs_and_odd_dacls_without_following() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let me = me();

            let dir = root.path().join("sub");
            std::fs::create_dir(&dir).unwrap();
            let security = inspect_file(&dir).unwrap().unwrap();
            assert!(
                security.is_dir && !security.is_reparse_point,
                "{security:?}"
            );

            let link = root.path().join("link");
            junction(&link, outside.path());
            let security = inspect_file(&link).unwrap().unwrap();
            assert!(security.is_reparse_point, "{security:?}");
            assert!(exposure(&security).contains(&Exposure::ReparsePoint));

            let first = root.path().join("a.toml");
            std::fs::write(&first, b"x").unwrap();
            let second = root.path().join("b.toml");
            std::fs::hard_link(&first, &second).unwrap();
            for name in [&first, &second] {
                let security = inspect_file(name).unwrap().unwrap();
                assert_eq!(security.links, 2, "{name:?}");
                assert!(exposure(&security).contains(&Exposure::HardLinked(2)));
            }

            // null DACL（全員にフルアクセス）。
            let null = root.path().join("null.toml");
            std::fs::write(&null, b"x").unwrap();
            set_dacl_sddl(&null, "D:NO_ACCESS_CONTROL");
            let security = inspect_file(&null).unwrap().unwrap();
            assert_eq!(security.dacl.aces, None, "{security:?}");
            assert!(exposure(&security).contains(&Exposure::NullDacl));

            // 条件付きの許可（XA、種類 9）は数え、拒否（D）は数えない（拒否の相手は Guests。自分を
            // 含む Users を拒否すると自分が読めなくなる）。
            let odd = root.path().join("odd.toml");
            std::fs::write(&odd, b"x").unwrap();
            set_dacl_sddl(
                &odd,
                &format!(
                    "D:P(D;;FA;;;BG)(A;;FA;;;SY)(A;;FA;;;{me})(XA;;0x1200a9;;;BU;(Member_of {{SID(BA)}}))",
                    me = me.as_str()
                ),
            );
            let security = inspect_file(&odd).unwrap().unwrap();
            assert!(security.dacl.protected);
            let found = exposure(&security);
            let conditional: Vec<_> = found
                .iter()
                .filter(|e| matches!(e, Exposure::AllowsOther { sid, .. } if sid == USERS_SID))
                .collect();
            assert_eq!(
                conditional.len(),
                1,
                "条件付きの許可だけが数えられる: {found:?}"
            );
            assert!(
                matches!(conditional[0], Exposure::AllowsOther { ace_type: 9, .. }),
                "{found:?}"
            );
            // 拒否（Guests）は理由にならない。
            let guests = "S-1-5-32-546";
            assert!(
                security
                    .dacl
                    .aces
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|a| a.ace_type == 1 && a.sid == guests),
                "{security:?}"
            );
            assert_eq!(allows_to(&found, guests), None, "{found:?}");
            set_dacl_sddl(&odd, &format!("D:(A;;FA;;;{})", me.as_str()));
            drop(security);
            std::fs::remove_dir(&link).unwrap();
        }

        /// 確かめ済みのフォルダのハンドルを基準に、子のファイルを辿らずに開き、そのハンドルの
        /// まま読む。リンク・フォルダは読まずに断り、持っている間は消せない。
        #[test]
        fn open_child_file_reads_through_the_handle_without_following() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let contents = b"hub_url = \"http://127.0.0.1:8722\"\napi_key = \"dummy\"\n";
            std::fs::write(root.path().join("conf.toml"), contents).unwrap();
            std::fs::write(outside.path().join("conf.toml"), b"outside").unwrap();
            std::fs::create_dir(root.path().join("sub")).unwrap();
            junction(&root.path().join("link"), outside.path());
            let parent = pin_dir(root.path()).unwrap().unwrap();

            let mut file = parent.open_child_file("conf.toml").unwrap().unwrap();
            assert_eq!(file.final_path(), parent.final_path().join("conf.toml"));
            assert_eq!(
                Some(file.security().clone()),
                inspect_file(&root.path().join("conf.toml")).unwrap(),
                "パスで読んだものと同じ"
            );
            assert_eq!(file.read_to_end(1024).unwrap(), contents);
            // 2 回目も先頭から。
            assert_eq!(file.read_to_end(contents.len()).unwrap(), contents);
            assert!(matches!(
                file.read_to_end(contents.len() - 1),
                Err(SafeRootError::Io {
                    op: crate::error::IoOp::ReadFile,
                    kind: std::io::ErrorKind::FileTooLarge,
                    ..
                })
            ));
            // 上限が `usize::MAX` でもあふれずに全部読む（#648 の P2）。
            assert_eq!(file.read_to_end(usize::MAX).unwrap(), contents);
            // 持っている間は消す・名前を変える・書き込みで開くことができない（共有は読み取り
            // だけ。#648 の P1）。読み取りで開くことはできる。
            assert!(std::fs::remove_file(root.path().join("conf.toml")).is_err());
            assert!(std::fs::rename(
                root.path().join("conf.toml"),
                root.path().join("moved.toml")
            )
            .is_err());
            let write_open = std::fs::OpenOptions::new()
                .write(true)
                .open(root.path().join("conf.toml"));
            assert_eq!(
                write_open
                    .as_ref()
                    .err()
                    .and_then(std::io::Error::raw_os_error),
                Some(32),
                "書き込みの開きは共有違反（ERROR_SHARING_VIOLATION）: {write_open:?}"
            );
            assert_eq!(
                std::fs::read(root.path().join("conf.toml")).unwrap(),
                contents
            );

            assert!(parent.open_child_file("missing.toml").unwrap().is_none());
            let mut sub = parent.open_child_file("sub").unwrap().unwrap();
            assert!(sub.security().is_dir);
            assert!(matches!(
                sub.read_to_end(1024),
                Err(SafeRootError::Io {
                    kind: std::io::ErrorKind::IsADirectory,
                    ..
                })
            ));
            let mut link = parent.open_child_file("link").unwrap().unwrap();
            assert!(link.security().is_reparse_point, "{link:?}");
            assert_eq!(
                link.read_to_end(1024),
                Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint))
            );
            for bad in [
                "",
                ".",
                "..",
                "a\\b",
                "a/b",
                "c:",
                "conf.toml:x",
                "x.",
                "x ",
                "nul",
                "con.txt",
                "a*",
                "a?",
            ] {
                assert!(parent.open_child_file(bad).is_err(), "{bad:?}");
                assert!(parent.create_child_file(bad, b"x").is_err(), "{bad:?}");
            }
            drop((file, sub, link, parent));
            std::fs::remove_dir(root.path().join("link")).unwrap();
            assert_eq!(
                std::fs::read(outside.path().join("conf.toml")).unwrap(),
                b"outside"
            );
        }

        /// 読めないファイルは、`io::ErrorKind::PermissionDenied` を残して断る（sink が案内を
        /// 出し分けるため）。ファイルはバックアップの意図を付けずに開くので、管理者でも同じ。
        #[test]
        fn open_child_file_reports_access_denied_as_permission_denied() {
            let root = tempfile::tempdir().unwrap();
            let locked = root.path().join("locked.toml");
            std::fs::write(&locked, b"x").unwrap();
            set_dacl_sddl(&locked, "D:P(A;;FA;;;SY)");
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let result = parent.open_child_file("locked.toml");
            assert!(
                matches!(
                    result,
                    Err(SafeRootError::Io {
                        op: crate::error::IoOp::OpenFile,
                        kind: std::io::ErrorKind::PermissionDenied,
                        ..
                    })
                ),
                "{result:?}"
            );
            drop(parent);
            set_dacl_sddl(&locked, &format!("D:(A;;FA;;;{})", me().as_str()));
        }

        /// #648 の P1: 別のハンドルが書き込みで開いたまま（共有はすべて許す）なら、確かめた後に
        /// 中身を書き換えられうるので、`open_child_file` は共有違反で開かない。`Io { op:
        /// OpenFile }` に OS のエラー番号（`ERROR_SHARING_VIOLATION` = 32）が残る。閉じれば開ける。
        #[test]
        fn open_child_file_refuses_while_another_handle_can_write() {
            use std::os::windows::fs::OpenOptionsExt;
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("conf.toml"), b"x").unwrap();
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .share_mode(0x7) // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
                .open(root.path().join("conf.toml"))
                .unwrap();
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let result = parent.open_child_file("conf.toml");
            assert!(
                matches!(
                    result,
                    Err(SafeRootError::Io {
                        op: crate::error::IoOp::OpenFile,
                        os_error: Some(32),
                        ..
                    })
                ),
                "{result:?}"
            );
            drop(writer);
            let mut file = parent.open_child_file("conf.toml").unwrap().unwrap();
            assert_eq!(file.read_to_end(16).unwrap(), b"x");
        }

        /// 置き場（フォルダ）の記述子をハンドルから読む。`service_only` で作った置き場は、
        /// 所有者のほかに理由が無い。
        #[test]
        fn pinned_dir_security_reads_a_service_only_place() {
            let root = tempfile::tempdir().unwrap();
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let sink = parent
                .create_child_dir("sink", Some(&DirAccess::service_only()))
                .unwrap()
                .unwrap();
            let security = sink.security().unwrap();
            assert!(security.is_dir && !security.is_reparse_point);
            assert!(security.dacl.protected, "{security:?}");
            let mut sids: Vec<_> = security
                .dacl
                .aces
                .as_ref()
                .unwrap()
                .iter()
                .map(|a| a.sid.as_str())
                .collect();
            sids.sort_unstable();
            assert_eq!(sids, [SYSTEM_SID, ADMINISTRATORS_SID]);
            let found = exposure(&security);
            if is_trusted_sid(&security.dacl.owner) {
                assert_eq!(found, vec![]);
            } else {
                // 一般の利用者・「作成者を所有者に」の PC の管理者が作ると、作った利用者が所有者。
                assert_eq!(
                    found,
                    vec![Exposure::ForeignOwner(security.dacl.owner.clone())]
                );
            }
            drop((sink, parent));
            set_dacl_sddl(
                &root.path().join("sink"),
                &format!("D:(A;OICI;FA;;;{})", me().as_str()),
            );
        }

        /// 確かめ済みのフォルダのハンドルを基準に、保護された DACL と所有者 Administrators を
        /// **作るのと同時に**付けてファイルを作る。所有者を Administrators にするので昇格が要り、
        /// 昇格していなければ何も作らずに失敗する（手元）。昇格していれば（CI）作った
        /// ファイルを読み戻して確かめ、保護されていなかったときは作ったファイルを消す。
        #[test]
        fn create_child_file_attaches_the_protected_dacl_at_creation() {
            let root = tempfile::tempdir().unwrap();
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let contents = b"# example\nhub_url = \"http://127.0.0.1:8722\"\n";
            let name = "banto-hub-sink.toml.example";

            if !is_elevated_admin() {
                let result = parent.create_child_file(name, contents);
                assert!(
                    matches!(
                        result,
                        Err(SafeRootError::Io {
                            op: crate::error::IoOp::CreateFile,
                            ..
                        })
                    ),
                    "{result:?}"
                );
                assert!(!root.path().join(name).exists(), "何も作らない");
                eprintln!("skip: 所有者を Administrators にする作成は管理者でだけ確かめる（CI）");
                return;
            }

            // 実際の形: 保護された置き場の中に作る。
            let sink = parent
                .create_child_dir("sink", Some(&DirAccess::service_only()))
                .unwrap()
                .unwrap();
            for place in [&parent, &sink] {
                let mut file = place.create_child_file(name, contents).unwrap().unwrap();
                assert_eq!(file.read_to_end(1024).unwrap(), contents);
                assert_eq!(file.final_path(), place.final_path().join(name));
                let security = file.security();
                assert!(!security.is_dir && !security.is_reparse_point);
                assert_eq!(security.links, 1);
                assert!(security.dacl.protected, "継承しない: {security:?}");
                assert_eq!(security.dacl.owner, ADMINISTRATORS_SID, "{security:?}");
                check_file_dacl(&security.dacl, &DirAccess::service_only());
                assert_eq!(exposure(security), vec![]);
                assert_eq!(
                    inspect_file(&place.final_path().join(name))
                        .unwrap()
                        .as_ref(),
                    Some(security),
                );
                drop(file);
                // 既にあれば作らず、中身も変えない。
                assert!(place.create_child_file(name, b"other").unwrap().is_none());
                let mut again = place.open_child_file(name).unwrap().unwrap();
                assert_eq!(again.read_to_end(1024).unwrap(), contents);
            }
            let note = sink.security().unwrap();
            eprintln!(
                "note: create_child_dir で作った置き場の所有者: {}（exposure: {:?}）",
                note.dacl.owner,
                exposure(&note)
            );

            // 読み戻しで保護されていなかったときは、作ったハンドルで消してから失敗を返す。
            imp::FORCE_EXPOSED.with(|force| force.set(true));
            let result = parent.create_child_file("rolled-back.toml", contents);
            imp::FORCE_EXPOSED.with(|force| force.set(false));
            assert!(
                matches!(result, Err(SafeRootError::Exposed(ref found)) if found == &[Exposure::NullDacl]),
                "{result:?}"
            );
            assert!(
                !root.path().join("rolled-back.toml").exists(),
                "作ったファイルが消えていない"
            );
        }

        /// 記述子を書く・消す権利で開く（#630 PR-2）。中身は読めない。持っている間はほかの
        /// 誰も消す・名前を変えることができない。ハンドルで消すと、閉じたときに消える。フォルダは
        /// 消さずに断る。
        #[test]
        fn open_child_file_for_update_holds_and_deletes_through_the_handle() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("a.example"), b"old").unwrap();
            std::fs::write(outside.path().join("a.example"), b"outside").unwrap();
            std::fs::create_dir(root.path().join("sub")).unwrap();
            junction(&root.path().join("link"), outside.path());
            let parent = pin_dir(root.path()).unwrap().unwrap();

            assert!(parent
                .open_child_file_for_update("missing")
                .unwrap()
                .is_none());
            let mut file = parent
                .open_child_file_for_update("a.example")
                .unwrap()
                .unwrap();
            assert_eq!(file.final_path(), parent.final_path().join("a.example"));
            assert!(!file.security().is_dir && !file.security().is_reparse_point);
            // 中身の権利は無い。
            assert!(file.read_to_end(1024).is_err());
            // 持っている間は消す・名前を変えることができない。
            assert!(std::fs::remove_file(root.path().join("a.example")).is_err());
            assert!(std::fs::rename(
                root.path().join("a.example"),
                root.path().join("moved.example")
            )
            .is_err());
            // 持っている間は、ほかの誰も書き込みに開けない（共有は読み取りだけ）。
            assert!(std::fs::OpenOptions::new()
                .write(true)
                .open(root.path().join("a.example"))
                .is_err());
            file.delete().unwrap();
            assert!(!root.path().join("a.example").exists(), "閉じたら消える");

            // 既に書き込みに開かれていれば、開くこと自体が共有違反で失敗する。
            std::fs::write(root.path().join("busy.toml"), b"x").unwrap();
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(root.path().join("busy.toml"))
                .unwrap();
            let busy = parent.open_child_file_for_update("busy.toml");
            assert!(
                matches!(
                    busy,
                    Err(SafeRootError::Io {
                        op: crate::error::IoOp::OpenFile,
                        os_error: Some(32),
                        ..
                    })
                ),
                "{busy:?}"
            );
            drop(writer);
            assert!(parent
                .open_child_file_for_update("busy.toml")
                .unwrap()
                .is_some());

            let sub = parent.open_child_file_for_update("sub").unwrap().unwrap();
            assert!(matches!(
                sub.delete(),
                Err(SafeRootError::Io {
                    kind: std::io::ErrorKind::IsADirectory,
                    ..
                })
            ));
            assert!(root.path().join("sub").is_dir(), "フォルダは消さない");
            let link = parent.open_child_file_for_update("link").unwrap().unwrap();
            assert!(link.security().is_reparse_point);
            assert_eq!(
                link.reset_acl(),
                Err(SafeRootError::AclMismatch(AclMismatch::ReparsePoint))
            );
            for bad in ["", "..", r"a\b", "c:", "x.", "nul"] {
                assert!(parent.open_child_file_for_update(bad).is_err(), "{bad:?}");
            }
            drop((link, parent));
            std::fs::remove_dir(root.path().join("link")).unwrap();
            assert_eq!(
                std::fs::read(outside.path().join("a.example")).unwrap(),
                b"outside"
            );
        }

        /// ハードリンクには書かずに断る（記述子を共有するので、書くと木の外の名前のアクセスも
        /// 変わる - #624）。外の名前の DACL は前後で同じ。管理者でなくても走る（書く前に断る）。
        #[test]
        fn reset_acl_refuses_a_hardlinked_file_without_writing() {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("victim.toml");
            std::fs::write(&target, b"x").unwrap();
            std::fs::hard_link(&target, root.path().join("banto-hub-sink.toml")).unwrap();
            let before = read_dir_dacl(&target).unwrap();
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let file = parent
                .open_child_file_for_update("banto-hub-sink.toml")
                .unwrap()
                .unwrap();
            assert_eq!(file.security().links, 2);
            assert_eq!(
                file.reset_acl(),
                Err(SafeRootError::Exposed(vec![Exposure::HardLinked(2)]))
            );
            assert_eq!(
                read_dir_dacl(&target).unwrap(),
                before,
                "外の名前は変わらない"
            );
        }

        /// 普通のファイルに、保護された DACL と所有者 Administrators をハンドルで書く。所有者を
        /// 移すので昇格が要り、昇格していなければ 1307 で何も変えずに失敗する（手元）。昇格して
        /// いれば（CI）書いた形を読み戻し、中身が変わらないことも確かめる。
        #[test]
        fn reset_acl_writes_the_protected_dacl_and_owner_through_the_handle() {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("banto-hub-sink.toml");
            let contents = b"hub_url = \"http://127.0.0.1:8722\"\n";
            std::fs::write(&path, contents).unwrap();
            let before = read_dir_dacl(&path).unwrap();
            assert!(!before.protected, "継承の DACL から始める");
            let parent = pin_dir(root.path()).unwrap().unwrap();
            let file = parent
                .open_child_file_for_update("banto-hub-sink.toml")
                .unwrap()
                .unwrap();
            if !is_elevated_admin() {
                assert_eq!(
                    file.reset_acl(),
                    Err(SafeRootError::AclApi {
                        call: "SetKernelObjectSecurity",
                        os_error: 1307,
                    })
                );
                assert_eq!(read_dir_dacl(&path).unwrap(), before, "何も変えない");
                eprintln!(
                    "skip: 所有者を Administrators に移す書き込みは管理者でだけ確かめる（CI）"
                );
                return;
            }
            let after = file.reset_acl().unwrap();
            assert!(after.dacl.protected, "{after:?}");
            assert_eq!(after.dacl.owner, ADMINISTRATORS_SID);
            check_file_dacl(&after.dacl, &DirAccess::service_only());
            assert_eq!(exposure(&after), vec![]);
            assert_eq!(inspect_file(&path).unwrap(), Some(after));
            drop((file, parent));
            assert_eq!(std::fs::read(&path).unwrap(), contents, "中身は変えない");
        }
    }
}
