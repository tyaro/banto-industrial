//! 実 Windows の通常の利用者で、protected DACL・排他 open・作成/復旧を確かめる。
//! 実ユーザーの LOCALAPPDATA は触らない。4 つの置き場すべてを専用の一時 anchor に差し替える。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use banto_hub_tls_trust::{
    RevocationSide, TlsTrustOps, UserFault, UserPlaceSpec, UserTlsOps, REVOCATIONS_FILE,
};
use banto_safe_root::acl::{inspect_file, ADMINISTRATORS_SID, SYSTEM_SID};

use super::*;

static NONCE: AtomicU64 = AtomicU64::new(0);
const NOW: i64 = 1_795_000_000;

#[derive(Clone)]
struct Place(PathBuf);
impl UserTlsPlace for Place {
    fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec {
        let mut spec = DefaultUserTlsPlace::new(
            self.0.clone(),
            WindowsTlsTrustOps.current_user_sid().unwrap(),
        )
        .spec(kind);
        spec.policy.anchor_is_local_app_data = false;
        spec
    }
}
impl Place {
    fn tls(&self) -> PathBuf {
        self.spec(UserPlaceKind::Tls).path()
    }
    fn load(&self) -> Result<DefaultHubTls, String> {
        load_in(&WindowsTlsTrustOps, self, &ReconcileOptions::default(), NOW)
    }
    fn dir(&self) -> CheckedUserPlace<WinTlsDir> {
        tls_place(&WindowsTlsTrustOps, self, &ReconcileOptions::default()).unwrap()
    }
    fn initialize_records(&self) {
        settled(&WindowsTlsTrustOps, self, &ReconcileOptions::default()).unwrap();
    }
    fn put(&self, name: &str, bytes: &[u8]) {
        create(self.dir().dir(), name, bytes).unwrap();
    }
}

struct Fixture(Place);
impl Fixture {
    fn new() -> Self {
        let exe = std::env::current_exe().unwrap();
        let parent_path = exe.parent().unwrap();
        let parent = WindowsTlsTrustOps.pin_dir(parent_path).unwrap().unwrap();
        let name = format!(
            "desktop-tls-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let made = parent
            .pinned()
            .create_child_dir_current_user(&name)
            .unwrap()
            .unwrap();
        let path = made.final_path().to_path_buf();
        drop(made);
        Self(Place(path))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0 .0).expect("専用の TLS 一時フォルダを片付ける");
    }
}
fn acl(path: &Path, args: &[&str]) {
    let output = Command::new("icacls.exe")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
fn thumb(cert: &DefaultHubTls) -> CertThumbprint {
    CertThumbprint::of_pem(cert.cert_pem()).unwrap()
}

#[test]
fn configured_tls_test_mode_and_other_hosts_never_load_user_place() {
    use crate::profile_lock::HubHostKind::{Console, Service, Shell};
    for (host, skip_lock, configured) in [
        (Shell, true, false),
        (Shell, false, true),
        (Console, false, false),
        (Service, false, false),
    ] {
        let result: Option<()> = select_default(host, skip_lock, configured, || {
            panic!("置き場を触ってはいけない")
        });
        assert!(result.is_none());
    }
    assert_eq!(select_default(Shell, false, false, || 8723), Some(8723));
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_tls_load_runs_off_the_async_worker() {
    use crate::profile_lock::HubHostKind::Shell;
    let worker = std::thread::current().id();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let release_task = tokio::spawn(async move {
        ready.await.unwrap();
        tokio::task::yield_now().await;
        release.send(()).unwrap();
    });
    let found = load_for_shell_with(Shell, false, false, (), move || {
        assert_ne!(worker, std::thread::current().id());
        started.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        Err(unavailable("injected TLS failure"))
    })
    .await
    .unwrap()
    .unwrap_err();
    release_task.await.unwrap();
    assert_eq!(found.reason, "injected TLS failure");
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_loader_panic_becomes_tls_unavailable_without_releasing_profile_guard() {
    use crate::profile_lock::HubHostKind::{Console, Service, Shell};
    let keep_alive = std::sync::Arc::new(None::<crate::profile_lock::ProfileLockGuard>);
    for (host, skip, configured) in [
        (Shell, true, false),
        (Shell, false, true),
        (Console, false, false),
        (Service, false, false),
    ] {
        assert!(
            load_for_shell_with(host, skip, configured, keep_alive.clone(), || panic!(
                "must not load"
            ))
            .await
            .is_none()
        );
        assert_eq!(std::sync::Arc::strong_count(&keep_alive), 1);
    }
    let found = load_for_shell_with(Shell, false, false, keep_alive.clone(), || {
        panic!("private panic payload")
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(std::sync::Arc::strong_count(&keep_alive), 1);
    assert!(found.reason.contains("準備タスクが異常終了"));
    let lines = found.log_lines().join("\n");
    assert!(lines.contains("TLS 無しで起動"));
    assert!(!lines.contains("private panic payload"));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_the_start_wait_keeps_the_real_profile_locked_until_load_finishes() {
    use crate::profile_lock::{try_acquire_profile_lock, HubHostKind::Shell};
    let fixture = Fixture::new();
    let profile = format!(
        "tls-cancel-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    );
    let paths = crate::profile_paths::resolve_profile_paths(&fixture.0 .0, &profile).unwrap();
    let guard = std::sync::Arc::new(Some(try_acquire_profile_lock(&paths, Shell).unwrap()));
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let pending = tokio::spawn(load_for_shell_with(
        Shell,
        false,
        false,
        guard.clone(),
        move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            Err(unavailable("test finished"))
        },
    ));
    drop(guard);
    ready.await.unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert!(
        try_acquire_profile_lock(&paths, Shell).is_err(),
        "blocking load が終わる前に排他を離さない"
    );
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(again) = try_acquire_profile_lock(&paths, Shell) {
            drop(again);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "blocking load 終了後は排他が返る"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn unavailable_remedy_explains_safe_recreation_without_deleting_revocations() {
    let found = unavailable("broken pair");
    let close = found.remedy.find("Hub を閉じ").unwrap();
    let inspect = found.remedy.find("中身を確認").unwrap();
    let delete = found.remedy.find("フォルダごと削除").unwrap();
    let restart = found.remedy.find("再起動").unwrap();
    assert!(close < inspect && inspect < delete && delete < restart);
    assert!(found.remedy.contains(r"%LOCALAPPDATA%\BantoHub\tls"));
    assert!(found.remedy.contains("親の置き場"));
    assert!(found.remedy.contains("失効の記録は消さない"));
    assert!(found
        .remedy
        .contains("ACL を直すだけでは漏れた鍵は無効になりません"));
}

#[test]
fn file_and_place_faults_use_the_callers_revocation_lock_options() {
    for place_fault in [false, true] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        place.load().unwrap();
        let spec = place.spec(UserPlaceKind::RevocationsLock);
        let parent = WindowsTlsTrustOps
            .pin_dir(&spec.dir_path())
            .unwrap()
            .unwrap();
        let held = WindowsTlsTrustOps
            .open_child_file_exclusive(&parent, spec.file.as_deref().unwrap())
            .unwrap()
            .unwrap();
        let options = ReconcileOptions {
            lock_wait: Duration::from_millis(70),
            lock_poll: Duration::from_millis(5),
        };
        let reason = if place_fault {
            acl(&place.tls(), &["/grant", "*S-1-1-0:(OI)(CI)(M)"]);
            let started = Instant::now();
            let reason = match tls_place(&WindowsTlsTrustOps, place, &options) {
                Ok(_) => panic!("通らない置き場"),
                Err(reason) => reason,
            };
            assert!(
                started.elapsed() >= options.lock_wait
                    && started.elapsed() < Duration::from_secs(2)
            );
            reason
        } else {
            acl(
                &place.tls().join(KEY_FILE),
                &["/grant", "*S-1-5-32-545:(R)"],
            );
            let dir = place.dir();
            let started = Instant::now();
            let reason = match checked_identity_file(
                &WindowsTlsTrustOps,
                place,
                &options,
                &dir,
                KEY_FILE,
                UserFileRole::Secret,
            ) {
                Ok(_) => panic!("通らない鍵"),
                Err(reason) => reason,
            };
            assert!(
                started.elapsed() >= options.lock_wait
                    && started.elapsed() < Duration::from_secs(2)
            );
            reason
        };
        assert!(reason.contains("失効の記録に残せませんでした"), "{reason}");
        drop(held);
        drop(parent);
    }
}

#[test]
fn creates_records_before_identity_and_reuses_exact_pair_with_protected_acl() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    // 3 者以外への継承する読み取りが起点にあっても、鍵へ届かない。
    acl(&place.0, &["/grant", "*S-1-5-32-545:(OI)(CI)(RX)"]);
    let first = place.load().unwrap();
    assert_eq!(first.port(), 8723);
    assert!(read_user_revocations(&WindowsTlsTrustOps, place).is_settled());
    assert!(!place.tls().join(CREATING).exists());
    let key = std::fs::read(place.tls().join(KEY_FILE)).unwrap();
    let second = place.load().unwrap();
    assert_eq!(thumb(&first), thumb(&second));
    assert_eq!(key, std::fs::read(place.tls().join(KEY_FILE)).unwrap());
    let me = WindowsTlsTrustOps.current_user_sid().unwrap();
    for path in [place.tls(), place.tls().join(KEY_FILE)] {
        let security = inspect_file(&path).unwrap().unwrap();
        assert!(security.dacl.protected, "{security:?}");
        assert!(user_place_problems(&security, std::slice::from_ref(&me), false).is_empty());
        let aces = security.dacl.aces.unwrap();
        assert!(aces
            .iter()
            .all(|ace| [me.as_str(), SYSTEM_SID, ADMINISTRATORS_SID].contains(&ace.sid.as_str())));
        assert_eq!(aces.len(), 3);
    }
}

#[test]
fn interrupted_creation_recovers_partial_mismatched_and_complete_pairs() {
    for state in 0..4 {
        let fixture = Fixture::new();
        let place = &fixture.0;
        place.initialize_records();
        place.put(CREATING, CREATING_CONTENTS);
        let first = generate_identity(NOW).unwrap();
        match state {
            0 => {
                place.put(KEY_FILE, first.key_pem.as_bytes());
            }
            1 => {
                place.put(CERT_FILE, first.cert_pem.as_bytes());
            }
            2 => {
                place.put(CERT_FILE, first.cert_pem.as_bytes());
                place.put(KEY_FILE, generate_identity(NOW).unwrap().key_pem.as_bytes());
                place.put(KEY_TEMP, b"interrupted temporary file");
            }
            _ => {
                place.put(CERT_FILE, first.cert_pem.as_bytes());
                place.put(KEY_FILE, first.key_pem.as_bytes());
                place.put(CERT_TEMP, b"old temporary file");
            }
        }
        let loaded = place.load().unwrap();
        if state == 3 {
            assert_eq!(loaded.cert_pem(), first.cert_pem.as_bytes());
        } else {
            assert_ne!(loaded.cert_pem(), first.cert_pem.as_bytes());
        }
        for name in [CREATING, KEY_TEMP, CERT_TEMP] {
            assert!(!place.tls().join(name).exists());
        }
    }
}

#[test]
fn without_marker_partial_malformed_or_temporary_files_are_untouched() {
    for name in [KEY_FILE, CERT_FILE, KEY_TEMP, CERT_TEMP] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        place.initialize_records();
        place.put(name, b"broken");
        assert!(place.load().is_err());
        assert_eq!(std::fs::read(place.tls().join(name)).unwrap(), b"broken");
        assert!(!place.tls().join(CREATING).exists());
        assert!(!place
            .tls()
            .join(if name == KEY_FILE {
                CERT_FILE
            } else {
                KEY_FILE
            })
            .exists());
    }
}

#[test]
fn untrusted_anchor_or_record_place_never_creates_identity() {
    for target in [
        None,
        Some(UserPlaceKind::RevocationsA),
        Some(UserPlaceKind::RevocationsB),
    ] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        let target = match target {
            None => place.0.clone(),
            Some(kind) => {
                place.initialize_records();
                place.spec(kind).path()
            }
        };
        acl(&target, &["/grant", "*S-1-1-0:(OI)(CI)(M)"]);
        assert!(place.load().is_err());
        assert!(!place.tls().join(KEY_FILE).exists());
        assert!(!place.tls().join(CERT_FILE).exists());
    }
}

#[test]
fn missing_record_is_repaired_on_start_but_existing_certificate_stays_revoked() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    let cert = place.load().unwrap();
    std::fs::remove_file(
        place
            .spec(UserPlaceKind::RevocationsA)
            .path()
            .join(REVOCATIONS_FILE),
    )
    .unwrap();
    assert!(place.load().unwrap_err().contains("失効"));
    let records = read_user_revocations(&WindowsTlsTrustOps, place);
    assert!(records.is_settled());
    assert!(records.is_revoked(&thumb(&cert)));
    assert!(matches!(
        records.side(RevocationSide::A),
        banto_hub_tls_trust::SideContent::Record(_)
    ));
}

#[test]
fn exposure_is_recorded_and_acl_repair_does_not_restore_identity() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    let cert = place.load().unwrap();
    let path = place.tls().join(KEY_FILE);
    acl(&path, &["/grant", "*S-1-5-32-545:(R)"]);
    assert!(place.load().unwrap_err().contains("失効の記録"));
    assert!(read_user_revocations(&WindowsTlsTrustOps, place).is_revoked(&thumb(&cert)));
    acl(&path, &["/remove:g", "*S-1-5-32-545"]);
    assert!(place.load().unwrap_err().contains("失効"));
}

#[test]
fn unrecordable_exposure_warns_about_restart_and_requires_recreating_tls() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    let cert = place.load().unwrap();
    let spec = place.spec(UserPlaceKind::RevocationsLock);
    let parent = WindowsTlsTrustOps
        .pin_dir(&spec.dir_path())
        .unwrap()
        .unwrap();
    let held = WindowsTlsTrustOps
        .open_child_file_exclusive(&parent, spec.file.as_deref().unwrap())
        .unwrap()
        .unwrap();
    acl(&place.tls(), &["/grant", "*S-1-1-0:(OI)(CI)(M)"]);
    let options = ReconcileOptions {
        lock_wait: Duration::from_millis(70),
        lock_poll: Duration::from_millis(5),
    };
    let reason = load_in(&WindowsTlsTrustOps, place, &options, NOW).unwrap_err();
    assert!(reason.contains("失効の記録に残せませんでした"), "{reason}");
    assert!(
        reason.contains("今回の起動ではこの証明書を使いません"),
        "{reason}"
    );
    assert!(
        reason.contains("再起動すると同じ証明書が使われることがあります"),
        "{reason}"
    );
    assert!(
        reason.contains("ACL を直すだけでは漏れた鍵は無効になりません"),
        "{reason}"
    );
    assert!(reason.contains("フォルダごと削除"), "{reason}");
    assert!(!read_user_revocations(&WindowsTlsTrustOps, place).is_revoked(&thumb(&cert)));
    drop(held);
    drop(parent);
    acl(&place.tls(), &["/remove:g", "*S-1-1-0"]);
    // §7 の受容済みの限界を隠さない。永続の記録が無ければ、ACL を直すだけでは同じ組を
    // 再び使いうる。そのためログは、その操作だけを復旧として案内してはいけない。
    assert_eq!(thumb(&place.load().unwrap()), thumb(&cert));
    std::fs::remove_dir_all(place.tls()).unwrap();
    let renewed = place.load().unwrap();
    assert_ne!(thumb(&renewed), thumb(&cert));
    assert!(read_user_revocations(&WindowsTlsTrustOps, place).is_settled());
}

#[test]
fn untrusted_auxiliary_files_do_not_revoke_the_valid_identity() {
    for name in [CREATING, KEY_TEMP, CERT_TEMP] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        let cert = place.load().unwrap();
        place.put(
            name,
            if name == CREATING {
                CREATING_CONTENTS
            } else {
                b"auxiliary"
            },
        );
        let auxiliary = place.tls().join(name);
        acl(&auxiliary, &["/grant", "*S-1-1-0:(M)"]);
        assert!(place.load().is_err());
        assert!(
            !read_user_revocations(&WindowsTlsTrustOps, place).is_revoked(&thumb(&cert)),
            "{name} の問題で本体を失効させない"
        );
        assert!(auxiliary.exists(), "問題の補助ファイルを自動では消さない");
        std::fs::remove_file(&auxiliary).unwrap();
        assert_eq!(thumb(&place.load().unwrap()), thumb(&cert));
    }
}

#[test]
fn auxiliary_failure_does_not_hide_exposure_of_the_identity() {
    for name in [CREATING, KEY_TEMP, CERT_TEMP] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        let cert = place.load().unwrap();
        place.put(
            name,
            if name == CREATING {
                CREATING_CONTENTS
            } else {
                b"auxiliary"
            },
        );
        acl(&place.tls().join(name), &["/grant", "*S-1-1-0:(M)"]);
        acl(
            &place.tls().join(KEY_FILE),
            &["/grant", "*S-1-5-32-545:(R)"],
        );
        assert!(place.load().is_err());
        assert!(
            read_user_revocations(&WindowsTlsTrustOps, place).is_revoked(&thumb(&cert)),
            "{name} の問題で本体の露出を隠さない"
        );
        assert!(place.tls().join(name).exists(), "問題の補助ファイルは残す");
        assert_eq!(
            std::fs::read(place.tls().join(CERT_FILE)).unwrap(),
            cert.cert_pem()
        );
    }
}

#[test]
fn marked_complete_pair_does_not_delete_untrusted_temp_and_expiry_does_not_renew() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    let cert = place.load().unwrap();
    place.put(CREATING, CREATING_CONTENTS);
    let outside = place.0.join("unrelated");
    std::fs::write(&outside, b"keep").unwrap();
    std::fs::hard_link(&outside, place.tls().join(KEY_TEMP)).unwrap();
    assert!(place.load().is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    assert!(place.tls().join(KEY_TEMP).exists());
    std::fs::remove_file(place.tls().join(KEY_TEMP)).unwrap();
    let error = load_in(
        &WindowsTlsTrustOps,
        place,
        &ReconcileOptions::default(),
        cert.info().not_after_unix + 1,
    )
    .unwrap_err();
    assert!(error.contains("期限"), "{error}");
    assert_eq!(
        std::fs::read(place.tls().join(CERT_FILE)).unwrap(),
        cert.cert_pem()
    );
}

#[test]
fn identity_and_record_locks_wait_then_fail_without_creating_key() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    let dir = place.dir();
    let held = lock(&WindowsTlsTrustOps, &dir, Duration::from_secs(1)).unwrap();
    let options = ReconcileOptions {
        lock_wait: Duration::from_millis(70),
        lock_poll: Duration::from_millis(10),
    };
    let started = Instant::now();
    assert!(load_in(&WindowsTlsTrustOps, place, &options, NOW)
        .unwrap_err()
        .contains("ロック"));
    assert!(started.elapsed() >= options.lock_wait);
    assert!(!place.tls().join(KEY_FILE).exists());
    drop(held);
    drop(dir);
    place.initialize_records();
    let lock_spec = place.spec(UserPlaceKind::RevocationsLock);
    let parent = WindowsTlsTrustOps
        .pin_dir(&lock_spec.dir_path())
        .unwrap()
        .unwrap();
    let held = WindowsTlsTrustOps
        .open_child_file_exclusive(&parent, lock_spec.file.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert!(load_in(&WindowsTlsTrustOps, place, &options, NOW)
        .unwrap_err()
        .contains("確定できません"));
    assert!(!place.tls().join(KEY_FILE).exists());
    drop(held);
    drop(parent);
    assert!(place.load().is_ok());
}

#[test]
fn broken_record_without_certificate_is_repaired_before_creation() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    place.initialize_records();
    std::fs::write(
        place
            .spec(UserPlaceKind::RevocationsB)
            .path()
            .join(REVOCATIONS_FILE),
        b"broken",
    )
    .unwrap();
    let cert = place.load().unwrap();
    let records = read_user_revocations(&WindowsTlsTrustOps, place);
    assert!(records.is_settled());
    assert!(!records.is_revoked(&thumb(&cert)));
}

#[test]
fn malformed_creation_marker_preserves_the_pair_and_marker() {
    let fixture = Fixture::new();
    let place = &fixture.0;
    let cert = place.load().unwrap();
    place.put(CREATING, b"broken");
    assert!(place.load().unwrap_err().contains("印の形"));
    assert_eq!(
        std::fs::read(place.tls().join(CREATING)).unwrap(),
        b"broken"
    );
    assert_eq!(
        std::fs::read(place.tls().join(CERT_FILE)).unwrap(),
        cert.cert_pem()
    );
}

#[test]
fn owner_fault_revokes_but_other_untrusted_faults_do_not() {
    for fault in [
        UserFault::ForeignOwner,
        UserFault::Untrusted,
        UserFault::Broken,
        UserFault::Transient,
    ] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        let cert = place.load().unwrap();
        // 実際の所有者の書き換えには特権が要る。分類そのものは共有 crate の偽物の表で確かめ、
        // ここでは受け取った分類が実ファイルの記録へ反映されることを確かめる。
        problem(
            &WindowsTlsTrustOps,
            place,
            &ReconcileOptions::default(),
            UserPlaceProblem {
                reason: "test fault".into(),
                remedy: banto_hub_tls_trust::UserRemedy::Renew,
                fault,
            },
        );
        assert_eq!(
            read_user_revocations(&WindowsTlsTrustOps, place).is_revoked(&thumb(&cert)),
            fault == UserFault::ForeignOwner
        );
    }
}

struct ChildGuard(Option<std::process::Child>);
impl ChildGuard {
    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn output(mut self) -> std::process::Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn two_processes_create_one_identity_and_interrupted_process_releases_lock() {
    for interrupted in [false, true] {
        let fixture = Fixture::new();
        let place = &fixture.0;
        let spawn = |mode: &str| {
            ChildGuard(Some(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "hub_tls_prepare::desktop::tests::creation_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("BANTO_TEST_TLS_ANCHOR", &place.0)
                    .env("BANTO_TEST_TLS_MODE", mode)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            ))
        };
        if interrupted {
            let mut first = spawn("lock");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !place.0.join("locked").exists() {
                assert!(Instant::now() < deadline, "子プロセスのロック待ち");
                std::thread::sleep(Duration::from_millis(10));
            }
            // OS がプロセス終了でロックを放すこと、印のある部分的な組だけを復旧すること。
            first.stop();
            assert!(place.tls().join(CREATING).exists());
        }
        let a = spawn("create");
        let b = spawn("create");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read_dir(&place.0)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("ready-")
            })
            .count()
            != 2
        {
            assert!(
                Instant::now() < deadline,
                "子プロセスが両方起動するまで待つ"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::write(place.0.join("go"), b"").unwrap();
        let a = a.output();
        let b = b.output();
        for output in [&a, &b] {
            assert!(
                output.status.success(),
                "stdout: {} stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let fingerprint = |output: &[u8]| {
            String::from_utf8_lossy(output)
                .lines()
                .find(|line| line.starts_with("CERT "))
                .unwrap()
                .to_owned()
        };
        assert_eq!(fingerprint(&a.stdout), fingerprint(&b.stdout));
        let cert = place.load().unwrap();
        assert_eq!(fingerprint(&a.stdout), format!("CERT {}", thumb(&cert)));
    }
}

#[test]
#[ignore = "親のテストから専用の一時置き場を渡して実行"]
fn creation_child() {
    let place = Place(PathBuf::from(
        std::env::var_os("BANTO_TEST_TLS_ANCHOR").expect("親からの置き場"),
    ));
    if std::env::var("BANTO_TEST_TLS_MODE").unwrap() == "lock" {
        let dir = place.dir();
        let _held = lock(&WindowsTlsTrustOps, &dir, Duration::from_secs(1)).unwrap();
        place.initialize_records();
        create(dir.dir(), CREATING, CREATING_CONTENTS).unwrap();
        create(dir.dir(), KEY_FILE, b"interrupted").unwrap();
        std::fs::write(place.0.join("locked"), b"").unwrap();
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("親プロセスが子を終了しませんでした");
    }
    std::fs::write(place.0.join(format!("ready-{}", std::process::id())), b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !place.0.join("go").exists() {
        assert!(Instant::now() < deadline, "親が同時作成を始めるまで待つ");
        std::thread::sleep(Duration::from_millis(10));
    }
    let cert = place.load().unwrap();
    println!("CERT {}", thumb(&cert));
}
