//! サービスの失効の記録（#1121 PR2b。docs/hub-desktop-tls-design.md §4.4.1「サービスの記録」・
//! §4.4.2 の場面 8・11・25・26）: サービスの `tls\` の証明書のうち、二度と使わないものの拇印。
//!
//! 置き場は `<ProgramData>\BantoHub\tls-revocations\`（[`SERVICE_REVOCATIONS_DIR`]）（`tls\` の外）の
//! [`REVOCATIONS_FILE`]。形は利用者の記録と同じ [`RevocationRecord`]（状態は常に `確定`）。
//!
//! ## 利用者の記録との違い
//!
//! - **書けるのは SYSTEM と管理者だけ**（設計 §3 の信頼の範囲）。書くのはサービスの Hub と
//!   elev（`prepare-hub-tls`）だけで、読むのはサービスの Hub・すべての利用者の ChronoGazer・
//!   chronogazer-server。そのため対の記録も `準備中` も持たず、**「無い」は空**として扱い、消失の
//!   判定はしない。
//! - **置き場の確かめはサービスの `tls\` と同じ**: ルートは [`crate::open_place`] と同じ手順
//!   （所有者・DACL）、記録のフォルダは本当のパス・reparse point でないフォルダ・所有者
//!   （SYSTEM・Administrators・TrustedInstaller。elev だけは実行した本人も）・
//!   `exposure_beyond_users_read`（Users の読み取りより広い許可が無い）。ファイルも種類・場所と
//!   `exposure_beyond_users_read`（ハードリンク・ほかの所有者を含む）を確かめる。
//! - **使えないときの 3 つ**（[`ServiceRevocationFault`]）: 置き場が確かめに通らない
//!   （`Untrusted`。記録のファイルがフォルダ・リンクになっている場合も含む。管理者が置き場を
//!   直してから `prepare-hub-tls --renew`）・形が壊れている
//!   （`Broken`。フォルダは通っているので、elev が置き換えで作り直せる）・一時的な I/O
//!   （`Transient`）。どれも、サービスの証明書を使わない側に倒す（場面 26）。
//! - 壊れているときも、行ごとに読める拇印は拾う（[`ServiceRevocationProblem::salvaged`]。
//!   拇印は増えるだけなので、安全側にしか動かない）。elev はそれを引き継いで作り直す。
//!
//! ## 書き方とロック（2026-11-14 オーナー決定 (4) の確かめ）
//!
//! 書く口は 2 つで、どちらもフォルダを確かめて握ったハンドル基準:
//!
//! - **無ければ作る**（[`create_service_revocations_if_absent`]。サービスの Hub の起動・elev）:
//!   同じフォルダの一時ファイルに書き切ってから、**置き換えずに**付け替える
//!   （[`ServiceRevocationOps::create_file_no_replace`]）。先に誰かが作っていれば何も変えず、
//!   読み直したものを使う。
//! - **作り直す**（[`rewrite_service_revocations`]。elev が、形が壊れた記録を引き継ぐときだけ。
//!   PR2c でサービスの Hub が露出を書き足すときも使う）: 残った一時ファイルを片付けてから、
//!   一時ファイル → 書き切る → 置き換え（[`ServiceRevocationOps::write_file_atomic`]）。
//!
//! 記録用のロックは足さない。書き手どうしの組み合わせはどれも失効を消さない:
//!
//! - 「無い」を見た 2 者（サービスの Hub の起動と elev）は、置き換えずに作るので、後の者は先の者の
//!   記録を上書きしない（どちらも空なので、どちらが勝っても同じ）。PR2c で「無い」ときに露出を
//!   書く場合も、置き換えずに作るので、同時に空を作った相手に消されない。
//! - 置き換えるのは、壊れた記録を見た elev（elev 同士は名前付き mutex で直列）と、健全な記録に
//!   書き足すサービスの Hub（PR2c）だけ。記録の書き込みはどれも原子的なので、健全な記録が
//!   壊れた記録に変わるのは第三者（管理者）が書いたときだけで、両者が同時に置き換えることは
//!   信頼の範囲の外の操作なしには起きない。
//! - elev の「`tls\` → 記録」の順は、名前付き mutex（`tls\` と記録の両方を作る・作り直す実行
//!   全体を直列化する）を取ってから記録を書くことで満たす。
//!
//! 一時ファイルの片付けは作り直しのときだけ（作るときにすると、同時に作っている相手の一時
//! ファイルを消しうる。消されても相手は書けないだけで、失効は消えない）。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::exposure_beyond_users_read;
use banto_safe_root::guard::{self, DirState, KnownFolders};

use crate::place::{dir_refusal, open_file_in, open_root, CheckedRoot};
use crate::text::{real_path_reason, reasons_text};
use crate::{
    CertThumbprint, CheckedPlace, ReadError, RecordState, Remedy, RevocationRecord, RevokedCert,
    RevokedCertPlace, TlsDir, TlsFile, TlsTrustOps, MAX_REVOCATIONS_BYTES, REVOCATIONS_FILE,
};

/// ルートの下の、サービスの失効の記録の置き場のフォルダ名（`tls\` の外）。
pub const SERVICE_REVOCATIONS_DIR: &str = "tls-revocations";

/// 文言の中のサービスの記録の置き場の呼び名。
pub const SERVICE_REVOCATIONS_LABEL: &str = "サービスの失効の記録の置き場";

/// 文言の中のサービスの記録の呼び名。
const RECORD_LABEL: &str = "サービスの失効の記録";

/// サービスの記録を読んだもの（「無い」は空）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRevocations {
    /// 記録が無い（ルート・置き場のフォルダ・ファイルのどれかが無い）。空として扱う。
    Absent,
    /// 確かめに通って、形が読めた（状態は `確定`）。
    Record(RevocationRecord),
}

impl ServiceRevocations {
    /// 失効した証明書（拇印の昇順。無ければ空）。
    pub fn entries(&self) -> &[RevokedCert] {
        match self {
            Self::Absent => &[],
            Self::Record(record) => record.entries(),
        }
    }

    /// 拇印が失効しているか。
    pub fn is_revoked(&self, thumbprint: &CertThumbprint) -> bool {
        match self {
            Self::Absent => false,
            Self::Record(record) => record.contains(thumbprint),
        }
    }
}

/// サービスの記録を使えない理由の種類（文言は呼ぶ側が決める）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceRevocationFault {
    /// 置き場（ルート・記録のフォルダ）が確かめに通らない（露出・所有者違い・リンク・場所の
    /// 食い違い）か、記録のファイルがフォルダ・reparse point・場所の食い違い（置き換えでは
    /// 直せないので、管理者に消してもらう。#1132 の Codex の指摘）。書いても別のアカウントに書き換えられうるので、直るまで書かない。管理者が
    /// 置き場を直してから `prepare-hub-tls --renew`。
    Untrusted,
    /// 置き場は通るが、記録の形が壊れている（読めない形・大きすぎる・`準備中`・ファイルの
    /// ACL の露出・ハードリンク・所有者違い）。elev が拇印を引き継いで置き換えで作り直せる。
    Broken,
    /// 一時的かもしれない（開けない・読めない・書けない）。状態を見たのではない。
    Transient,
}

/// サービスの記録を使えない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRevocationProblem {
    /// 何が合わなかったか（表示用）。
    pub reason: String,
    /// 種類。
    pub fault: ServiceRevocationFault,
    /// 形が壊れていても行ごとに読めた拇印（作り直すときに引き継ぐ。読めなければ空）。
    pub salvaged: Vec<RevokedCert>,
}

fn problem(reason: String, fault: ServiceRevocationFault) -> ServiceRevocationProblem {
    ServiceRevocationProblem {
        reason,
        fault,
        salvaged: Vec::new(),
    }
}

/// 確かめた、サービスの記録の置き場のフォルダ（辿らずに握ったまま持つ）。
pub struct ServiceRevocationsDir<D> {
    dir: D,
    path: PathBuf,
    real: PathBuf,
}

impl<D> ServiceRevocationsDir<D> {
    /// 握ったフォルダのハンドル。
    pub fn dir(&self) -> &D {
        &self.dir
    }

    /// 期待のパス（`<ProgramData>\BantoHub\tls-revocations`。文言用）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 記録のファイルの期待のパス（文言用）。
    pub fn record_path(&self) -> PathBuf {
        self.path.join(REVOCATIONS_FILE)
    }
}

/// 確かめたルートを指すもの（[`open_service_revocations_dir`] の引数）。
pub struct ServiceRoot<'a, D> {
    /// ルートのハンドル（確かめ済み）。
    pub root: &'a D,
    /// ルートの期待のパス（文言用）。
    pub root_path: &'a Path,
    /// ルートの本当のパス。
    pub root_real: &'a Path,
    /// 既知のフォルダ。
    pub known: &'a KnownFolders,
    /// 所有者として許す、実行した本人（elev だけ。読む側・サービスの Hub は `None`）。
    pub elevated: Option<&'a str>,
}

impl<'a, D> ServiceRoot<'a, D> {
    /// [`crate::open_place`] が確かめたルート（実行した本人は信頼しない）。
    pub fn of_place(place: &'a CheckedPlace<D>) -> Self {
        Self {
            root: place.root(),
            root_path: place.root_path(),
            root_real: place.root_real(),
            known: place.known(),
            elevated: None,
        }
    }
}

/// 開いた記録のフォルダを確かめる（本当のパス・種類・所有者・`exposure_beyond_users_read`）。
fn check_dir<O: TlsTrustOps>(
    dir: O::Dir,
    root: &ServiceRoot<'_, O::Dir>,
) -> Result<ServiceRevocationsDir<O::Dir>, ServiceRevocationProblem> {
    let path = root.root_path.join(SERVICE_REVOCATIONS_DIR);
    let real = dir.final_path();
    let untrusted = |reason: String| problem(reason, ServiceRevocationFault::Untrusted);
    if let Err(refusal) = guard::check_real_path(
        &real,
        &root.root_real.join(SERVICE_REVOCATIONS_DIR),
        root.known,
    ) {
        return Err(untrusted(real_path_reason(
            SERVICE_REVOCATIONS_LABEL,
            &path,
            &real,
            refusal,
        )));
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(dir.info()), None, root.elevated) {
        return Err(untrusted(dir_refusal(
            SERVICE_REVOCATIONS_LABEL,
            &path,
            &refusal,
        )));
    }
    let security = dir.security().map_err(|error| {
        problem(
            format!(
                "{SERVICE_REVOCATIONS_LABEL}（{}）の ACL を読めませんでした: {error}",
                path.display()
            ),
            ServiceRevocationFault::Transient,
        )
    })?;
    let open = exposure_beyond_users_read(&security);
    if !open.is_empty() {
        return Err(untrusted(format!(
            "{SERVICE_REVOCATIONS_LABEL}（{}）は保護されていません（{}）。ほかの利用者が失効の記録を書き換えた恐れがあります",
            path.display(),
            reasons_text(&open)
        )));
    }
    Ok(ServiceRevocationsDir { dir, path, real })
}

/// 確かめたルートのハンドル基準で、記録のフォルダを辿らずに開いて確かめる（無ければ
/// `Ok(None)`）。書き込み・作成はしない。
pub fn open_service_revocations_dir<O: TlsTrustOps>(
    ops: &O,
    root: &ServiceRoot<'_, O::Dir>,
) -> Result<Option<ServiceRevocationsDir<O::Dir>>, ServiceRevocationProblem> {
    let path = root.root_path.join(SERVICE_REVOCATIONS_DIR);
    match ops.open_child_dir(root.root, SERVICE_REVOCATIONS_DIR) {
        Ok(Some(dir)) => check_dir::<O>(dir, root).map(Some),
        Ok(None) => Ok(None),
        Err(error) => Err(problem(
            format!(
                "{SERVICE_REVOCATIONS_LABEL}（{}）を開けませんでした: {error}",
                path.display()
            ),
            ServiceRevocationFault::Transient,
        )),
    }
}

/// 形の壊れた記録から、行ごとに読める拇印を拾う（純関数。`revoked <拇印> <置き場> <時刻>` の形の
/// 行だけ。和は増えるだけなので、壊れた中身から拾っても安全側にしか動かない）。
pub fn salvage_revocations(bytes: &[u8]) -> Vec<RevokedCert> {
    let text = String::from_utf8_lossy(bytes);
    let mut found = Vec::new();
    for line in text.split('\n') {
        let words: Vec<&str> = line.split(' ').collect();
        let ["revoked", thumbprint, place, recorded_at] = words.as_slice() else {
            continue;
        };
        let Some(thumbprint) = CertThumbprint::from_hex(thumbprint) else {
            continue;
        };
        // サービスの記録なので、読めない置き場の種類はサービスの証明書として拾う。
        let place = if *place == "user" {
            RevokedCertPlace::User
        } else {
            RevokedCertPlace::Service
        };
        found.push(RevokedCert {
            thumbprint,
            place,
            recorded_at: recorded_at.parse().unwrap_or(0),
        });
    }
    // 拇印の昇順・重複なしにそろえる。
    RevocationRecord::new(RecordState::Committed, found)
        .entries()
        .to_vec()
}

/// 確かめたフォルダの中の記録を読む（無ければ [`ServiceRevocations::Absent`]）。書き込みはしない。
pub fn read_service_revocations_in<O: TlsTrustOps>(
    ops: &O,
    dir: &ServiceRevocationsDir<O::Dir>,
) -> Result<ServiceRevocations, ServiceRevocationProblem> {
    let shown = dir.record_path();
    let broken = |reason: String, salvaged: Vec<RevokedCert>| ServiceRevocationProblem {
        reason,
        fault: ServiceRevocationFault::Broken,
        salvaged,
    };
    let mut file = match open_file_in(
        ops,
        &dir.dir,
        &dir.path,
        &dir.real,
        REVOCATIONS_FILE,
        RECORD_LABEL,
    ) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(ServiceRevocations::Absent),
        Err(refusal) if refusal.transient => {
            return Err(problem(refusal.reason, ServiceRevocationFault::Transient))
        }
        // フォルダ・reparse point・場所の食い違い: 中身は読まない。原子的な置き換え
        // （`write_atomic`）はフォルダ・ジャンクションの宛先を断るので、`--renew` の作り直しでは
        // 直らない。置き場の問題（Untrusted）として扱い、管理者に記録の置き場を消してから
        // `--renew` するよう案内する（#1132 の Codex の指摘）。
        Err(refusal) => {
            return Err(problem(
                format!(
                    "{}。記録のファイルがフォルダ・リンクになっているため、置き換えでは直せません",
                    refusal.reason
                ),
                ServiceRevocationFault::Untrusted,
            ))
        }
    };
    let open = exposure_beyond_users_read(&file.security());
    let bytes = match file.read_all(MAX_REVOCATIONS_BYTES) {
        Ok(bytes) => bytes,
        Err(ReadError::TooLarge) => {
            return Err(broken(
                format!(
                    "{RECORD_LABEL}（{}）が大きすぎます（上限 {} KiB）",
                    shown.display(),
                    MAX_REVOCATIONS_BYTES / 1024
                ),
                Vec::new(),
            ))
        }
        Err(error) => {
            return Err(problem(
                format!(
                    "{RECORD_LABEL}（{}）を読めませんでした: {error}",
                    shown.display()
                ),
                ServiceRevocationFault::Transient,
            ))
        }
    };
    drop(file);
    if !open.is_empty() {
        return Err(broken(
            format!(
                "{RECORD_LABEL}（{}）は保護されていません（{}）。ほかの利用者に書き換えられた恐れがあります",
                shown.display(),
                reasons_text(&open)
            ),
            salvage_revocations(&bytes),
        ));
    }
    match RevocationRecord::parse(&bytes) {
        Ok(record) if record.state() == RecordState::Committed => {
            Ok(ServiceRevocations::Record(record))
        }
        Ok(record) => Err(broken(
            format!(
                "{RECORD_LABEL}（{}）の状態が「準備中」です（サービスの記録は「確定」だけを持ちます）",
                shown.display()
            ),
            record.entries().to_vec(),
        )),
        Err(error) => Err(broken(
            format!(
                "{RECORD_LABEL}（{}）の形が壊れています: {error}",
                shown.display()
            ),
            salvage_revocations(&bytes),
        )),
    }
}

/// 読み口（サービスの Hub・ChronoGazer・chronogazer-server が共有する）: ルートを
/// [`crate::open_place`] と同じ手順で確かめ、記録のフォルダと記録を確かめて読む。ルート・
/// フォルダ・ファイルのどれかが無ければ [`ServiceRevocations::Absent`]（空）。書き込み・作成は
/// しない。
pub fn read_service_revocations<O: TlsTrustOps>(
    ops: &O,
) -> Result<ServiceRevocations, ServiceRevocationProblem> {
    let CheckedRoot {
        root,
        root_path,
        root_real,
        known,
    } = match open_root(ops) {
        Ok(root) => root,
        Err(found) => {
            return match found.remedy {
                Remedy::Prepare => Ok(ServiceRevocations::Absent),
                Remedy::Retry => Err(problem(found.reason, ServiceRevocationFault::Transient)),
                _ => Err(problem(found.reason, ServiceRevocationFault::Untrusted)),
            }
        }
    };
    let service_root = ServiceRoot {
        root: &root,
        root_path: &root_path,
        root_real: &root_real,
        known: &known,
        elevated: None,
    };
    match open_service_revocations_dir(ops, &service_root)? {
        Some(dir) => read_service_revocations_in(ops, &dir),
        None => Ok(ServiceRevocations::Absent),
    }
}

// --- 書く（サービスの Hub と elev だけ） ------------------------------------------------

/// サービスの記録を書くために OS に対してする操作（SYSTEM・昇格した管理者だけが使う）。どれも
/// 確かめて握ったフォルダの**ハンドル基準**に、名前 1 要素で辿らずに行う。Windows の実装は
/// banto-hub-core の `WindowsHubTlsOps`（部品は `crate::win` と `banto_safe_root`）、テストは偽物。
pub trait ServiceRevocationOps: TlsTrustOps {
    /// `parent` の直下に、SYSTEM・Administrators にフル、`BUILTIN\Users` に読み取りと実行、
    /// 所有者 Administrators、保護された DACL（子へ継承する）のフォルダを作るのと同時に付けて
    /// 作る（`tls\` と同じ。既にあれば `Ok(None)`）。
    fn create_protected_child_dir(
        &self,
        parent: &Self::Dir,
        name: &str,
    ) -> Result<Option<Self::Dir>, String>;

    /// `dir` の中に `name` が無ければ、同じフォルダの一時ファイルに書き切ってから、**置き換え
    /// ずに**付け替える（作れたら `Ok(true)`、既にあれば何も変えずに `Ok(false)`）。ファイルは
    /// `tls\` の証明書と同じ記述子（SYSTEM・Administrators にフル、Users に読み取りと実行、
    /// 所有者 Administrators）を作るのと同時に付ける（「作成者を所有者に」の PC で、昇格した
    /// 利用者が所有者にならないため）。
    fn create_file_no_replace(
        &self,
        dir: &Self::Dir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String>;

    /// `dir` の中の `name` を、同じフォルダの一時ファイル → 書き切る（flush）→ 置き換え、で
    /// 中身ごと原子的に置き換える（記述子は [`ServiceRevocationOps::create_file_no_replace`] と
    /// 同じ）。
    fn write_file_atomic(&self, dir: &Self::Dir, name: &str, contents: &[u8])
        -> Result<(), String>;

    /// `dir` の中に残った、`name` の一時ファイルを読まずに消す。
    fn remove_stale_file_temps(&self, dir: &Self::Dir, name: &str) -> Result<(), String>;
}

/// 記録のフォルダを確かめて開く。無ければ保護された DACL を付けて作ってから、ルートのハンドル
/// 基準で確かめる（`.1` は作ったか）。
pub fn open_or_create_service_revocations_dir<O: ServiceRevocationOps>(
    ops: &O,
    root: &ServiceRoot<'_, O::Dir>,
) -> Result<(ServiceRevocationsDir<O::Dir>, bool), ServiceRevocationProblem> {
    if let Some(dir) = open_service_revocations_dir(ops, root)? {
        return Ok((dir, false));
    }
    let path = root.root_path.join(SERVICE_REVOCATIONS_DIR);
    let transient = |reason: String| problem(reason, ServiceRevocationFault::Transient);
    let (dir, created) = match ops.create_protected_child_dir(root.root, SERVICE_REVOCATIONS_DIR) {
        Ok(Some(dir)) => (dir, true),
        // 確かめと作るの間に誰かが作った: 開き直す。
        Ok(None) => match ops.open_child_dir(root.root, SERVICE_REVOCATIONS_DIR) {
            Ok(Some(dir)) => (dir, false),
            Ok(None) => {
                return Err(transient(format!(
                    "{SERVICE_REVOCATIONS_LABEL}（{}）を作ろうとしたら既にあり、開き直すと無くなっていました",
                    path.display()
                )))
            }
            Err(error) => {
                return Err(transient(format!(
                    "{SERVICE_REVOCATIONS_LABEL}（{}）を開けませんでした: {error}",
                    path.display()
                )))
            }
        },
        Err(error) => {
            return Err(transient(format!(
                "{SERVICE_REVOCATIONS_LABEL}（{}）を作れませんでした: {error}",
                path.display()
            )))
        }
    };
    check_dir::<O>(dir, root).map(|dir| (dir, created))
}

/// 記録が無ければ空で作る（場面 25。サービスの Hub の起動・elev）。置き換えないので、先に誰かが
/// 作っていれば何も変えない。作った後（作らなかったときも）読み直したものを返す（`.1` は作ったか）。
pub fn create_service_revocations_if_absent<O: ServiceRevocationOps>(
    ops: &O,
    dir: &ServiceRevocationsDir<O::Dir>,
) -> Result<(ServiceRevocations, bool), ServiceRevocationProblem> {
    let empty = RevocationRecord::new(RecordState::Committed, []);
    let created = ops
        .create_file_no_replace(&dir.dir, REVOCATIONS_FILE, &empty.encode())
        .map_err(|error| {
            problem(
                format!(
                    "{RECORD_LABEL}（{}）を作れませんでした: {error}",
                    dir.record_path().display()
                ),
                ServiceRevocationFault::Transient,
            )
        })?;
    match read_service_revocations_in(ops, dir)? {
        ServiceRevocations::Absent => Err(problem(
            format!(
                "{RECORD_LABEL}（{}）を作ったのに、読み直すとありませんでした",
                dir.record_path().display()
            ),
            ServiceRevocationFault::Transient,
        )),
        found => Ok((found, created)),
    }
}

/// 記録を `record`（状態は `確定` にする）で作り直す: 残った一時ファイルを片付けてから、
/// 置き換えで書く。elev が壊れた記録を引き継ぐとき（場面 11・26）に使う。
pub fn rewrite_service_revocations<O: ServiceRevocationOps>(
    ops: &O,
    dir: &ServiceRevocationsDir<O::Dir>,
    entries: impl IntoIterator<Item = RevokedCert>,
) -> Result<RevocationRecord, String> {
    let record = RevocationRecord::new(RecordState::Committed, entries);
    let bytes = record.encode();
    let shown = dir.record_path();
    if bytes.len() > MAX_REVOCATIONS_BYTES {
        return Err(format!(
            "{RECORD_LABEL}（{}）が大きくなりすぎます（{} 件）",
            shown.display(),
            record.entries().len()
        ));
    }
    // 片付けの失敗では止めない（一時ファイルは読まないので、残っても判定は変わらない）。
    let _ = ops.remove_stale_file_temps(&dir.dir, REVOCATIONS_FILE);
    ops.write_file_atomic(&dir.dir, REVOCATIONS_FILE, &bytes)
        .map_err(|error| {
            format!(
                "{RECORD_LABEL}（{}）を書けませんでした: {error}",
                shown.display()
            )
        })?;
    Ok(record)
}

/// 残った一時ファイルを片付ける（失敗は無視する）。ほかの書き手が作っている最中の一時ファイルも
/// 消しうるので、記録を置き換えで作り直す直前にだけ呼ぶ（[`rewrite_service_revocations`] が呼ぶ）。
pub fn remove_stale_service_revocation_temps<O: ServiceRevocationOps>(
    ops: &O,
    dir: &ServiceRevocationsDir<O::Dir>,
) {
    let _ = ops.remove_stale_file_temps(&dir.dir, REVOCATIONS_FILE);
}

/// サービスの Hub の起動のとき（場面 25・26）: 確かめたルートの下の記録のフォルダを確かめ（無ければ
/// 作る）、記録を読む（無ければ空で作る）。使えないときは理由を返す（呼ぶ側はサービスの証明書を
/// 使わない）。`.1` は記録を作ったか。
pub fn ensure_service_revocations<O: ServiceRevocationOps>(
    ops: &O,
    place: &CheckedPlace<O::Dir>,
) -> Result<(RevocationRecord, bool), ServiceRevocationProblem> {
    let root = ServiceRoot::of_place(place);
    let (dir, _) = open_or_create_service_revocations_dir(ops, &root)?;
    let (found, created) = match read_service_revocations_in(ops, &dir)? {
        ServiceRevocations::Absent => create_service_revocations_if_absent(ops, &dir)?,
        found => (found, false),
    };
    // 作った後の読み直しは Absent を返さない（返したら、作れなかったのと同じに扱う）。
    let ServiceRevocations::Record(record) = found else {
        return Err(problem(
            format!(
                "{RECORD_LABEL}（{}）を読めませんでした",
                dir.record_path().display()
            ),
            ServiceRevocationFault::Transient,
        ));
    };
    Ok((record, created))
}
