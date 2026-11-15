//! デスクトップ既定 TLS の周期監視。記録のロックだけを取り、TLS は作成・変更しない。
//! 停止後も検査を続け、失効・露出以外なら同じメモリ上の組で復帰する。
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_hub_tls_trust::{read_user_revocations, reconcile_user_revocations};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::*;
use crate::tls_listener::{self, RunningTlsListener, TlsConfig, TlsRevoker};

const INTERVAL: Duration = Duration::from_secs(30);

/// PR3 の画面が参照する状態。理由は鍵の内容を含まない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopTlsStatus {
    /// 現在の待ち受け（停止中なら None）。
    pub address: Option<SocketAddr>,
    /// 最後の検査・bind の警告。正常なら None。
    pub reason: Option<String>,
    /// 露出・失効を確認したため作り直しが必要。
    pub requires_recreation: bool,
}

struct Snapshot {
    status: DesktopTlsStatus,
}

pub(crate) struct Monitor {
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    finish: Option<oneshot::Sender<bool>>,
    snapshot: Arc<Mutex<Snapshot>>,
    revoker: TlsRevoker,
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.signal_stop();
        if let Some(finish) = self.finish.take() {
            let _ = finish.send(false);
        }
    }
}
impl Monitor {
    pub(crate) fn status(&self) -> DesktopTlsStatus {
        let mut status = self
            .snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .status
            .clone();
        if self.revoker.is_revoked() {
            status.address = None;
            if !status.requires_recreation {
                status.reason = Some(external_stop_reason().into());
            }
        }
        status
    }
    pub(crate) fn revoker(&self) -> Option<TlsRevoker> {
        Some(self.revoker.clone())
    }
    /// shutdown の冒頭で新しい検査/再 bind を止める。listener は TLS の停止位置まで保持する。
    pub(crate) fn signal_stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
    /// TLS の通常の停止位置で graceful stop を依頼し、完了を待つ。
    /// この await のキャンセルでも task は閉じ切ってから profile 排他を離す。
    pub(crate) async fn stop(mut self) {
        self.signal_stop();
        if let Some(finish) = self.finish.take() {
            let _ = finish.send(true);
        }
        let _ = self.task.take().expect("monitor task").await;
    }
}

#[derive(Default)]
pub(super) struct Watch {
    incomplete: u8,
    permanently_revoked: bool,
    unwritten: BTreeSet<CertThumbprint>,
}
pub(super) struct Round {
    pub(super) allow: bool,
    reason: Option<String>,
}
impl Watch {
    fn decide(&mut self, outcome: ReconcileOutcome, invalid: bool, revoked: bool) -> bool {
        self.permanently_revoked |= revoked;
        self.incomplete = if outcome == ReconcileOutcome::Incomplete {
            self.incomplete.saturating_add(1)
        } else {
            0
        };
        !self.permanently_revoked
            && !invalid
            && outcome != ReconcileOutcome::Refused
            && self.incomplete < 3
    }

    pub(super) fn check<P: UserTlsPlace + ?Sized>(
        &mut self,
        place: &P,
        serving: CertThumbprint,
        options: &ReconcileOptions,
    ) -> Round {
        let ops = WindowsTlsTrustOps;
        // ロックを取れない場合も report のロック無しの読み口で失効を照合する。
        let report = reconcile_user_revocations(&ops, place, options);
        let mut revoked = report.revocations.is_revoked(&serving);
        let mut reason = (report.outcome != ReconcileOutcome::Consistent).then(|| {
            format!(
                "利用者の失効の記録 A・B: {:?}（{:?}）",
                report.outcome, report.warnings
            )
        });
        let mut invalid = false;
        let mut transient = false;
        let mut problems = Vec::new();
        let mut cert = None;
        let mut key = None;
        match open_user_place(&ops, place, UserPlaceKind::Tls) {
            Ok(UserPlaceState::Present(dir)) => {
                // 片方の欠損/失敗で他方の露出を見落とさない。
                for (name, role, target) in [
                    (CERT_FILE, UserFileRole::Public, &mut cert),
                    (KEY_FILE, UserFileRole::Secret, &mut key),
                ] {
                    match open_user_file(&ops, &dir, name, name, role) {
                        Ok(Some(mut file)) => match file.read_all(MAX_PEM_BYTES) {
                            Ok(bytes) => *target = Some(Zeroizing::new(bytes)),
                            Err(error) => {
                                match &error {
                                    ReadError::Other(_) => transient = true,
                                    ReadError::TooLarge => invalid = true,
                                }
                                reason = Some(format!("TLS の {name} を読めません: {error}"));
                            }
                        },
                        Ok(None) => {
                            invalid = true;
                            reason = Some(format!("TLS の {name} がありません"));
                        }
                        Err(found) => problems.push(found),
                    }
                }
            }
            Ok(UserPlaceState::Absent(_)) => {
                invalid = true;
                reason = Some("TLS の置き場がありません".into());
            }
            Err(found) => problems.push(found),
        }
        for found in problems {
            if found.fault.revokes() {
                self.unwritten.insert(serving);
                if let UncheckedUserCert::Seen(disk) = peek_user_cert(&ops, place) {
                    self.unwritten.insert(disk.thumbprint);
                }
                revoked = true;
            } else if found.fault == UserFault::Transient {
                transient = true;
            } else {
                invalid = true;
            }
            reason = Some(found.reason);
        }
        // 読めた証明書の不一致は、鍵の一時的な open/read 失敗とは独立に判定する。
        if let Some(cert) = &cert {
            if CertThumbprint::of_pem(cert).ok() != Some(serving) {
                invalid = true;
                reason = Some("TLS の証明書が正常でないか、起動時の証明書と一致しません".into());
            }
        }
        if let (Some(cert), Some(key)) = (&cert, &key) {
            if inspect_identity(cert, key, now_unix()).is_err() {
                invalid = true;
                reason =
                    Some("TLS の鍵・証明書が正常でないか、起動時の証明書と一致しません".into());
            }
        }
        revoked |= self.write_unwritten(place, serving, options);
        // reconcile の後で別の読み手が記録した失効も見る。
        revoked |= read_user_revocations(&ops, place).is_revoked(&serving);
        if revoked || self.permanently_revoked {
            reason = Some(format!(
                "証明書（拇印 {serving}）は露出・失効を確認したため使えません。{}",
                unavailable("").remedy
            ));
        }
        let outcome = if transient && report.outcome == ReconcileOutcome::Consistent {
            ReconcileOutcome::Incomplete
        } else {
            report.outcome
        };
        let allow = self.decide(outcome, invalid, revoked);
        Round { allow, reason }
    }
    /// 通常検査と panic 後の縮退経路が共有する、未書込拇印だけの再試行。
    fn write_unwritten<P: UserTlsPlace + ?Sized>(
        &mut self,
        place: &P,
        serving: CertThumbprint,
        options: &ReconcileOptions,
    ) -> bool {
        let mut revoked = false;
        for thumbprint in self.unwritten.clone() {
            let written = record_user_revocation(
                &WindowsTlsTrustOps,
                place,
                options,
                thumbprint,
                RevokedCertPlace::User,
            );
            if written.outcome == ReconcileOutcome::Consistent
                && written.revocations.is_settled()
                && written.revocations.is_revoked(&thumbprint)
            {
                self.unwritten.remove(&thumbprint);
            } else {
                crate::hub_log::log_err_line(&format!("[WARN] banto-hub: 露出を見た証明書（拇印 {thumbprint}）の失効を両方の記録に書けません。プロセス内に覚えて使わず、次の周期に再試行します: {:?}", written.warnings));
            }
            revoked |= written.revocations.is_revoked(&serving);
        }
        revoked
    }
}

pub(crate) fn spawn(
    listener: RunningTlsListener,
    config: TlsConfig,
    router: axum::Router,
    serving: CertThumbprint,
    keep_alive: Arc<Option<crate::profile_lock::ProfileLockGuard>>,
) -> Monitor {
    let task_guard = keep_alive.clone();
    spawn_with_retry(
        listener,
        config,
        router,
        move |watch| {
            let _keep_alive = &keep_alive;
            match DefaultUserTlsPlace::resolve(&WindowsTlsTrustOps) {
                Ok(place) => watch.check(&place, serving, &ReconcileOptions::default()),
                Err(error) => Round {
                    allow: watch.decide(ReconcileOutcome::Incomplete, false, false),
                    reason: Some(error.reason),
                },
            }
        },
        move |watch| match DefaultUserTlsPlace::resolve(&WindowsTlsTrustOps) {
            Ok(place) => {
                watch.write_unwritten(&place, serving, &ReconcileOptions::default());
            }
            Err(error) => crate::hub_log::log_err_line(&format!(
                "[WARN] banto-hub: panic 後の失効の記録の置き場を取得できません: {}",
                error.reason
            )),
        },
        MonitorSettings {
            interval: INTERVAL,
            serving,
        },
        task_guard,
    )
}

struct MonitorSettings {
    interval: Duration,
    serving: CertThumbprint,
}

#[cfg(test)]
fn spawn_with<F>(
    listener: RunningTlsListener,
    config: TlsConfig,
    router: axum::Router,
    check: F,
    settings: MonitorSettings,
    keep_alive: impl Send + 'static,
) -> Monitor
where
    F: FnMut(&mut Watch) -> Round + Send + 'static,
{
    spawn_with_retry(
        listener,
        config,
        router,
        check,
        |_| {},
        settings,
        keep_alive,
    )
}

fn spawn_with_retry<F, R>(
    listener: RunningTlsListener,
    config: TlsConfig,
    router: axum::Router,
    mut check: F,
    retry: R,
    settings: MonitorSettings,
    keep_alive: impl Send + 'static,
) -> Monitor
where
    F: FnMut(&mut Watch) -> Round + Send + 'static,
    R: FnMut(&mut Watch) + Send + 'static,
{
    let revoker = TlsRevoker::following(listener.revoker());
    let external = revoker.clone();
    let snapshot = Arc::new(Mutex::new(Snapshot {
        status: DesktopTlsStatus {
            address: Some(listener.local_addr()),
            reason: None,
            requires_recreation: false,
        },
    }));
    let state = snapshot.clone();
    let (stop, mut stopped) = oneshot::channel();
    let (finish, finished) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut listener = Some(listener);
        let mut watch = Watch::default();
        let mut ticks = tokio::time::interval(settings.interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await; // 初回の検査は起動時に済んでいる。
        loop {
            tokio::select! { biased;
                _ = &mut stopped => break,
                _ = ticks.tick() => {}
            }
            // await は中断しない。終了/プロファイル切替は検査の完了まで待つ。
            let checked = tokio::task::spawn_blocking(move || {
                // checker の panic payload は展開せず、更新済みの失効状態を必ず返す。
                let round = catch_unwind(AssertUnwindSafe(|| check(&mut watch))).ok();
                (watch, check, round)
            })
            .await;
            let mut round;
            match checked {
                Ok((next, next_check, Some(found))) => {
                    watch = next;
                    check = next_check;
                    round = found;
                }
                Ok((failed, failed_check, None)) => {
                    // panic した checker は再利用しない。TLS を閉じ切ってから書込専用へ縮退する。
                    drop(failed_check);
                    if failed.permanently_revoked || !failed.unwritten.is_empty() {
                        external.revoke();
                    }
                    if let Some(active) = listener.take() {
                        active.revoker().revoke();
                        active.stop().await;
                    }
                    let reason = panic_reason(&failed);
                    crate::hub_log::log_err_line(&format!("[WARN] banto-hub: {reason}"));
                    {
                        let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                        state.status = DesktopTlsStatus {
                            address: None,
                            reason: Some(reason),
                            requires_recreation: failed.permanently_revoked
                                || !failed.unwritten.is_empty(),
                        };
                    }
                    retry_after_panic(failed, retry, ticks, stopped, state.clone()).await;
                    drop(keep_alive);
                    return;
                }
                Err(_) => {
                    // パニックの payload に鍵が含まれる可能性があるので展開しない。
                    if let Some(active) = listener.take() {
                        active.revoker().revoke();
                        active.stop().await;
                    }
                    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                    state.status.address = None;
                    state.status.reason = Some(
                        "TLS の周期検査タスクが異常終了しました。Hub を再起動してください".into(),
                    );
                    crate::hub_log::log_err_line("[WARN] banto-hub: TLS の周期検査タスクが異常終了したため TLS を停止しました");
                    drop(keep_alive);
                    return;
                }
            }
            if watch.permanently_revoked {
                external.revoke();
            }
            if external.is_revoked() {
                round.allow = false;
                if !watch.permanently_revoked {
                    round.reason = Some(external_stop_reason().into());
                }
            }
            if !round.allow {
                if let Some(active) = listener.take() {
                    active.revoker().revoke();
                    active.stop().await; // abort 済み serve を join し、port を空け切る。
                }
            } else if listener.is_none() && round.reason.is_none() {
                // この周期で記録・置き場・組・拇印をすべて確認してから新しい listener を bind。
                // stop 通知が来ていれば再 bind しない。
                if stopped.try_recv() != Err(tokio::sync::oneshot::error::TryRecvError::Empty) {
                    break;
                }
                match tls_listener::start_following(&config, router.clone(), &external).await {
                    Ok(active) => {
                        if external.is_revoked() {
                            active.stop().await;
                            round.reason = Some(external_stop_reason().into());
                        } else {
                            crate::hub_log::log_line(&format!(
                                "banto-hub: 既定の TLS が復帰しました: https://{}（拇印 {}）",
                                active.local_addr(),
                                settings.serving
                            ));
                            listener = Some(active);
                        }
                    }
                    Err(error) => {
                        round.reason = Some(format!(
                            "TLS の復帰で待ち受けられません。次の周期に再試行します: {error}"
                        ));
                    }
                }
            }
            if let Some(reason) = &round.reason {
                crate::hub_log::log_err_line(&format!(
                    "[WARN] banto-hub: TLS の周期検査: {reason}"
                ));
            }
            let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
            state.status = DesktopTlsStatus {
                address: listener.as_ref().map(RunningTlsListener::local_addr),
                reason: round.reason,
                requires_recreation: watch.permanently_revoked,
            };
        }
        // 検査停止と listener 停止を分け、runtime の既存の停止順序を保つ。
        // 異常 drop / stop future のキャンセルでも、この task 自身が最後まで閉じる。
        let graceful = finished.await.unwrap_or(false);
        if let Some(active) = listener {
            if !graceful {
                active.revoker().revoke();
            }
            active.stop().await;
        }
        drop(check);
        drop(keep_alive);
    });
    Monitor {
        stop: Some(stop),
        task: Some(task),
        snapshot,
        finish: Some(finish),
        revoker,
    }
}

fn pending_thumbprints(watch: &Watch) -> String {
    watch
        .unwritten
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn panic_reason(watch: &Watch) -> String {
    let mut reason = "TLS の周期検査が panic したため、このプロセスでは TLS を再開しません。Hub を再起動する前にログを確認してください".to_string();
    if !watch.unwritten.is_empty() {
        reason.push_str(&format!(
            "。panic 時の未書込の失効拇印: {}。失効の記録への書き込みだけを次の周期に再試行します",
            pending_thumbprints(watch)
        ));
    }
    if watch.permanently_revoked || !watch.unwritten.is_empty() {
        reason.push_str(&format!("。{}", unavailable("").remedy));
    }
    reason
}

/// 既知の失効だけを書き直す。TLS の検査・復帰や panic した checker の再利用はしない。
async fn retry_after_panic<R>(
    mut watch: Watch,
    mut retry: R,
    mut ticks: tokio::time::Interval,
    mut stopped: oneshot::Receiver<()>,
    state: Arc<Mutex<Snapshot>>,
) where
    R: FnMut(&mut Watch) + Send + 'static,
{
    while !watch.unwritten.is_empty() {
        crate::hub_log::log_err_line(&format!(
            "[WARN] banto-hub: panic 後も未書込の失効拇印を保持しています: {}",
            pending_thumbprints(&watch)
        ));
        tokio::select! { biased;
            _ = &mut stopped => return,
            _ = ticks.tick() => {}
        }
        // 書込中の停止でも完了まで待ち、profile 排他を先に離さない。
        match tokio::task::spawn_blocking(move || {
            let succeeded = catch_unwind(AssertUnwindSafe(|| retry(&mut watch))).is_ok();
            (watch, retry, succeeded)
        })
        .await
        {
            Ok((next, next_retry, true)) => {
                watch = next;
                retry = next_retry;
            }
            Ok((next, _, false)) => {
                let note = format!(
                    "失効の書込専用タスクも panic したため再利用しません。残った未書込拇印: {}。{}",
                    pending_thumbprints(&next),
                    unavailable("").remedy
                );
                crate::hub_log::log_err_line(&format!("[WARN] banto-hub: {note}"));
                annotate_retry_status(&state, &note);
                return;
            }
            Err(_) => {
                // 通常の callback panic は上で捕捉する。タスク自体の異常は直前の全件ログを残す。
                let note = "失効の書込専用タスクが異常終了しました。直前の未書込拇印のログを確認してください";
                crate::hub_log::log_err_line(&format!("[WARN] banto-hub: {note}"));
                annotate_retry_status(&state, note);
                return;
            }
        }
    }
    let note = "panic 後に保持していた未書込の失効拇印をすべて記録しました。TLS は停止を保ちます";
    crate::hub_log::log_line(&format!("banto-hub: {note}"));
    annotate_retry_status(&state, note);
}

fn annotate_retry_status(state: &Mutex<Snapshot>, note: &str) {
    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
    let reason = state.status.reason.get_or_insert_with(String::new);
    reason.push_str("。現在の状態: ");
    reason.push_str(note);
}

fn external_stop_reason() -> &'static str {
    "外部から TLS の失効停止が要求されました。このプロセスでは再開しません"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_three_times_stops_and_consistent_restores() {
        let mut watch = Watch::default();
        assert!(watch.decide(ReconcileOutcome::Incomplete, false, false));
        assert!(watch.decide(ReconcileOutcome::Incomplete, false, false));
        assert!(!watch.decide(ReconcileOutcome::Incomplete, false, false));
        assert!(watch.decide(ReconcileOutcome::Consistent, false, false));
        assert!(watch.decide(ReconcileOutcome::Incomplete, false, false));
    }
    #[test]
    fn refused_or_invalid_stop_immediately_and_can_recover_but_revocation_cannot() {
        let mut watch = Watch::default();
        assert!(!watch.decide(ReconcileOutcome::Refused, false, false));
        assert!(watch.decide(ReconcileOutcome::Consistent, false, false));
        assert!(!watch.decide(ReconcileOutcome::Consistent, true, false));
        assert!(watch.decide(ReconcileOutcome::Consistent, false, false));
        assert!(!watch.decide(ReconcileOutcome::Incomplete, false, true));
        assert!(!watch.decide(ReconcileOutcome::Consistent, false, false));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn monitor_closes_connections_rebinds_and_joins_before_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::AsyncReadExt;
        let identity = generate_identity(now_unix()).unwrap();
        let config = TlsConfig::in_memory(
            0,
            identity.cert_pem.as_bytes().to_vec(),
            Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        );
        let router = axum::Router::new();
        let active = tls_listener::start(&config, router.clone()).await.unwrap();
        let first = active.local_addr();
        let config = TlsConfig::in_memory(
            first.port(),
            identity.cert_pem.as_bytes().to_vec(),
            Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        );
        let mut pending = tokio::net::TcpStream::connect(first).await.unwrap();
        let healthy = Arc::new(AtomicBool::new(false));
        let seen = healthy.clone();
        let monitor = spawn_with(
            active,
            config,
            router,
            move |_| Round {
                allow: seen.load(Ordering::SeqCst),
                reason: (!seen.load(Ordering::SeqCst)).then(|| "test stop".into()),
            },
            MonitorSettings {
                interval: Duration::from_millis(10),
                serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
            },
            (),
        );
        let bound = Duration::from_secs(10);
        tokio::time::timeout(bound, async {
            while monitor.status().address.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut byte = [0];
        let read = tokio::time::timeout(bound, pending.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
        assert!(tokio::net::TcpStream::connect(first).await.is_err());
        let blocker = std::net::TcpListener::bind(first).unwrap();
        healthy.store(true, Ordering::SeqCst);
        tokio::time::timeout(bound, async {
            while !monitor
                .status()
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("復帰で待ち受けられません"))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let failed = monitor.status();
        assert_eq!(failed.address, None);
        assert!(!failed.requires_recreation);
        assert!(
            failed
                .reason
                .as_ref()
                .unwrap()
                .contains("次の周期に再試行します"),
            "bind failure must publish the complete round reason: {failed:?}"
        );
        drop(blocker);
        tokio::time::timeout(bound, async {
            while monitor.status().address.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let restored = monitor.status().address.unwrap();
        assert_eq!(
            restored, first,
            "recovery must bind the original fixed TLS port"
        );
        monitor.stop().await;
        assert!(tokio::net::TcpStream::connect(restored).await.is_err());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_monitor_keeps_guard_through_inflight_check_and_listener_cleanup() {
        let identity = generate_identity(now_unix()).unwrap();
        let config = TlsConfig::in_memory(
            0,
            identity.cert_pem.as_bytes().to_vec(),
            Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        );
        let router = axum::Router::new();
        let active = tls_listener::start(&config, router.clone()).await.unwrap();
        let address = active.local_addr();
        let guard = Arc::new(());
        let weak = Arc::downgrade(&guard);
        let (entered, mut ready) = tokio::sync::mpsc::unbounded_channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let monitor = spawn_with(
            active,
            config,
            router,
            move |_| {
                let _held = &guard;
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Round {
                    allow: true,
                    reason: None,
                }
            },
            MonitorSettings {
                interval: Duration::from_millis(10),
                serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
            },
            (),
        );
        let bound = Duration::from_secs(10);
        tokio::time::timeout(bound, ready.recv())
            .await
            .unwrap()
            .unwrap();
        drop(monitor);
        assert!(
            weak.upgrade().is_some(),
            "in-flight check must keep profile guard"
        );
        release.send(()).unwrap();
        tokio::time::timeout(bound, async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::net::TcpStream::connect(address).await.is_err(),
            "listener must close before guard releases"
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_stop_still_closes_listener_before_releasing_guard() {
        let identity = generate_identity(now_unix()).unwrap();
        let config = TlsConfig::in_memory(
            0,
            identity.cert_pem.as_bytes().to_vec(),
            Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        );
        let router = axum::Router::new();
        let active = tls_listener::start(&config, router.clone()).await.unwrap();
        let address = active.local_addr();
        let guard = Arc::new(());
        let weak = Arc::downgrade(&guard);
        let (entered, mut ready) = tokio::sync::mpsc::unbounded_channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let monitor = spawn_with(
            active,
            config,
            router,
            move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Round {
                    allow: true,
                    reason: None,
                }
            },
            MonitorSettings {
                interval: Duration::from_millis(10),
                serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
            },
            guard,
        );
        let bound = Duration::from_secs(10);
        tokio::time::timeout(bound, ready.recv())
            .await
            .unwrap()
            .unwrap();
        {
            let mut stopping = Box::pin(monitor.stop());
            assert!(futures_util::poll!(&mut stopping).is_pending());
        }
        assert!(weak.upgrade().is_some());
        release.send(()).unwrap();
        tokio::time::timeout(bound, async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn external_revoker_stops_replacement_listener_and_prevents_future_rebind() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::io::AsyncReadExt;
        for revoke_while_stopped in [false, true] {
            let identity = generate_identity(now_unix()).unwrap();
            let config = TlsConfig::in_memory(
                0,
                identity.cert_pem.as_bytes().to_vec(),
                Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
            );
            let router = axum::Router::new();
            let active = tls_listener::start(&config, router.clone()).await.unwrap();
            let address = active.local_addr();
            let config = TlsConfig::in_memory(
                address.port(),
                identity.cert_pem.as_bytes().to_vec(),
                Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
            );
            let healthy = Arc::new(AtomicBool::new(false));
            let checks = Arc::new(AtomicUsize::new(0));
            let seen = healthy.clone();
            let counted = checks.clone();
            let monitor = spawn_with(
                active,
                config,
                router,
                move |_| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Round {
                        allow: seen.load(Ordering::SeqCst),
                        reason: (!seen.load(Ordering::SeqCst)).then(|| "test stop".into()),
                    }
                },
                MonitorSettings {
                    interval: Duration::from_millis(10),
                    serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
                },
                (),
            );
            let old = monitor.revoker().unwrap();
            let bound = Duration::from_secs(10);
            tokio::time::timeout(bound, async {
                while monitor.status().address.is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if revoke_while_stopped {
                old.revoke();
            }
            healthy.store(true, Ordering::SeqCst);
            if !revoke_while_stopped {
                tokio::time::timeout(bound, async {
                    while monitor.status().address.is_none() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let mut pending = tokio::net::TcpStream::connect(monitor.status().address.unwrap())
                    .await
                    .unwrap();
                old.revoke();
                let mut byte = [0];
                let read = tokio::time::timeout(Duration::from_secs(1), pending.read(&mut byte))
                    .await
                    .expect("old revoker must immediately sever replacement listener connections");
                assert!(matches!(read, Ok(0) | Err(_)));
            }
            assert!(old.is_revoked());
            let observed = checks.load(Ordering::SeqCst);
            tokio::time::timeout(bound, async {
                while checks.load(Ordering::SeqCst) < observed + 2 {
                    tokio::task::yield_now().await;
                }
                while monitor.status().address.is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(monitor.revoker().unwrap().is_revoked());
            assert!(
                tokio::net::TcpStream::connect(address).await.is_err(),
                "external revocation must keep the fixed port closed"
            );
            monitor.stop().await;
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watcher_revocation_marks_shared_handle_and_preserves_specific_reason() {
        let identity = generate_identity(now_unix()).unwrap();
        let config = TlsConfig::in_memory(
            0,
            identity.cert_pem.as_bytes().to_vec(),
            Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        );
        let router = axum::Router::new();
        let active = tls_listener::start(&config, router.clone()).await.unwrap();
        let monitor = spawn_with(
            active,
            config,
            router,
            |watch| {
                let allow = watch.decide(ReconcileOutcome::Consistent, false, true);
                Round {
                    allow,
                    reason: Some("specific exposure or revocation".into()),
                }
            },
            MonitorSettings {
                interval: Duration::from_millis(10),
                serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
            },
            (),
        );
        let original = monitor.revoker().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !monitor.status().requires_recreation {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(original.is_revoked());
        let status = monitor.status();
        assert_eq!(status.address, None);
        assert_eq!(
            status.reason.as_deref(),
            Some("specific exposure or revocation")
        );
        monitor.stop().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn panicking_check_preserves_all_unwritten_thumbprints_and_stops_tls() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let identity = generate_identity(now_unix()).unwrap();
        let serving = CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap();
        let disk = CertThumbprint::of_der(b"different certificate seen on disk");
        let config = TlsConfig::in_memory(
            0,
            identity.cert_pem.as_bytes().to_vec(),
            Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        );
        let router = axum::Router::new();
        let active = tls_listener::start(&config, router.clone()).await.unwrap();
        let address = active.local_addr();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let monitor = spawn_with(
            active,
            config,
            router,
            move |watch| {
                observed.fetch_add(1, Ordering::SeqCst);
                watch.unwritten.extend([serving, disk]);
                panic!("injected checker failure");
            },
            MonitorSettings {
                interval: Duration::from_millis(10),
                serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
            },
            (),
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while !monitor.status().requires_recreation {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let status = monitor.status();
        let reason = status.reason.unwrap();
        assert!(
            reason.contains(&serving.to_string()),
            "serving thumbprint lost: {reason}"
        );
        assert!(
            reason.contains(&disk.to_string()),
            "disk thumbprint lost: {reason}"
        );
        assert!(status.requires_recreation);
        assert!(monitor.revoker().unwrap().is_revoked());
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        monitor.stop().await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "panicking checker must not be reused"
        );
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn panic_degrades_to_write_only_retries_and_never_reuses_panicking_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for writer_panics in [false, true] {
            let identity = generate_identity(now_unix()).unwrap();
            let serving = CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap();
            let disk = CertThumbprint::of_der(b"disk certificate pending before panic");
            let config = TlsConfig::in_memory(
                0,
                identity.cert_pem.as_bytes().to_vec(),
                Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
            );
            let router = axum::Router::new();
            let active = tls_listener::start(&config, router.clone()).await.unwrap();
            let address = active.local_addr();
            let checks = Arc::new(AtomicUsize::new(0));
            let attempts = Arc::new(AtomicUsize::new(0));
            let written = Arc::new(Mutex::new(BTreeSet::new()));
            let checked = checks.clone();
            let retried = attempts.clone();
            let recorded = written.clone();
            let monitor = spawn_with_retry(
                active,
                config,
                router,
                move |watch| {
                    checked.fetch_add(1, Ordering::SeqCst);
                    watch.unwritten.extend([serving, disk]);
                    panic!("injected checker failure");
                },
                move |watch| {
                    let attempt = retried.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(
                        watch.unwritten,
                        BTreeSet::from([serving, disk]),
                        "both thumbprints must survive failed attempts"
                    );
                    assert!(!watch.unwritten.is_empty());
                    if writer_panics {
                        panic!("injected writer failure");
                    }
                    if attempt > 0 {
                        recorded
                            .lock()
                            .unwrap()
                            .extend(watch.unwritten.iter().copied());
                        watch.unwritten.clear();
                    }
                },
                MonitorSettings {
                    interval: Duration::from_millis(10),
                    serving: CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap(),
                },
                (),
            );
            tokio::time::timeout(Duration::from_secs(10), async {
                while !monitor.task.as_ref().unwrap().is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(checks.load(Ordering::SeqCst), 1);
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                if writer_panics { 1 } else { 2 }
            );
            if !writer_panics {
                assert_eq!(*written.lock().unwrap(), BTreeSet::from([serving, disk]));
            }
            let status = monitor.status();
            assert!(status.requires_recreation);
            assert!(status
                .reason
                .as_ref()
                .unwrap()
                .contains(&serving.to_string()));
            assert!(status.reason.as_ref().unwrap().contains(&disk.to_string()));
            assert!(status.reason.as_ref().unwrap().contains(if writer_panics {
                "書込専用タスクも panic"
            } else {
                "すべて記録しました"
            }));
            assert!(tokio::net::TcpStream::connect(address).await.is_err());
            monitor.stop().await;
        }
    }
}
