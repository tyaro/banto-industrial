//! プロセスの Redirection Guard を有効にする（#623 の (3b)。設計 §4.5）。
//!
//! `SetProcessMitigationPolicy(ProcessRedirectionTrustPolicy)` で
//! `EnforceRedirectionTrust` を立てると、このプロセスは**管理者でない利用者が作った
//! マウントポイント（ジャンクション）**を辿らなくなる（開くと失敗する）。SQLite・tstore・
//! banto の BackupService がパスで開き直すときの、途中の要素のジャンクションへの保険。
//!
//! - **効かないもの**（Fable B-4-3）: ハードリンク、ファイル・ディレクトリの**シンボリック
//!   リンク**（開発者モードの非管理者は作れる）。そこは crate の辿らない開き方と、運用モードの
//!   ACL（PR-1c）が守る。
//! - **プロセス全体**に効き、戻せない。管理者が作ったジャンクションは辿る（信頼される）が、
//!   非管理者が作った正当な構成（利用者が自分の D: のフォルダをジャンクションで指している等）は
//!   壊れる - 既知の制限。
//! - **対象の OS**: Windows 11 22H2 以降・Windows Server 2022 の更新以降。それより古い OS では
//!   `SetProcessMitigationPolicy` が失敗し、[`RedirectionGuard::Failed`] を返す（呼ぶ側はログに
//!   1 行出して続ける。保険なので、使えなくても起動は止めない）。
//!
//! 配線（サービスの起動のごく初めに呼ぶ）は PR-3（ChronoGazer）・PR-5（banto-hub）。

/// [`enable_redirection_guard`] の結果（Fable C-6）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedirectionGuard {
    /// 有効にした（`GetProcessMitigationPolicy` で読み戻して確かめた）。
    Enabled,
    /// 有効にできなかった（古い OS・ポリシーが反対向きに固定されている等）。`os_error` は
    /// `GetLastError` の値（読み戻しで有効になっていなかったときは 0）。
    Failed { os_error: u32 },
    /// Windows ではない。
    NotWindows,
}

/// このプロセスの Redirection Guard（強制）を有効にする（モジュール doc）。**戻せない**ので、
/// サービスのプロセスのごく初めに 1 回だけ呼ぶ。
pub fn enable_redirection_guard() -> RedirectionGuard {
    imp::enable()
}

/// このプロセスで Redirection Guard（強制）が有効か（`GetProcessMitigationPolicy` で読む）。
/// 読めなければ `None`。Windows でなければ `None`。
pub fn redirection_guard_enabled() -> Option<bool> {
    imp::enabled()
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn enable() -> super::RedirectionGuard {
        super::RedirectionGuard::NotWindows
    }

    pub(super) fn enabled() -> Option<bool> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::SystemServices::{
        PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY, PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY_0,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessMitigationPolicy, ProcessRedirectionTrustPolicy,
        SetProcessMitigationPolicy,
    };

    /// `EnforceRedirectionTrust`（ビット 0）。ビット 1 は `AuditRedirectionTrust`（監査だけ）。
    const ENFORCE_REDIRECTION_TRUST: u32 = 0x1;

    pub(super) fn enable() -> super::RedirectionGuard {
        let policy = PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY {
            Anonymous: PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY_0 {
                Flags: ENFORCE_REDIRECTION_TRUST,
            },
        };
        // SAFETY: `policy` は呼び出しの間生きている、大きさの合う構造体。
        let ok = unsafe {
            SetProcessMitigationPolicy(
                ProcessRedirectionTrustPolicy,
                (&policy as *const PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY).cast(),
                std::mem::size_of::<PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY>(),
            )
        };
        if ok == 0 {
            return super::RedirectionGuard::Failed {
                os_error: crate::acl::imp::last_error(),
            };
        }
        // 成功を返しても効いていない組み合わせが無いよう、読み戻して確かめる。
        match enabled() {
            Some(true) => super::RedirectionGuard::Enabled,
            _ => super::RedirectionGuard::Failed { os_error: 0 },
        }
    }

    pub(super) fn enabled() -> Option<bool> {
        let mut policy = PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY {
            Anonymous: PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY_0 { Flags: 0 },
        };
        // SAFETY: 今のプロセスの疑似ハンドル。`policy` は大きさの合う書き込み先。
        let ok = unsafe {
            GetProcessMitigationPolicy(
                GetCurrentProcess(),
                ProcessRedirectionTrustPolicy,
                (&mut policy as *mut PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY).cast(),
                std::mem::size_of::<PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY>(),
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: 共用体の中身はどちらも 32 ビットの整数（`Flags` で読んでよい）。
        let flags = unsafe { policy.Anonymous.Flags };
        Some(flags & ENFORCE_REDIRECTION_TRUST != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 子プロセスの印（この環境変数があるときだけ、子として実際に有効にする）。
    const CHILD_ENV: &str = "BANTO_SAFE_ROOT_RG_CHILD";

    /// 子プロセスの本体: 親が `CHILD_ENV` を付けて、このテストだけを走らせる。戻せないので、
    /// テストのプロセス（cargo test はほかのテストと同じプロセス）では有効にしない。
    #[test]
    fn redirection_guard_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return; // 親から呼ばれたときだけ働く。
        }
        let result = enable_redirection_guard();
        println!(
            "RG-RESULT={result:?} ENABLED={:?}",
            redirection_guard_enabled()
        );
    }

    /// `enable_redirection_guard` を子プロセスで呼び、有効になって読み戻せることを確かめる
    /// （Windows 11 22H2+ 前提。古い OS では `Failed` を受け入れる）。非 Windows は `NotWindows`。
    #[test]
    fn enabling_the_redirection_guard_reads_back_in_a_child_process() {
        if cfg!(not(windows)) {
            assert_eq!(enable_redirection_guard(), RedirectionGuard::NotWindows);
            assert_eq!(redirection_guard_enabled(), None);
            return;
        }
        // このプロセスはまだ有効にしていない（子に閉じている）。古い OS では読めない（None）。
        assert_ne!(redirection_guard_enabled(), Some(true));
        let exe = std::env::current_exe().unwrap();
        let output = std::process::Command::new(exe)
            .args([
                "redirection::tests::redirection_guard_child",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("子プロセスを起動できない");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "子が失敗した: {output:?}");
        let line = stdout
            .lines()
            .find(|l| l.contains("RG-RESULT="))
            .unwrap_or_else(|| panic!("子の出力に結果が無い: {stdout}"));
        if line.contains("RG-RESULT=Enabled") {
            assert!(line.contains("ENABLED=Some(true)"), "{line}");
        } else {
            // 古い OS では失敗を返すだけで、パニックしない。CI（Windows Server 2022/2025 の
            // ランナー）で何が返るかはログで見る。
            assert!(line.contains("RG-RESULT=Failed"), "{line}");
            eprintln!("note: この OS では Redirection Guard を有効にできない: {line}");
        }
        // 親は変わっていない。
        assert_ne!(redirection_guard_enabled(), Some(true));
    }
}
