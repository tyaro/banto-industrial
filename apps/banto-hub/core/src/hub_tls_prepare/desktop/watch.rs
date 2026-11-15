//! デスクトップ既定 TLS の周期監視。記録のロックだけを取り、TLS は作成・変更しない。
//! 停止後も検査を続け、失効・露出以外なら同じメモリ上の組で復帰する。
use std::collections::BTreeSet;
use std::net::SocketAddr;
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
    revoker: Option<TlsRevoker>,
}

pub(crate) struct Monitor {
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    finish: Option<oneshot::Sender<bool>>,
    snapshot: Arc<Mutex<Snapshot>>,
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
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .status
            .clone()
    }
    pub(crate) fn revoker(&self) -> Option<TlsRevoker> {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revoker
            .clone()
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
                                transient |= matches!(error, ReadError::Other(_));
                                invalid |= matches!(error, ReadError::TooLarge);
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
        if let (Some(cert), Some(key)) = (&cert, &key) {
            if CertThumbprint::of_pem(cert).ok() != Some(serving)
                || inspect_identity(cert, key, now_unix()).is_err()
            {
                invalid = true;
                reason =
                    Some("TLS の鍵・証明書が正常でないか、起動時の証明書と一致しません".into());
            }
        }
        for thumbprint in self.unwritten.clone() {
            let written =
                record_user_revocation(&ops, place, options, thumbprint, RevokedCertPlace::User);
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
}

pub(crate) fn spawn(
    listener: RunningTlsListener,
    config: TlsConfig,
    router: axum::Router,
    serving: CertThumbprint,
    keep_alive: Arc<Option<crate::profile_lock::ProfileLockGuard>>,
) -> Monitor {
    let task_guard = keep_alive.clone();
    spawn_with(
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
        INTERVAL,
        task_guard,
    )
}

fn spawn_with<F>(
    listener: RunningTlsListener,
    config: TlsConfig,
    router: axum::Router,
    mut check: F,
    interval: Duration,
    keep_alive: impl Send + 'static,
) -> Monitor
where
    F: FnMut(&mut Watch) -> Round + Send + 'static,
{
    let snapshot = Arc::new(Mutex::new(Snapshot {
        status: DesktopTlsStatus {
            address: Some(listener.local_addr()),
            reason: None,
            requires_recreation: false,
        },
        revoker: Some(listener.revoker()),
    }));
    let state = snapshot.clone();
    let (stop, mut stopped) = oneshot::channel();
    let (finish, finished) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut listener = Some(listener);
        let mut watch = Watch::default();
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await; // 初回の検査は起動時に済んでいる。
        loop {
            tokio::select! { biased;
                _ = &mut stopped => break,
                _ = ticks.tick() => {}
            }
            // await は中断しない。終了/プロファイル切替は検査の完了まで待つ。
            let checked = tokio::task::spawn_blocking(move || {
                let round = check(&mut watch);
                (watch, check, round)
            })
            .await;
            let round;
            match checked {
                Ok((next, next_check, found)) => {
                    watch = next;
                    check = next_check;
                    round = found;
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
                    state.revoker = None;
                    crate::hub_log::log_err_line("[WARN] banto-hub: TLS の周期検査タスクが異常終了したため TLS を停止しました");
                    drop(keep_alive);
                    return;
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
                match tls_listener::start(&config, router.clone()).await {
                    Ok(active) => listener = Some(active),
                    Err(error) => {
                        crate::hub_log::log_err_line(&format!("[WARN] banto-hub: TLS の復帰で待ち受けられません。次の周期に再試行します: {error}"));
                        let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                        state.status.reason =
                            Some(format!("TLS の復帰で待ち受けられません: {error}"));
                        continue;
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
            state.revoker = listener.as_ref().map(RunningTlsListener::revoker);
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
    }
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
            Duration::from_millis(10),
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
        healthy.store(true, Ordering::SeqCst);
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
            Duration::from_millis(10),
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
            Duration::from_millis(10),
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
}
