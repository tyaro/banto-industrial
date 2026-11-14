//! デスクトップ Hub の既定の TLS（#1121 PR2d、設計 §4.1〜4.4）。
//!
//! 既知の利用者の置き場を確かめ、`tls\` の排他のファイル open でセッションをまたいで
//! 直列化する（上限 5 秒）。記録 A・B を確定させてから、無い組だけを protected DACL で
//! 作る。`creating` がある作りかけだけは復旧する。既存の壊れ・露出は自動で作り直さない。
//! 鍵を含むファイルはすべて、確かめて握ったフォルダのハンドル基準で操作する。
//! 読んだ組をメモリ上で確かめて渡す。周期の見張りは PR2e、作り直しとシェルの https は PR3。

use std::time::{Duration, Instant};

use banto_hub_tls_trust::win::{WinTlsDir, WindowsTlsTrustOps};
use banto_hub_tls_trust::{
    check_place_layout, open_user_file, open_user_place, peek_user_cert, read_user_revocations,
    reconcile_user_revocations, record_user_revocation, user_place_problems, CertThumbprint,
    CheckedUserPlace, DefaultUserTlsPlace, FileOpenError, ReconcileOptions, ReconcileOutcome,
    RevocationOps, RevokedCertPlace, TlsFile, UncheckedUserCert, UserFileRole, UserPlaceKind,
    UserPlaceProblem, UserPlaceState, UserTlsPlace, DEFAULT_LOCK_POLL,
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
        remedy: "Hub を閉じ、ログの理由と TLS・失効の記録の置き場の所有者・アクセス許可を確かめてください。TLS の組が壊れた・失効した場合は、利用者の TLS フォルダ（既定は %LOCALAPPDATA%\\BantoHub\\tls）だけを、中身を確認してフォルダごと削除し、Hub のデスクトップ版を再起動してください。親の置き場の所有者・アクセス許可が異常なら先に直してください。失効の記録は消さないでください".into(),
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
        Err(unavailable(format!(
            "デスクトップ版の TLS の準備タスクが異常終了しました（{reason}）"
        )))
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
    let place = DefaultUserTlsPlace::resolve(&ops).map_err(|error| unavailable(error.reason))?;
    load_in(&ops, &place, &ReconcileOptions::default(), now_unix()).map_err(unavailable)
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

fn problem<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    found: UserPlaceProblem,
) -> String {
    let mut reason = found.reason;
    if found.fault.revokes() {
        if let UncheckedUserCert::Seen(seen) = peek_user_cert(ops, place) {
            let report = record_user_revocation(
                ops,
                place,
                options,
                seen.thumbprint,
                RevokedCertPlace::User,
            );
            reason.push_str(if report.revocations.is_revoked(&seen.thumbprint) {
                "。証明書の拇印を失効の記録に残しました"
            } else {
                "。証明書の拇印を失効の記録に残せませんでした（この証明書は使いません）"
            });
        }
    }
    reason
}

fn tls_place<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
) -> Result<CheckedUserPlace<WinTlsDir>, String> {
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
                    .ok_or("TLS の置き場の名前がありません")?;
                missing
                    .deepest()
                    .pinned()
                    .create_child_dir_current_user(name)
                    .map_err(|err| err.to_string())?;
            }
        }
    }
    Err("TLS の置き場を作成・再確認できませんでした（試行回数の上限）".into())
}

fn checked_file<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    dir: &CheckedUserPlace<WinTlsDir>,
    name: &str,
    role: UserFileRole,
) -> Result<Option<PinnedFile>, String> {
    open_user_file(ops, dir, name, name, role).map_err(|found| problem(ops, place, options, found))
}

fn create(dir: &WinTlsDir, name: &str, bytes: &[u8]) -> Result<(), String> {
    let made = dir
        .pinned()
        .create_child_file_current_user(name, bytes)
        .map_err(|error| error.to_string())?;
    if made.is_none() {
        return Err(format!("{name} は既にあります（置き換えません）"));
    }
    Ok(())
}

fn lock(
    ops: &WindowsTlsTrustOps,
    dir: &CheckedUserPlace<WinTlsDir>,
    wait: Duration,
) -> Result<PinnedFile, String> {
    let until = Instant::now() + wait;
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
                    return Err(
                        "TLS のロックファイルの場所・所有者・アクセス許可が通りません".into(),
                    );
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
                    Err(error) => return Err(format!("TLS のロックファイルを作れません: {error}")),
                }
            }
            Err(FileOpenError::InUse) => {}
            Err(FileOpenError::Other(error)) => return Err(error),
        }
        if Instant::now() >= until {
            return Err("TLS のロックを上限までに取れませんでした".into());
        }
        ops.sleep(DEFAULT_LOCK_POLL.min(until.saturating_duration_since(Instant::now())));
    }
}

fn settled<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
) -> Result<(), String> {
    let mut report = reconcile_user_revocations(ops, place, options);
    // PR2 の決定: この回で消失を修復したときだけもう一度確認する。
    if report.outcome == ReconcileOutcome::Refused
        && report.saw_loss()
        && report.revocations.is_settled()
    {
        report = reconcile_user_revocations(ops, place, options);
    }
    if report.outcome != ReconcileOutcome::Consistent {
        return Err(format!(
            "利用者の失効の記録 A・B を確定できません: {:?}（{:?}）",
            report.outcome, report.warnings
        ));
    }
    Ok(())
}

type PairBytes = (Option<Vec<u8>>, Option<Zeroizing<Vec<u8>>>);

fn read_pair<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    dir: &CheckedUserPlace<WinTlsDir>,
) -> Result<PairBytes, String> {
    // 両方のハンドルを握ってから読む。ACL やリンクの問題は作成中でも復旧しない。
    let mut cert = checked_file(ops, place, options, dir, CERT_FILE, UserFileRole::Public)?;
    let mut key = checked_file(ops, place, options, dir, KEY_FILE, UserFileRole::Secret)?;
    let cert = cert
        .as_mut()
        .map(|file| TlsFile::read_all(file, MAX_PEM_BYTES))
        .transpose()
        .map_err(|err| err.to_string())?;
    let key = key
        .as_mut()
        .map(|file| TlsFile::read_all(file, MAX_PEM_BYTES).map(Zeroizing::new))
        .transpose()
        .map_err(|err| err.to_string())?;
    Ok((cert, key))
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

fn cleanup(dir: &WinTlsDir, names: &[&str]) -> Result<(), String> {
    for name in names {
        dir.managed().remove(name).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn load_in<P: UserTlsPlace + ?Sized>(
    ops: &WindowsTlsTrustOps,
    place: &P,
    options: &ReconcileOptions,
    now: i64,
) -> Result<DefaultHubTls, String> {
    check_place_layout(place)?;
    let dir = tls_place(ops, place, options)?;
    let _lock = lock(ops, &dir, options.lock_wait)?;
    settled(ops, place, options)?;
    let marker = checked_file(ops, place, options, &dir, CREATING, UserFileRole::Public)?;
    let creating = if let Some(mut marker) = marker {
        if TlsFile::read_all(&mut marker, 128).map_err(|error| error.to_string())?
            != CREATING_CONTENTS
        {
            return Err("TLS の作成中の印の形が壊れています".into());
        }
        true
    } else {
        false
    };
    let (cert, key) = read_pair(ops, place, options, &dir)?;
    let complete = matches!((&cert, &key), (Some(cert), Some(key)) if pair_matches(cert, key));
    // 印が無い一時ファイルを「前の作りかけ」と決めつけない。完全な組を使う場合も、
    // 印を片付ける前に一時ファイルの種類・所有者・ACL を確かめる。
    for name in [KEY_TEMP, CERT_TEMP] {
        if checked_file(ops, place, options, &dir, name, UserFileRole::Secret)?.is_some()
            && !creating
        {
            return Err("TLS の一時ファイルが残っていますが、作成中の印がありません".into());
        }
    }
    if !complete {
        if !creating && (cert.is_some() || key.is_some()) {
            return Err("TLS の鍵と証明書が揃っていないか、組として合いません（作成中の印が無いので作り直しません）".into());
        }
        // 読み取りのハンドルを放してから、作成中と確認できたファイルだけを消す。
        drop((cert, key));
        if !creating {
            create(dir.dir(), CREATING, CREATING_CONTENTS)?;
        }
        cleanup(dir.dir(), &[KEY_FILE, CERT_FILE, KEY_TEMP, CERT_TEMP])?;
        let identity = generate_identity(now)?;
        create(dir.dir(), KEY_TEMP, identity.key_pem.as_bytes())?;
        create(dir.dir(), CERT_TEMP, identity.cert_pem.as_bytes())?;
        for (from, to) in [(KEY_TEMP, KEY_FILE), (CERT_TEMP, CERT_FILE)] {
            if !dir
                .dir()
                .managed()
                .rename_no_replace(from, to)
                .map_err(|err| err.to_string())?
            {
                return Err(format!("TLS の {from} を {to} に付け替えられませんでした"));
            }
        }
    }
    let (cert, key) = read_pair(ops, place, options, &dir)?;
    let cert = cert.ok_or("TLS の証明書がありません")?;
    let key = key.ok_or("TLS の鍵がありません")?;
    let identity = DefaultHubTls::from_checked_pem(
        DEFAULT_TLS_PORT,
        cert,
        key,
        dir.dir_path().join(CERT_FILE),
        now,
    )?;
    let thumbprint = CertThumbprint::of_pem(identity.cert_pem())?;
    let records = read_user_revocations(ops, place);
    if !records.is_settled() || records.is_revoked(&thumbprint) {
        return Err("利用者の失効の記録が揃っていないか、この証明書は失効しています".into());
    }
    cleanup(dir.dir(), &[KEY_TEMP, CERT_TEMP, CREATING])?;
    Ok(identity)
}

#[cfg(test)]
mod tests;
