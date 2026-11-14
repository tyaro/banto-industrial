//! 利用者ごとの置き場（#1121 PR1a。docs/hub-desktop-tls-design.md §4.1・§4.4.1）: 差し替えの口
//! [`UserTlsPlace`]、既定の実装 [`DefaultUserTlsPlace`]、置き場の確かめ [`open_user_place`]、
//! ChronoGazer 向けの読み取りの入口 [`load_pinned_user_cert`]。書き込み・作成はしない
//! （crate doc「利用者ごとの置き場」）。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    exposure, Exposure, FileSecurity, ACCESS_ALLOWED_ACE_TYPE, READ_EXECUTE_ACCESS_MASK,
};
use banto_safe_root::guard::{self, DriveKind, RealPathRefusal};

use crate::place::open_file_in;
use crate::text::{exposure_text, real_path_reason};
use crate::{
    parse_single_certificate, HubCertPin, TlsDir, TlsFile, TlsTrustOps, CERT_FILE, KEY_FILE,
    MAX_PEM_BYTES, PRODUCT_DIR, TLS_DIR,
};

/// 既定の失効の記録 A のフォルダ名（`%LOCALAPPDATA%\BantoHub\` の下。`tls\` の外）。
pub const USER_REVOCATIONS_A_DIR: &str = "revocations";

/// 既定の失効の記録 B のフォルダ名（`%LOCALAPPDATA%` の直下。A と親が違う）。
pub const USER_REVOCATIONS_B_DIR: &str = "BantoHubRevocations";

/// 既定の、失効の記録のロックファイルの名前（`%LOCALAPPDATA%` の直下。A・B のどちらの
/// フォルダにも入れない）。
pub const USER_REVOCATIONS_LOCK_FILE: &str = "BantoHubRevocations.lock";

/// 文言の中の、起点（既定は `%LOCALAPPDATA%`）の呼び名。
pub const USER_ANCHOR_LABEL: &str = "利用者の置き場の起点";

/// `GENERIC_READ | GENERIC_EXECUTE`（読み取りだけの汎用の権利）。
const GENERIC_READ_EXECUTE: u32 = 0x8000_0000 | 0x2000_0000;

/// 信頼する相手以外に許してよい権利（読み取り・実行・列挙だけ）。これに収まらない bit
/// （書き込み・追加・削除・子の削除・`WRITE_DAC`・`WRITE_OWNER`・`GENERIC_WRITE`・`GENERIC_ALL`・
/// `MAXIMUM_ALLOWED` など）が 1 つでもあれば断る。
pub const USER_PLACE_OTHERS_READ_MASK: u32 = READ_EXECUTE_ACCESS_MASK | GENERIC_READ_EXECUTE;

/// 利用者ごとの 4 つの置き場（§4.1「trait が返すのは 4 つの置き場」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserPlaceKind {
    /// 証明書と鍵の `tls\`（フォルダ）。
    Tls,
    /// 失効の記録 A（フォルダ）。
    RevocationsA,
    /// 失効の記録 B（フォルダ。A と別のフォルダ）。
    RevocationsB,
    /// 失効の記録のロックファイル（ファイル。A・B のどちらのフォルダにも入れない）。
    RevocationsLock,
}

impl UserPlaceKind {
    /// 4 つすべて。
    pub const ALL: [UserPlaceKind; 4] = [
        UserPlaceKind::Tls,
        UserPlaceKind::RevocationsA,
        UserPlaceKind::RevocationsB,
        UserPlaceKind::RevocationsLock,
    ];

    /// 文言の中の呼び名。
    pub fn label(self) -> &'static str {
        match self {
            UserPlaceKind::Tls => "利用者の TLS の置き場",
            UserPlaceKind::RevocationsA => "失効の記録 A の置き場",
            UserPlaceKind::RevocationsB => "失効の記録 B の置き場",
            UserPlaceKind::RevocationsLock => "失効の記録のロックファイルの置き場",
        }
    }
}

/// 確かめの方針（§4.1「確かめること」のうち、置き場ごとに変わりうるもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPlacePolicy {
    /// 起点が、今の利用者の `%LOCALAPPDATA%`（既知のフォルダ `FOLDERID_LocalAppData`）の値と
    /// 一致することを求める。既定は真。別の起点（MSIX の publisher cache folder など）を返す
    /// 実装は偽にする。
    pub anchor_is_local_app_data: bool,
    /// 起点の本当のパスがローカルの固定ドライブにあることを求める（ネットワーク・取り外せる
    /// ドライブへの転送を断る）。既定は真。
    pub require_local_fixed_drive: bool,
}

/// 置き場 1 つの、パス・期待する所有者・確かめの方針（[`UserTlsPlace::spec`] が返す）。
///
/// パスは「起点」と、その下の名前の並びで表す。[`open_user_place`] は起点をパスで辿らずに
/// 開き、その下は**親のハンドル基準**で 1 要素ずつ開いて、起点から対象までのすべてを確かめる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPlaceSpec {
    /// 確かめの起点（絶対パス。既定は今の利用者の `%LOCALAPPDATA%`）。起点より上は確かめない。
    pub anchor: PathBuf,
    /// 起点の下のフォルダ（起点に近い順。名前 1 要素ずつ）。置き場がフォルダなら、最後が
    /// その置き場。
    pub dirs: Vec<String>,
    /// 置き場がファイル（ロックファイル）のときの名前。`dirs` の最後（空なら起点）の直下。
    pub file: Option<String>,
    /// 期待する所有者（SID の文字列）。SYSTEM・Administrators（と、それより強い
    /// TrustedInstaller。`banto_safe_root::acl::exposure` と同じ）に**加えて**信頼する相手で、
    /// 既定は今のプロセスのトークンの利用者だけ。所有者として認め、書き込み・削除・ACL の
    /// 変更の許可を持っていてよい。
    pub owners: Vec<String>,
    /// 確かめの方針。
    pub policy: UserPlacePolicy,
}

impl UserPlaceSpec {
    /// 置き場を入れるフォルダの期待のパス（置き場がフォルダなら、その置き場そのもの）。
    pub fn dir_path(&self) -> PathBuf {
        let mut path = self.anchor.clone();
        path.extend(&self.dirs);
        path
    }

    /// 置き場の期待のパス（フォルダか、ロックファイル）。
    pub fn path(&self) -> PathBuf {
        match &self.file {
            Some(file) => self.dir_path().join(file),
            None => self.dir_path(),
        }
    }
}

/// 利用者ごとの置き場の差し替えの口（§4.1）。4 つの置き場それぞれについて、パス・期待する
/// 所有者・確かめの方針を返す。パスを決め打ちするのは既定の実装 [`DefaultUserTlsPlace`]
/// だけで、フォーク（Store 版）はこの trait を差し替える。
///
/// 差し替える実装が守ること（[`check_place_layout`] が形を確かめる）: 同じ利用者の Hub と
/// ChronoGazer が 4 つとも**同じ実体**を見ること。A と B は別のフォルダで、ロックファイルは
/// A・B のどちらのフォルダにも入れないこと。
pub trait UserTlsPlace {
    /// `kind` の置き場。
    fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec;
}

/// 既定の置き場（今の利用者の `%LOCALAPPDATA%` の下。パスを決め打ちする唯一の箇所）:
///
/// - `tls\`: `%LOCALAPPDATA%\BantoHub\tls\`
/// - 失効の記録 A: `%LOCALAPPDATA%\BantoHub\revocations\`
/// - 失効の記録 B: `%LOCALAPPDATA%\BantoHubRevocations\`
/// - ロックファイル: `%LOCALAPPDATA%\BantoHubRevocations.lock`
///
/// 期待する所有者は今のプロセスのトークンの利用者（と SYSTEM・Administrators）、方針は
/// 「起点が既知のフォルダの値と一致」「ローカルの固定ドライブ」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultUserTlsPlace {
    local_app_data: PathBuf,
    user_sid: String,
}

impl DefaultUserTlsPlace {
    /// 今の利用者の `%LOCALAPPDATA%`（環境変数ではなく既知のフォルダ）とトークンの利用者の
    /// SID を `ops` から読んで作る。
    pub fn resolve<O: UserTlsOps>(ops: &O) -> Result<Self, UserPlaceProblem> {
        let local_app_data = ops.local_app_data().map_err(|error| {
            problem(
                format!("%LOCALAPPDATA% の場所（既知のフォルダ）を取れませんでした: {error}"),
                UserRemedy::Retry,
            )
        })?;
        let user_sid = ops.current_user_sid().map_err(|error| {
            problem(
                format!("今の利用者の SID を取れませんでした: {error}"),
                UserRemedy::Retry,
            )
        })?;
        Ok(Self::new(local_app_data, user_sid))
    }

    /// 既に読んだ値から作る（`local_app_data` は既知のフォルダの値、`user_sid` は今のプロセスの
    /// トークンの利用者の SID）。
    pub fn new(local_app_data: PathBuf, user_sid: String) -> Self {
        Self {
            local_app_data,
            user_sid,
        }
    }
}

impl UserTlsPlace for DefaultUserTlsPlace {
    fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec {
        let names = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
        let (dirs, file): (Vec<String>, Option<String>) = match kind {
            UserPlaceKind::Tls => (names(&[PRODUCT_DIR, TLS_DIR]), None),
            UserPlaceKind::RevocationsA => (names(&[PRODUCT_DIR, USER_REVOCATIONS_A_DIR]), None),
            UserPlaceKind::RevocationsB => (names(&[USER_REVOCATIONS_B_DIR]), None),
            UserPlaceKind::RevocationsLock => {
                (Vec::new(), Some(USER_REVOCATIONS_LOCK_FILE.to_string()))
            }
        };
        UserPlaceSpec {
            anchor: self.local_app_data.clone(),
            dirs,
            file,
            owners: vec![self.user_sid.clone()],
            policy: UserPlacePolicy {
                anchor_is_local_app_data: true,
                require_local_fixed_drive: true,
            },
        }
    }
}

/// 利用者ごとの置き場の確かめに要る、読み取りだけの操作（[`TlsTrustOps`] に足すもの）。
/// Windows の実装は `crate::win::WindowsTlsTrustOps`、テストは偽物。
pub trait UserTlsOps: TlsTrustOps {
    /// 今の利用者の `%LOCALAPPDATA%`（既知のフォルダ `FOLDERID_LocalAppData`。環境変数は
    /// 読まない）。
    fn local_app_data(&self) -> Result<PathBuf, String>;
    /// 今のプロセスのトークンの利用者の SID（文字列）。
    fn current_user_sid(&self) -> Result<String, String>;
    /// 本当のパス `real` が載っているドライブの種類。
    fn drive_kind(&self, real: &Path) -> Result<DriveKind, String>;
}

/// 対処の種類（文言は呼ぶ側が決める。サービスの置き場の `Remedy` と同じ考え）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRemedy {
    /// 一時的かもしれない（開けない・読めない・場所を取れない）。原因を取り除いてやり直す。
    Retry,
    /// 起点（このパス。既定は `%LOCALAPPDATA%`）の場所・所有者・ACL が信じられない。利用者か
    /// 管理者が直す。
    FixAnchor(PathBuf),
    /// このフォルダの所有者・ACL・種類が信じられない。中身を確かめてフォルダごと消し、
    /// 作り直す（§4.2「作り直しの前にも置き場を確かめる」の手順）。
    RecreateDir(PathBuf),
    /// 鍵・証明書が壊れている・露出している。作り直す（§4.2 の「作り直しの口」）。
    Renew,
}

/// 「あるが通らない」の理由（表示用の日本語の文。鍵の中身は含めない）と対処の種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPlaceProblem {
    /// 何が合わなかったか。
    pub reason: String,
    /// 対処の種類。
    pub remedy: UserRemedy,
}

fn problem(reason: String, remedy: UserRemedy) -> UserPlaceProblem {
    UserPlaceProblem { reason, remedy }
}

/// [`Exposure`] の文言（利用者の置き場用。信頼する相手に利用者本人が入るので、所有者と許可の
/// 文言だけ言い換える）。
fn user_exposure_text(found: &Exposure) -> String {
    match found {
        Exposure::ForeignOwner(owner) => {
            format!("所有者（{owner}）が利用者本人・SYSTEM・Administrators ではありません")
        }
        Exposure::AllowsOther { sid, mask, .. } => format!(
            "利用者本人・SYSTEM・Administrators 以外（{sid}）にアクセスを許しています（アクセス {mask:#x}）"
        ),
        other => exposure_text(other),
    }
}

fn user_reasons_text(found: &[Exposure]) -> String {
    found
        .iter()
        .map(user_exposure_text)
        .collect::<Vec<_>>()
        .join("・")
}

/// SID の文字列が AppContainer の SID か: パッケージ SID（`S-1-15-2-…`。ALL APPLICATION
/// PACKAGES `S-1-15-2-1`・ALL RESTRICTED APPLICATION PACKAGES `S-1-15-2-2` を含む）か、
/// capability SID（`S-1-15-3-…`）。
///
/// 利用者の置き場の確かめ（[`user_place_problems`]）は、これらへの普通の許可を問題にしない
/// （2026-11-14 オーナー決定、docs/hub-desktop-tls-design.md §4.1）。AppContainer のトークンの
/// アクセスは、「ふつうの確かめ（利用者・グループの SID）」と「パッケージ・capability の SID の
/// 確かめ」の**両方**を通った分の積になり、通常のトークンはこれらの SID を持たない。そのため、
/// この種の ACE だけでは別のアカウントに権限は付かず、付くのは同じ利用者（信頼の範囲、§3）の
/// AppContainer のプロセスだけ。
///
/// 判定は文字列の形で厳しく取る: 接頭辞は大文字の `S-1-15-2-`・`S-1-15-3-` ちょうどで、続きは
/// 1 つ以上の「数字だけの要素」を `-` で繋いだもの。`S-1-15-21-…`（接頭辞の偽の一致）・
/// `S-1-15-2`・`S-1-15-2-`（続きが無い。実在しない形）・小文字の `s-1-15-2-1`・数字以外の
/// 混じったものは当てはめない（今までどおり、ほかの相手として確かめる）。
pub fn is_app_container_sid(sid: &str) -> bool {
    ["S-1-15-2-", "S-1-15-3-"].iter().any(|prefix| {
        sid.strip_prefix(prefix).is_some_and(|rest| {
            rest.split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}

/// 置き場のフォルダ・ファイルの記述子が、信頼する相手（`owners` と SYSTEM・Administrators）
/// 以外に開いている理由をすべて返す（純関数。空なら通す）。`exposure` から次を除いたもの:
///
/// - 所有者が `owners` のどれか（`ForeignOwner`）。
/// - `owners` のどれかへの許可（`AllowsOther`）。
/// - `others_may_read` が真のとき、信頼する相手以外への普通の許可（`ACCESS_ALLOWED_ACE_TYPE`）
///   で、マスクが [`USER_PLACE_OTHERS_READ_MASK`]（読み取りだけ）に収まるもの。継承のフラグは
///   問わない（inherit-only でも、書き込みを含めば断る。`%LOCALAPPDATA%` の既定の ACL には
///   信頼する相手以外への ACE が無いので、ルートの `root_dacl_problems` のような緩めは要らない）。
/// - AppContainer の SID（[`is_app_container_sid`]: パッケージ SID `S-1-15-2-…`・capability
///   SID `S-1-15-3-…`）への普通の許可（`ACCESS_ALLOWED_ACE_TYPE`）。マスクも
///   `others_may_read` も問わない（鍵でも通す）。2026-11-14 オーナー決定。理由は
///   [`is_app_container_sid`]。条件付き・オブジェクトの許可は、相手が AppContainer の SID でも
///   今までどおり残す。
///
/// 残るのは、reparse point・ハードリンク・見知らぬ所有者・null DACL・種類の分からない ACE・
/// SID の読めない ACE・信頼する相手以外への条件付き／オブジェクトの許可・信頼する相手でも
/// AppContainer の SID でもない相手への書き込み・削除・`WRITE_DAC`・`WRITE_OWNER` を含む許可
/// （`others_may_read` が偽なら、読み取りを含むすべての許可）。
///
/// フォルダ・証明書・ロックファイル・記録は `others_may_read = true`、鍵は `false`（§4.1
/// 「鍵のファイルは、その 3 者以外が読めない」）。
pub fn user_place_problems(
    security: &FileSecurity,
    owners: &[String],
    others_may_read: bool,
) -> Vec<Exposure> {
    let trusted = |sid: &str| owners.iter().any(|owner| owner == sid);
    exposure(security)
        .into_iter()
        .filter(|found| match found {
            Exposure::ForeignOwner(sid) => !trusted(sid),
            Exposure::AllowsOther { sid, .. } if trusted(sid) => false,
            Exposure::AllowsOther { ace_type, sid, .. }
                if *ace_type == ACCESS_ALLOWED_ACE_TYPE && is_app_container_sid(sid) =>
            {
                false
            }
            Exposure::AllowsOther { ace_type, mask, .. }
                if others_may_read && *ace_type == ACCESS_ALLOWED_ACE_TYPE =>
            {
                mask & !USER_PLACE_OTHERS_READ_MASK != 0
            }
            _ => true,
        })
        .collect()
}

/// [`UserPlaceSpec`] の形の確かめ（純関数）: 起点が絶対パスでネットワークの形（`\\`）でない・
/// 名前がどれも普通の 1 要素（`guard::is_plain_child_name`）・起点そのものを置き場にしていない。
pub fn check_spec(spec: &UserPlaceSpec) -> Result<(), String> {
    if !spec.anchor.is_absolute() {
        return Err(format!(
            "{USER_ANCHOR_LABEL}（{}）が絶対パスではありません",
            spec.anchor.display()
        ));
    }
    if spec.dirs.is_empty() && spec.file.is_none() {
        return Err(format!(
            "{USER_ANCHOR_LABEL}（{}）そのものは置き場にできません",
            spec.anchor.display()
        ));
    }
    for name in spec.dirs.iter().chain(spec.file.iter()) {
        if !guard::is_plain_child_name(name) {
            return Err(format!(
                "置き場の名前（{name}）が普通の 1 要素ではありません"
            ));
        }
    }
    Ok(())
}

/// [`UserTlsPlace`] の実装が返す 4 つの置き場の並びの確かめ（純関数。§4.1「差し替える実装が
/// 守ること」、§4.4.1）: どれも [`check_spec`] に通る・`tls\` と A と B はフォルダ、ロックは
/// ファイル・`tls\`・A・B は互いに別で、どれも他の中に無い・ロックファイルは A・B・`tls\` の
/// どのフォルダの中にも無い。実体が同じか（MSIX の振り向け）はここでは分からない。
pub fn check_place_layout<P: UserTlsPlace + ?Sized>(place: &P) -> Result<(), String> {
    let spec_of = |kind: UserPlaceKind| -> Result<UserPlaceSpec, String> {
        let spec = place.spec(kind);
        check_spec(&spec).map_err(|error| format!("{}: {error}", kind.label()))?;
        let is_file = spec.file.is_some();
        if is_file != (kind == UserPlaceKind::RevocationsLock) {
            return Err(format!(
                "{}（{}）の種類（フォルダ・ファイル）が違います",
                kind.label(),
                spec.path().display()
            ));
        }
        Ok(spec)
    };
    let dirs = [
        (UserPlaceKind::Tls, spec_of(UserPlaceKind::Tls)?),
        (
            UserPlaceKind::RevocationsA,
            spec_of(UserPlaceKind::RevocationsA)?,
        ),
        (
            UserPlaceKind::RevocationsB,
            spec_of(UserPlaceKind::RevocationsB)?,
        ),
    ];
    let lock = spec_of(UserPlaceKind::RevocationsLock)?;
    for (index, (kind, spec)) in dirs.iter().enumerate() {
        for (other_kind, other) in &dirs[index + 1..] {
            let (a, b) = (spec.path(), other.path());
            if guard::is_same_or_inside(&a, &b) || guard::is_same_or_inside(&b, &a) {
                return Err(format!(
                    "{}（{}）と{}（{}）が同じか、片方がもう片方の中にあります",
                    kind.label(),
                    a.display(),
                    other_kind.label(),
                    b.display()
                ));
            }
        }
        if guard::is_same_or_inside(&lock.path(), &spec.path()) {
            return Err(format!(
                "{}（{}）が{}（{}）の中にあります",
                UserPlaceKind::RevocationsLock.label(),
                lock.path().display(),
                kind.label(),
                spec.path().display()
            ));
        }
    }
    Ok(())
}

/// 確かめ終えた置き場（起点から置き場のフォルダまでを、辿らずに握ったまま持つ）。置き場が
/// ロックファイルのときは、握っているのはファイルを入れるフォルダで、ファイルそのものは
/// [`open_user_file`]（読み取り）か、呼ぶ側の排他の open（PR1b）で開いて
/// [`user_place_problems`] で確かめる。
pub struct CheckedUserPlace<D> {
    /// 起点から、置き場のフォルダの親まで（握っている間は名前の変更・削除を止める）。
    _ancestors: Vec<D>,
    dir: D,
    dir_path: PathBuf,
    dir_real: PathBuf,
    file: Option<String>,
    owners: Vec<String>,
}

impl<D> CheckedUserPlace<D> {
    /// 確かめたフォルダのハンドル（置き場がフォルダならその置き場、ファイルならそれを入れる
    /// フォルダ）。
    pub fn dir(&self) -> &D {
        &self.dir
    }

    /// そのフォルダの期待のパス（文言用）。
    pub fn dir_path(&self) -> &Path {
        &self.dir_path
    }

    /// そのフォルダの本当のパス。
    pub fn dir_real(&self) -> &Path {
        &self.dir_real
    }

    /// 置き場がファイル（ロックファイル）のときの名前。
    pub fn file_name(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// 期待する所有者（[`UserPlaceSpec::owners`]）。中のファイルの確かめに使う。
    pub fn owners(&self) -> &[String] {
        &self.owners
    }
}

/// 置き場のフォルダがまだ無い（途中のフォルダが無い場合を含む）。確かめ済みのいちばん深い
/// フォルダを握ったまま返すので、作る側（Hub の core、PR2）はそのハンドル基準で残りを作れる。
pub struct MissingUserPlace<D> {
    _ancestors: Vec<D>,
    deepest: D,
    deepest_path: PathBuf,
    deepest_real: PathBuf,
    missing: Vec<String>,
    owners: Vec<String>,
}

impl<D> MissingUserPlace<D> {
    /// 確かめ済みのいちばん深いフォルダのハンドル。
    pub fn deepest(&self) -> &D {
        &self.deepest
    }

    /// そのフォルダの期待のパス。
    pub fn deepest_path(&self) -> &Path {
        &self.deepest_path
    }

    /// そのフォルダの本当のパス。
    pub fn deepest_real(&self) -> &Path {
        &self.deepest_real
    }

    /// まだ無いフォルダの名前（[`MissingUserPlace::deepest`] に近い順。最初の 1 つは確かに無く、
    /// その先は見ていない）。
    pub fn missing(&self) -> &[String] {
        &self.missing
    }

    /// 期待する所有者（[`UserPlaceSpec::owners`]）。
    pub fn owners(&self) -> &[String] {
        &self.owners
    }
}

/// [`open_user_place`] の結果のうち、断らなかったもの（「無い」と「ある」）。
pub enum UserPlaceState<D> {
    /// ある（確かめに通った）。
    Present(CheckedUserPlace<D>),
    /// 無い（起点から途中までは確かめに通った）。
    Absent(MissingUserPlace<D>),
}

/// 開いたフォルダ 1 つの確かめ（種類・本当の場所・所有者と DACL）。通れば本当のパス。
fn check_dir<D: TlsDir>(
    label: &str,
    shown: &Path,
    expected_real: &Path,
    dir: &D,
    owners: &[String],
    remedy: &UserRemedy,
) -> Result<PathBuf, UserPlaceProblem> {
    let security = dir.security().map_err(|error| {
        problem(
            format!(
                "{label}（{}）の ACL を読めませんでした: {error}",
                shown.display()
            ),
            UserRemedy::Retry,
        )
    })?;
    let kind = if security.is_reparse_point {
        Some("ジャンクションかシンボリックリンク（reparse point）です")
    } else if !security.is_dir {
        Some("フォルダではありません")
    } else {
        None
    };
    if let Some(kind) = kind {
        return Err(problem(
            format!("{label}（{}）は{kind}", shown.display()),
            remedy.clone(),
        ));
    }
    let real = dir.final_path();
    if !guard::paths_match(&real, expected_real) {
        return Err(problem(
            real_path_reason(label, shown, &real, RealPathRefusal::Mismatch),
            remedy.clone(),
        ));
    }
    let open = user_place_problems(&security, owners, true);
    if !open.is_empty() {
        return Err(problem(
            format!(
                "{label}（{}）は保護されていません（{}）。ほかの利用者が中身を書き換えられる恐れがあります",
                shown.display(),
                user_reasons_text(&open)
            ),
            remedy.clone(),
        ));
    }
    Ok(real)
}

/// 置き場 `kind` を、起点から順に確かめて開く（§4.1「確かめること」。書き込み・作成はしない）:
///
/// 1. [`check_spec`]。方針が求めれば、起点が今の利用者の `%LOCALAPPDATA%`（既知のフォルダの値）
///    と一致すること。
/// 2. 起点を辿らずに開き、reparse point でないフォルダ・本当のパスが起点のパスと同じ場所
///    （途中のジャンクション・フォルダの転送で別の場所を開かされていない）・所有者と DACL
///    （[`user_place_problems`]）を確かめる。方針が求めれば、本当のパスがローカルの固定ドライブ
///    にあること。
/// 3. 起点の下のフォルダを、親のハンドル基準で 1 要素ずつ辿らずに開き、同じ確かめをする。
///    無ければそこで [`UserPlaceState::Absent`]（「無い」。起点そのものが無いのは断る）。
///
/// 断るのは「あるが通らない」（[`UserPlaceProblem`]）。置き場がロックファイルのときは、
/// それを入れるフォルダまでを確かめる（ファイルの有無は見ない）。
pub fn open_user_place<O: UserTlsOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
    kind: UserPlaceKind,
) -> Result<UserPlaceState<O::Dir>, UserPlaceProblem> {
    let spec = place.spec(kind);
    let anchor_path = spec.anchor.clone();
    let fix_anchor = UserRemedy::FixAnchor(anchor_path.clone());

    // --- 1. 純粋な確かめ ------------------------------------------------------------
    check_spec(&spec).map_err(|error| {
        problem(
            format!("{}の指定が使えません: {error}", kind.label()),
            fix_anchor.clone(),
        )
    })?;
    if spec.policy.anchor_is_local_app_data {
        let known = ops.local_app_data().map_err(|error| {
            problem(
                format!("%LOCALAPPDATA% の場所（既知のフォルダ）を取れませんでした: {error}"),
                UserRemedy::Retry,
            )
        })?;
        if !guard::paths_match(&known, &anchor_path) {
            return Err(problem(
                format!(
                    "{USER_ANCHOR_LABEL}（{}）が、今の利用者の %LOCALAPPDATA%（既知のフォルダの値 {}）と一致しません",
                    anchor_path.display(),
                    known.display()
                ),
                fix_anchor,
            ));
        }
    }

    // --- 2. 起点 --------------------------------------------------------------------
    let anchor = match ops.pin_dir(&anchor_path) {
        Ok(Some(anchor)) => anchor,
        Ok(None) => {
            return Err(problem(
                format!(
                    "{USER_ANCHOR_LABEL}（{}）がありません",
                    anchor_path.display()
                ),
                fix_anchor,
            ))
        }
        Err(error) => {
            return Err(problem(
                format!(
                    "{USER_ANCHOR_LABEL}（{}）を開けませんでした: {error}",
                    anchor_path.display()
                ),
                UserRemedy::Retry,
            ))
        }
    };
    let anchor_real = check_dir(
        USER_ANCHOR_LABEL,
        &anchor_path,
        &anchor_path,
        &anchor,
        &spec.owners,
        &fix_anchor,
    )?;
    if spec.policy.require_local_fixed_drive {
        let drive = ops.drive_kind(&anchor_real).map_err(|error| {
            problem(
                format!(
                    "{USER_ANCHOR_LABEL}（{}）のドライブの種類を読めませんでした: {error}",
                    anchor_real.display()
                ),
                UserRemedy::Retry,
            )
        })?;
        if drive != DriveKind::Fixed {
            let found = match drive {
                DriveKind::Network => "ネットワークの場所",
                DriveKind::Removable => "取り外せるドライブ",
                _ => "ローカルの固定ドライブではない場所",
            };
            return Err(problem(
                format!(
                    "{USER_ANCHOR_LABEL}（{}）は{found}にあります。ローカルの固定ドライブのフォルダだけを使います",
                    anchor_real.display()
                ),
                fix_anchor,
            ));
        }
    }

    // --- 3. 起点の下のフォルダ --------------------------------------------------------
    let mut ancestors: Vec<O::Dir> = Vec::new();
    let mut current = anchor;
    let mut current_path = anchor_path;
    let mut current_real = anchor_real;
    for (index, name) in spec.dirs.iter().enumerate() {
        let child_path = current_path.join(name);
        // 文言の呼び名: 最後の要素が置き場そのもの、途中はその親。
        let label = if index + 1 == spec.dirs.len() && spec.file.is_none() {
            kind.label().to_string()
        } else {
            format!("{}の親のフォルダ", kind.label())
        };
        let child = match ops.open_child_dir(&current, name) {
            Ok(Some(child)) => child,
            Ok(None) => {
                return Ok(UserPlaceState::Absent(MissingUserPlace {
                    _ancestors: ancestors,
                    deepest: current,
                    deepest_path: current_path,
                    deepest_real: current_real,
                    missing: spec.dirs[index..].to_vec(),
                    owners: spec.owners,
                }))
            }
            Err(error) => {
                return Err(problem(
                    format!(
                        "{label}（{}）を開けませんでした: {error}",
                        child_path.display()
                    ),
                    UserRemedy::Retry,
                ))
            }
        };
        let child_real = check_dir(
            &label,
            &child_path,
            &current_real.join(name),
            &child,
            &spec.owners,
            &UserRemedy::RecreateDir(child_path.clone()),
        )?;
        ancestors.push(std::mem::replace(&mut current, child));
        current_path = child_path;
        current_real = child_real;
    }

    Ok(UserPlaceState::Present(CheckedUserPlace {
        _ancestors: ancestors,
        dir: current,
        dir_path: current_path,
        dir_real: current_real,
        file: spec.file,
        owners: spec.owners,
    }))
}

/// 置き場の中のファイルの役割（どこまで開いていてよいか）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserFileRole {
    /// 鍵: 信頼する相手以外に、読み取りを含むどの許可も無いこと。
    Secret,
    /// 証明書・ロックファイル・記録: 信頼する相手以外に、書き込み・削除・ACL の変更・所有者の
    /// 変更の許可が無いこと（読み取りはあってよい）。
    Public,
}

/// 確かめ済みの置き場のフォルダのハンドル基準で、名前 1 要素のファイルを辿らずに読み取りで
/// 開き、種類・場所（フォルダ・reparse point・本当の場所の食い違いを断る）と、所有者・DACL・
/// ハードリンク（[`user_place_problems`]）を確かめる（無ければ `None`。まだ読まない）。`label`
/// は文言の中の呼び名（「証明書」など）。
pub fn open_user_file<O: UserTlsOps>(
    ops: &O,
    place: &CheckedUserPlace<O::Dir>,
    name: &str,
    label: &str,
    role: UserFileRole,
) -> Result<Option<O::File>, UserPlaceProblem> {
    let dir_path = place.dir_path();
    let file = open_file_in(ops, place.dir(), dir_path, place.dir_real(), name, label).map_err(
        |refusal| {
            let remedy = if refusal.transient {
                UserRemedy::Retry
            } else {
                UserRemedy::RecreateDir(dir_path.to_path_buf())
            };
            problem(refusal.reason, remedy)
        },
    )?;
    let Some(file) = file else {
        return Ok(None);
    };
    let open = user_place_problems(
        &file.security(),
        place.owners(),
        role == UserFileRole::Public,
    );
    if !open.is_empty() {
        let risk = match role {
            UserFileRole::Secret => "ほかの利用者に読まれた恐れがあります",
            UserFileRole::Public => "ほかの利用者に書き換えられた恐れがあります",
        };
        return Err(problem(
            format!(
                "{label}（{}）は保護されていません（{}）。{risk}",
                dir_path.join(name).display(),
                user_reasons_text(&open)
            ),
            UserRemedy::Renew,
        ));
    }
    Ok(Some(file))
}

/// 利用者の置き場の証明書（pin の根）を読んだ結果。「無い」と「あるが通らない」を分ける
/// （§4.4: 「無い」は黙って外す、「あるが通らない」は pin に入れず警告を出す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserCertPin {
    /// 無い（置き場のフォルダか証明書がまだ作られていない。デスクトップ版の Hub を一度も
    /// 動かしていない利用者ではこれが普通）。
    Absent,
    /// 確かめに通った証明書。
    Usable(HubCertPin),
    /// あるが通らない（理由と対処の種類）。
    Unusable(UserPlaceProblem),
}

/// ChronoGazer 向けの入口: 利用者の `tls\` を確かめ（[`open_user_place`]）、証明書を辿らずに
/// 開いて確かめ（[`open_user_file`]。信頼する相手以外が書き換えられない）、**鍵も開いて露出を
/// 確かめ**（信頼する相手以外が読めない。中身は読まない）、握ったハンドルから証明書を読み、
/// PEM に証明書がちょうど 1 つで DER として解析できることを確かめる。書き込み・作成はしない。
///
/// サービスの置き場の [`crate::load_pinned_hub_cert`] と違って鍵も確かめるのは、鍵が同じ
/// 利用者のものなので開けることと、鍵の露出を見つけるのがこの読み手の役目だから（§4.4
/// 「毎周期、確かめ直す」「露出を見た証明書は二度と使わない」）。証明書があるのに鍵が無いのは
/// 「あるが通らない」（鍵の無い証明書を pin にしない）。
pub fn load_pinned_user_cert<O: UserTlsOps, P: UserTlsPlace + ?Sized>(
    ops: &O,
    place: &P,
) -> UserCertPin {
    use x509_cert::der::Decode;

    let checked = match open_user_place(ops, place, UserPlaceKind::Tls) {
        Ok(UserPlaceState::Present(checked)) => checked,
        Ok(UserPlaceState::Absent(_)) => return UserCertPin::Absent,
        Err(found) => return UserCertPin::Unusable(found),
    };
    let cert_path = checked.dir_path().join(CERT_FILE);
    let mut cert = match open_user_file(ops, &checked, CERT_FILE, "証明書", UserFileRole::Public)
    {
        Ok(Some(cert)) => cert,
        Ok(None) => return UserCertPin::Absent,
        Err(found) => return UserCertPin::Unusable(found),
    };
    match open_user_file(ops, &checked, KEY_FILE, "鍵", UserFileRole::Secret) {
        Ok(Some(_key)) => {}
        Ok(None) => {
            return UserCertPin::Unusable(problem(
                format!(
                    "{}（{}）に証明書はありますが、鍵（{KEY_FILE}）がありません",
                    UserPlaceKind::Tls.label(),
                    checked.dir_path().display()
                ),
                UserRemedy::Renew,
            ))
        }
        Err(found) => return UserCertPin::Unusable(found),
    }
    let real = cert.final_path();
    let cert_pem = match cert.read_all(MAX_PEM_BYTES) {
        Ok(cert_pem) => cert_pem,
        Err(error) => {
            return UserCertPin::Unusable(problem(
                format!(
                    "証明書（{}）を読めませんでした: {error}",
                    cert_path.display()
                ),
                UserRemedy::Retry,
            ))
        }
    };
    drop(cert);
    let broken = |error: String| {
        UserCertPin::Unusable(problem(
            format!("証明書（{}）は使えません: {error}", cert_path.display()),
            UserRemedy::Renew,
        ))
    };
    let der = match parse_single_certificate(&cert_pem) {
        Ok(der) => der,
        Err(error) => return broken(error),
    };
    if x509_cert::Certificate::from_der(&der).is_err() {
        return broken("証明書（DER）を読めません".to_string());
    }
    UserCertPin::Usable(HubCertPin {
        cert_pem,
        cert_path: real,
    })
}
