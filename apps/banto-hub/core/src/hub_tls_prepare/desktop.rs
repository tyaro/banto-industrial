//! デスクトップ Hub の既定の TLS（#1121 PR2d、設計 §4.1〜4.4）。
//!
//! 既知の利用者の置き場を確かめ、`tls\` の排他のファイル open でセッションをまたいで
//! 直列化する（上限 5 秒）。次に記録の排他も取り、両方を保ったまま記録 A・B の確定・
//! 初回作成・最後の照合と片付けを行う。無い組だけを protected DACL で作り、`creating` が
//! ある作りかけだけは復旧する。既存の壊れ・露出は自動で作り直さない。
//! 鍵を含むファイルはすべて、確かめて握ったフォルダのハンドル基準で操作する。
//! 読んだ組をメモリ上で確かめて渡す。周期の見張りは [`watch`]（PR2e）、作り直しとシェルの https は PR3。

use std::time::{Duration, Instant};

use banto_hub_tls_trust::win::{WinTlsDir, WindowsTlsTrustOps};
use banto_hub_tls_trust::{
    check_place_layout, check_spec, lock_user_revocations, open_user_file, open_user_place,
    peek_user_cert, record_user_revocation, user_place_problems, CertThumbprint, CheckedUserPlace,
    DefaultUserTlsPlace, FileOpenError, ReadError, ReconcileOptions, ReconcileOutcome,
    ReconcileReport, RevocationOps, RevocationSide, RevokedCertPlace, SideContent, TlsFile,
    TlsTrustOps, UncheckedUserCert, UserFault, UserFileRole, UserPlaceKind, UserPlaceProblem,
    UserPlaceState, UserTlsPlace,
};
use banto_safe_root::acl::PinnedFile;
use banto_safe_root::guard::paths_match;
use zeroize::Zeroizing;

use super::load::{DefaultHubTls, DefaultTlsUnavailable, DEFAULT_TLS_PORT};
use super::{
    generate_identity, inspect_identity, CERT_FILE, CERT_TEMP, KEY_FILE, KEY_TEMP, MAX_PEM_BYTES,
};

const LOCK_FILE: &str = "identity.lock";
const CREATING: &str = "creating";
const CREATING_CONTENTS: &[u8] = b"banto-hub-tls-creating 1\n";

fn unavailable(reason: impl Into<String>) -> DefaultTlsUnavailable {
    DefaultTlsUnavailable {
        reason: reason.into(),
        remedy: "Hub を閉じ、ログの理由と TLS・失効の記録の置き場の所有者・アクセス許可を確かめてください。本体の TLS の組が壊れた・失効した場合や、本体の鍵・証明書・置き場に露出や所有者の異常を見た場合は、ACL を直すだけでは漏れた鍵は無効になりません。利用者の TLS フォルダ（既定は %LOCALAPPDATA%\\BantoHub\\tls）だけを、中身を確認してフォルダごと削除し、Hub のデスクトップ版を再起動してください。親の置き場の所有者・アクセス許可が異常なら先に直してください。失効の記録は消さないでください".into(),
    }
}

/// 文字列の内容から対処を推測しない。本体・補助ファイル・証明書欠損を区別して案内する。
#[derive(Debug)]
struct DesktopFailure {
    reason: String,
    remedy: DesktopRemedy,
}

#[derive(Debug)]
enum DesktopRemedy {
    InspectAndRecreate,
    RepairPlaceOnly,
    InspectRemainingKey,
    InspectAuxiliaryFile(std::path::PathBuf),
    CleanupAuxiliaryFile(std::path::PathBuf),
    RetryLater,
}

impl std::fmt::Display for DesktopFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl DesktopFailure {
    fn recreate(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            remedy: DesktopRemedy::InspectAndRecreate,
        }
    }

    fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            remedy: DesktopRemedy::RetryLater,
        }
    }

    fn auxiliary(dir: &CheckedUserPlace<WinTlsDir>, name: &str, reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            remedy: DesktopRemedy::InspectAuxiliaryFile(dir.dir_path().join(name)),
        }
    }

    fn into_unavailable(self) -> DefaultTlsUnavailable {
        let remedy = match self.remedy {
            DesktopRemedy::InspectAndRecreate => return unavailable(self.reason),
            DesktopRemedy::RetryLater => "本体の hub-cert.pem・hub-key.pem は削除せず、ログの理由に従って置き場の指定・所有者・アクセス許可や読み書きの失敗の原因を確かめ、ロックの競合は終了まで待ってから、Hub のデスクトップ版を再起動してください".into(),
            DesktopRemedy::RepairPlaceOnly => "証明書も鍵もありません。Hub を閉じ、置き場と親の置き場の所有者・アクセス許可を修復してから、Hub のデスクトップ版を再起動してください".into(),
            DesktopRemedy::InspectRemainingKey => "証明書はありません。Hub を閉じ、利用者の TLS フォルダの中身を確認してください。鍵が残る場合はフォルダごと削除して作り直してください。鍵も無い場合は、置き場と親の置き場の所有者・アクセス許可を修復してから再起動してください。失効の記録は消さないでください".into(),
            DesktopRemedy::InspectAuxiliaryFile(path) => format!("本体の hub-cert.pem・hub-key.pem は削除せず、Hub を閉じ、補助ファイル（{}）の中身・所有者・アクセス許可を確認してください。補助ファイルだけを安全に削除するか、その所有者・アクセス許可を修復してから再起動してください", path.display()),
            DesktopRemedy::CleanupAuxiliaryFile(path) => format!("本体の鍵・証明書の検査と失効照合は済んでいます。本体の hub-cert.pem・hub-key.pem は削除せず、Hub を閉じ、補助ファイル（{}）の中身・アクセス許可と他のプロセスによる使用を確認し、安全に片付けてから再起動してください", path.display()),
        };
        DefaultTlsUnavailable {
            reason: self.reason,
            remedy,
        }
    }
}

/// env の検証・profile 排他の取得後に runtime が呼ぶ。既存の TLS 設定があるとき、
/// Shell 以外、テスト専用の profile 排他を省くときは、利用者の置き場を一切触らない。
/// 同期 I/O・ロック待ち・鍵の作成は blocking pool で行い、終了まで `keep_alive` を持つ。
pub async fn load_for_shell(
    host: crate::profile_lock::HubHostKind,
    skip_profile_lock: bool,
    tls_configured: bool,
    keep_alive: std::sync::Arc<Option<crate::profile_lock::ProfileLockGuard>>,
) -> Option<Result<DefaultHubTls, DefaultTlsUnavailable>> {
    load_for_shell_with(
        host,
        skip_profile_lock,
        tls_configured,
        keep_alive,
        load_desktop_identity,
    )
    .await
}

async fn load_for_shell_with<G, F>(
    host: crate::profile_lock::HubHostKind,
    skip_profile_lock: bool,
    tls_configured: bool,
    keep_alive: G,
    load: F,
) -> Option<Result<DefaultHubTls, DefaultTlsUnavailable>>
where
    G: Send + 'static,
    F: FnOnce() -> Result<DefaultHubTls, DefaultTlsUnavailable> + Send + 'static,
{
    let task = select_default(host, skip_profile_lock, tls_configured, || {
        tokio::task::spawn_blocking(move || {
            // await 側がキャンセルされても、ファイル操作の終了まで profile の排他を保つ。
            let result = load();
            drop(keep_alive);
            result
        })
    })?;
    Some(task.await.unwrap_or_else(|error| {
        // panic の payload は鍵などを含む可能性があるのでログへ展開しない。
        let reason = if error.is_panic() {
            "パニック"
        } else {
            "キャンセル"
        };
        Err(DesktopFailure::retry(format!(
            "デスクトップ版の TLS の準備タスクが異常終了しました（{reason}）"
        ))
        .into_unavailable())
    }))
}

fn select_default<T>(
    host: crate::profile_lock::HubHostKind,
    skip_profile_lock: bool,
    tls_configured: bool,
    load: impl FnOnce() -> T,
) -> Option<T> {
    (host == crate::profile_lock::HubHostKind::Shell && !skip_profile_lock && !tls_configured)
        .then(load)
}

/// 明示した TLS の環境変数が無い Shell からだけ呼ぶ。本番は既知のフォルダとトークンの SID。
pub fn load_desktop_identity() -> Result<DefaultHubTls, DefaultTlsUnavailable> {
    let ops = WindowsTlsTrustOps;
    let place = DefaultUserTlsPlace::resolve(&ops)
        .map_err(|error| DesktopFailure::retry(error.reason).into_unavailable())?;
    load_in(&ops, &place, &ReconcileOptions::default(), now_unix())
        .map_err(DesktopFailure::into_unavailable)
}

/// 利用者向けの 8723 の競合の案内（サービス・別のプロファイルが待ち受けている場合も含む）。
pub fn bind_failure_lines(port: u16, error: &str) -> Vec<String> {
    vec![
        format!("[WARN] banto-hub: デスクトップ版の既定の TLS（https://127.0.0.1:{port}）を待ち受けられないので、TLS 無しで起動します: {error}"),
        format!("[WARN] banto-hub: 対処: 127.0.0.1:{port} を使っている別のプロセス・Hub のサービス・別のプロファイルを確かめてから、Hub のデスクトップ版を再起動してください。それまで、証明書を pin したクライアントは繋がりません"),
    ]
}

fn now_unix() -> i64 {
    i64::try_from(WindowsTlsTrustOps.now_unix_seconds()).unwrap_or(i64::MAX)
}

/// 対処のための存在確認だけ（信頼や読み込みの入口ではない）。所有者・ACL が通らない場所でも
/// 起点から親のハンドル基準で辿らずに開き、祖先を保持する。鍵の内容は一切読まない。
/// 途中がリンク・種類違い・I/O 失敗なら「無い」と断定しない。
fn key_is_absent<P: UserTlsPlace + ?Sized>(ops: &WindowsTlsTrustOps, place: &P) -> bool {
    let spec = place.spec(UserPlaceKind::Tls);
    if check_spec(&spec).is_err() || spec.file.is_some() {
        return false;
    }
    let plain = |dir: &WinTlsDir| {
        let info = dir.pinned().info();
        info.is_dir && !info.is_reparse_point
    };
    let anchor = match ops.pin_dir(&spec.anchor) {
        Ok(None) => return true,
        Ok(Some(dir)) if plain(&dir) => dir,
        _ => return false,
    };
    let mut held = vec![anchor];
    for name in &spec.dirs {
        match ops.open_child_dir(held.last().expect("起点を保持している"), name) {
            Ok(None) => return true,
            Ok(Some(dir)) if plain(&dir) => held.push(dir),
            _ => return false,
        }
    }
    matches!(
        ops.open_child_file(held.last().expect("起点を保持している"), KEY_FILE),
        Ok(None)
    )
}

fn problem<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    found: UserPlaceProblem,
) -> DesktopFailure {
    problem_recording(ops, place, found, |thumbprint| {
        record_user_revocation(ops, place, options, thumbprint, RevokedCertPlace::User)
    })
}

fn problem_recording<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    found: UserPlaceProblem,
    record: impl FnOnce(CertThumbprint) -> ReconcileReport,
) -> DesktopFailure {
    let mut reason = found.reason;
    if found.fault == UserFault::Transient {
        return DesktopFailure::retry(reason);
    }
    if found.fault.revokes() {
        let seen = peek_user_cert(ops, place);
        if seen == UncheckedUserCert::Absent {
            let remedy = if key_is_absent(ops, place) {
                reason.push_str("。証明書も鍵もありません。置き場の所有者・アクセス許可を修復してから再起動してください");
                DesktopRemedy::RepairPlaceOnly
            } else {
                reason.push_str("。証明書はありません。残っている鍵の確認が必要です");
                DesktopRemedy::InspectRemainingKey
            };
            return DesktopFailure { reason, remedy };
        }
        let recorded = if let UncheckedUserCert::Seen(seen) = seen {
            let report = record(seen.thumbprint);
            if report.revocations.is_revoked(&seen.thumbprint) {
                reason.push_str("。証明書の拇印を失効の記録に残しました");
                true
            } else {
                reason.push_str("。証明書の拇印を失効の記録に残せませんでした");
                false
            }
        } else {
            reason.push_str("。証明書の拇印を読めず、失効を記録できませんでした");
            false
        };
        if !recorded {
            // §7 の受容済みの限界: 永続の記録が無ければ再起動後の失効は保証できない。
            reason.push_str("。今回の起動ではこの証明書を使いません。失効の記録が無いまま再起動すると同じ証明書が使われることがあります。ACL を直すだけでは漏れた鍵は無効になりません。Hub を閉じ、利用者の TLS フォルダの中身を確認しフォルダごと削除してから再起動してください（失効の記録は消さないでください）");
        }
    }
    DesktopFailure::recreate(reason)
}

fn tls_place<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
) -> Result<CheckedUserPlace<WinTlsDir>, DesktopFailure> {
    // 作った直後も起点から読み直す。既存のフォルダの DACL は書き換えない。
    for _ in 0..16 {
        match open_user_place(ops, place, UserPlaceKind::Tls)
            .map_err(|found| problem(ops, place, options, found))?
        {
            UserPlaceState::Present(dir) => return Ok(dir),
            UserPlaceState::Absent(missing) => {
                let name = missing
                    .missing()
                    .first()
                    .ok_or_else(|| DesktopFailure::retry("TLS の置き場の名前がありません"))?;
                missing
                    .deepest()
                    .pinned()
                    .create_child_dir_current_user(name)
                    .map_err(|err| {
                        DesktopFailure::retry(format!("TLS のフォルダ {name} を作れません: {err}"))
                    })?;
            }
        }
    }
    Err(DesktopFailure::retry(
        "TLS の置き場を作成・再確認できませんでした（試行回数の上限）",
    ))
}

/// 本体の証明書・鍵の問題だけを、失効の判定に渡す。
fn checked_identity_file<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    dir: &CheckedUserPlace<WinTlsDir>,
    name: &str,
    role: UserFileRole,
    record: &mut impl FnMut(CertThumbprint) -> ReconcileReport,
) -> Result<Option<PinnedFile>, DesktopFailure> {
    open_user_file(ops, dir, name, name, role)
        .map_err(|found| problem_recording(ops, place, found, record))
}

/// 初回作成の印・一時ファイルは安全性を確かめ、問題があれば TLS を使わず残す。
/// 本体とは別の未使用の補助ファイルなので、所有者や ACL の問題でも本体の失効には渡さない。
fn checked_auxiliary_file(
    ops: &WindowsTlsTrustOps,
    dir: &CheckedUserPlace<WinTlsDir>,
    name: &str,
) -> Result<Option<PinnedFile>, DesktopFailure> {
    let role = if name == CREATING {
        UserFileRole::Public
    } else {
        UserFileRole::Secret
    };
    open_user_file(ops, dir, name, name, role).map_err(|found| {
        if found.fault == UserFault::Transient {
            DesktopFailure::retry(found.reason)
        } else {
            DesktopFailure::auxiliary(dir, name, found.reason)
        }
    })
}

fn create(dir: &WinTlsDir, name: &str, bytes: &[u8]) -> Result<(), DesktopFailure> {
    let made = dir
        .pinned()
        .create_child_file_current_user(name, bytes)
        .map_err(|error| DesktopFailure::retry(format!("TLS の {name} を作れません: {error}")))?;
    if made.is_none() {
        return Err(DesktopFailure::retry(format!(
            "{name} は既にあります（置き換えません）"
        )));
    }
    Ok(())
}

fn lock(
    ops: &WindowsTlsTrustOps,
    dir: &CheckedUserPlace<WinTlsDir>,
    options: &ReconcileOptions,
) -> Result<PinnedFile, DesktopFailure> {
    lock_with_sleep(ops, dir, options, |duration| ops.sleep(duration))
}

fn lock_with_sleep(
    ops: &WindowsTlsTrustOps,
    dir: &CheckedUserPlace<WinTlsDir>,
    options: &ReconcileOptions,
    mut sleep: impl FnMut(Duration),
) -> Result<PinnedFile, DesktopFailure> {
    let until = Instant::now() + options.lock_wait;
    loop {
        match ops.open_child_file_exclusive(dir.dir(), LOCK_FILE) {
            Ok(Some(file)) => {
                let security = TlsFile::security(&file);
                if security.is_dir
                    || security.is_reparse_point
                    || security.links != 1
                    || !paths_match(&TlsFile::final_path(&file), &dir.dir_real().join(LOCK_FILE))
                    || !user_place_problems(&security, dir.owners(), true).is_empty()
                {
                    return Err(DesktopFailure::auxiliary(
                        dir,
                        LOCK_FILE,
                        "TLS のロックファイルの場所・所有者・アクセス許可が通りません",
                    ));
                }
                return Ok(file);
            }
            Ok(None) => {
                // 作成と open の間で他のセッションが先に握ることもある。再試行して確認する。
                match dir
                    .dir()
                    .pinned()
                    .create_child_file_current_user(LOCK_FILE, b"")
                {
                    Ok(_) => {}
                    Err(banto_safe_root::SafeRootError::Io {
                        os_error: Some(32), ..
                    }) => {}
                    Err(error) => {
                        return Err(DesktopFailure::retry(format!(
                            "TLS のロックファイルを作れません: {error}"
                        )))
                    }
                }
            }
            Err(FileOpenError::InUse) => {}
            Err(FileOpenError::Other(error)) => return Err(DesktopFailure::retry(error)),
        }
        if Instant::now() >= until {
            return Err(DesktopFailure::retry(
                "TLS のロック（identity.lock）を上限までに取れませんでした",
            ));
        }
        sleep(
            options
                .lock_poll
                .max(Duration::from_millis(1))
                .min(until.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
fn settled<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
) -> Result<(), DesktopFailure> {
    settle_with(|| banto_hub_tls_trust::reconcile_user_revocations(ops, place, options))
}

fn failure_from_report(report: ReconcileReport) -> DesktopFailure {
    let reason = format!(
        "利用者の失効の記録 A・B を確定できません: {:?}（{:?}）",
        report.outcome, report.warnings
    );
    if report.outcome == ReconcileOutcome::Incomplete
        || report.warnings.iter().any(|warning| {
            matches!(
                warning,
                banto_hub_tls_trust::RevocationWarning::Layout(_)
                    | banto_hub_tls_trust::RevocationWarning::LockRefused(_)
            )
        })
    {
        DesktopFailure::retry(reason)
    } else {
        DesktopFailure::recreate(reason)
    }
}

fn settle_with(mut reconcile: impl FnMut() -> ReconcileReport) -> Result<(), DesktopFailure> {
    let mut report = reconcile();
    // 修復後の再確認も、保持する口なら同じロックの下で行う。
    if report.outcome == ReconcileOutcome::Refused
        && report.saw_loss()
        && report.revocations.is_settled()
    {
        report = reconcile();
    }
    if report.outcome != ReconcileOutcome::Consistent {
        return Err(failure_from_report(report));
    }
    Ok(())
}

type PairBytes = (Option<Vec<u8>>, Option<Zeroizing<Vec<u8>>>);

fn read_pair<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    dir: &CheckedUserPlace<WinTlsDir>,
    record: &mut impl FnMut(CertThumbprint) -> ReconcileReport,
) -> Result<PairBytes, DesktopFailure> {
    // 両方のハンドルを握ってから読む。ACL やリンクの問題は作成中でも復旧しない。
    let mut cert = checked_identity_file(ops, place, dir, CERT_FILE, UserFileRole::Public, record)?;
    let mut key = checked_identity_file(ops, place, dir, KEY_FILE, UserFileRole::Secret, record)?;
    let cert = cert
        .as_mut()
        .map(|file| TlsFile::read_all(file, MAX_PEM_BYTES))
        .transpose()
        .map_err(|err| identity_read_failure(CERT_FILE, err))?;
    let key = key
        .as_mut()
        .map(|file| TlsFile::read_all(file, MAX_PEM_BYTES).map(Zeroizing::new))
        .transpose()
        .map_err(|err| identity_read_failure(KEY_FILE, err))?;
    Ok((cert, key))
}

fn identity_read_failure(name: &str, error: ReadError) -> DesktopFailure {
    let reason = format!("TLS の {name} を読めませんでした: {error}");
    match error {
        ReadError::TooLarge => DesktopFailure::recreate(reason),
        ReadError::Other(_) => DesktopFailure::retry(reason),
    }
}

fn pair_matches(cert: &[u8], key: &[u8]) -> bool {
    use x509_cert::der::Decode;
    // 期限だけで作り直さない。完全な組は、印が残っていても必ずその組を優先する。
    let at = banto_hub_tls_trust::parse_single_certificate(cert)
        .ok()
        .and_then(|der| x509_cert::Certificate::from_der(&der).ok())
        .and_then(|cert| {
            i64::try_from(
                cert.tbs_certificate
                    .validity
                    .not_before
                    .to_unix_duration()
                    .as_secs(),
            )
            .ok()
        });
    at.is_some_and(|at| inspect_identity(cert, key, at).is_ok())
}

fn cleanup(dir: &WinTlsDir, names: &[&str]) -> Result<(), DesktopFailure> {
    for name in names {
        dir.managed().remove(name).map_err(|err| {
            DesktopFailure::retry(format!("TLS の作りかけ {name} を片付けられません: {err}"))
        })?;
    }
    Ok(())
}

/// 本体の検査・失効照合を終えてから呼ぶ。失敗しても本体を削除・再作成する理由にはしない。
fn cleanup_auxiliary_files(dir: &CheckedUserPlace<WinTlsDir>) -> Result<(), DesktopFailure> {
    for name in [KEY_TEMP, CERT_TEMP, CREATING] {
        dir.dir().managed().remove(name).map_err(|error| {
            let path = dir.dir_path().join(name);
            DesktopFailure {
                reason: format!(
                    "補助ファイル（{}）の片付けに失敗しました: {error}",
                    path.display()
                ),
                remedy: DesktopRemedy::CleanupAuxiliaryFile(path),
            }
        })?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationStage {
    BeforeGeneration,
    BeforePublish,
    AfterPublish,
}

fn load_in<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    now: i64,
) -> Result<DefaultHubTls, DesktopFailure> {
    load_in_observed(ops, place, options, now, |_| {})
}

fn load_in_observed<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    now: i64,
    mut observe: impl FnMut(CreationStage),
) -> Result<DefaultHubTls, DesktopFailure> {
    check_place_layout(place).map_err(DesktopFailure::retry)?;
    let dir = tls_place(ops, place, options)?;
    let _lock = lock(ops, &dir, options)?;
    let mut records = lock_user_revocations(ops, place, options)
        .map_err(|report| failure_from_report(*report))?;
    settle_with(|| records.reconcile())?;
    // 本体の露出を補助ファイルの異常で隠さない。失効の記録への書き足しまで先に行う。
    let (cert, key) = read_pair(ops, place, &dir, &mut |thumbprint| {
        records.record(thumbprint, RevokedCertPlace::User)
    })?;
    let complete = matches!((&cert, &key), (Some(cert), Some(key)) if pair_matches(cert, key));
    let marker = checked_auxiliary_file(ops, &dir, CREATING)?;
    let creating = if let Some(mut marker) = marker {
        if TlsFile::read_all(&mut marker, 128).map_err(|error| {
            let reason = format!("TLS の作成中の印（{CREATING}）を読めませんでした: {error}");
            match error {
                ReadError::TooLarge => DesktopFailure::auxiliary(&dir, CREATING, reason),
                ReadError::Other(_) => DesktopFailure::retry(reason),
            }
        })? != CREATING_CONTENTS
        {
            return Err(DesktopFailure::auxiliary(
                &dir,
                CREATING,
                "TLS の作成中の印の形が壊れています",
            ));
        }
        true
    } else {
        false
    };
    // 印が無い一時ファイルを「前の作りかけ」と決めつけない。完全な組を使う場合も、
    // 印を片付ける前に一時ファイルの種類・所有者・ACL を確かめる。
    for name in [KEY_TEMP, CERT_TEMP] {
        if checked_auxiliary_file(ops, &dir, name)?.is_some() && !creating {
            return Err(DesktopFailure::auxiliary(
                &dir,
                name,
                "TLS の一時ファイルが残っていますが、作成中の印がありません",
            ));
        }
    }
    if !complete {
        if !creating && (cert.is_some() || key.is_some()) {
            return Err(DesktopFailure::recreate("TLS の鍵と証明書が揃っていないか、組として合いません（作成中の印が無いので作り直しません）"));
        }
        // 読み取りのハンドルを放してから、作成中と確認できたファイルだけを消す。
        drop((cert, key));
        if !creating {
            create(dir.dir(), CREATING, CREATING_CONTENTS)?;
        }
        cleanup(dir.dir(), &[KEY_FILE, CERT_FILE, KEY_TEMP, CERT_TEMP])?;
        observe(CreationStage::BeforeGeneration);
        let identity = generate_identity(now).map_err(DesktopFailure::retry)?;
        create(dir.dir(), KEY_TEMP, identity.key_pem.as_bytes())?;
        create(dir.dir(), CERT_TEMP, identity.cert_pem.as_bytes())?;
        observe(CreationStage::BeforePublish);
        for (from, to) in [(KEY_TEMP, KEY_FILE), (CERT_TEMP, CERT_FILE)] {
            if !dir
                .dir()
                .managed()
                .rename_no_replace(from, to)
                .map_err(|err| {
                    DesktopFailure::retry(format!(
                        "TLS の {from} を {to} に付け替えられませんでした: {err}"
                    ))
                })?
            {
                return Err(DesktopFailure::retry(format!(
                    "TLS の {from} を {to} に付け替えられませんでした"
                )));
            }
        }
    }
    if !complete {
        observe(CreationStage::AfterPublish);
    }
    let (cert, key) = read_pair(ops, place, &dir, &mut |thumbprint| {
        records.record(thumbprint, RevokedCertPlace::User)
    })?;
    let missing = |reason| {
        if creating || !complete {
            DesktopFailure::retry(reason)
        } else {
            DesktopFailure::recreate(reason)
        }
    };
    let cert = cert.ok_or_else(|| missing("TLS の証明書がありません"))?;
    let key = key.ok_or_else(|| missing("TLS の鍵がありません"))?;
    let identity = DefaultHubTls::from_checked_pem(
        DEFAULT_TLS_PORT,
        cert,
        key,
        dir.dir_path().join(CERT_FILE),
        now,
    )
    .map_err(DesktopFailure::recreate)?;
    let thumbprint =
        CertThumbprint::of_pem(identity.cert_pem()).map_err(DesktopFailure::recreate)?;
    let current = records.read();
    if current.is_revoked(&thumbprint) {
        return Err(DesktopFailure::recreate(
            "利用者の失効の記録が揃っていないか、この証明書は失効しています",
        ));
    }
    if !current.is_settled() {
        let reason = "利用者の失効の記録が揃っていません";
        if RevocationSide::BOTH
            .iter()
            .any(|side| matches!(current.side(*side), SideContent::Unread { .. }))
        {
            return Err(DesktopFailure::retry(reason));
        }
        return Err(DesktopFailure::recreate(reason));
    }
    cleanup_auxiliary_files(&dir)?;
    Ok(identity)
}

#[cfg(test)]
mod tests;

/// デスクトップ既定 TLS の周期検査と停止・復帰。
pub mod watch;
