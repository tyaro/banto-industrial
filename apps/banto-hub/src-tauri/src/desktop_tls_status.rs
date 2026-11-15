//! Display state is explicit: absence of observed exposure does not imply that
//! a failed monitor can resume. The same presentation feeds UI and tray.
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Presentation {
    pub label: String,
    pub reason: Option<String>,
    pub remedy: String,
}

#[cfg(windows)]
pub(crate) fn present(
    status: &banto_hub_core::hub_tls_prepare::desktop::watch::DesktopTlsStatus,
) -> Presentation {
    let (label, remedy) = if status.address.is_some() {
        ("ChronoGazer 接続用 TLS: 待ち受け中", "")
    } else if status.requires_recreation {
        ("ChronoGazer 接続用 TLS: 作り直しが必要", "理由を確認して、トレイから TLS の証明書を作り直してください。置き場が安全でない場合は Hub を閉じて所有者・アクセス許可を修復してください。失効の記録は消さないでください")
    } else if status.can_resume {
        (
            "ChronoGazer 接続用 TLS: 一時停止中",
            "原因を取り除くと次の周期の検査で自動復帰します",
        )
    } else {
        ("ChronoGazer 接続用 TLS: この起動では再開できません", "理由とログを確認し、原因を取り除いて Hub を再起動してください。露出が疑われる場合は鍵・証明書を作り直し、失効の記録は消さないでください")
    };
    Presentation {
        label: label.into(),
        reason: status.reason.clone(),
        remedy: remedy.into(),
    }
}

pub(crate) fn current(state: &crate::AppState) -> Option<Presentation> {
    #[cfg(windows)]
    {
        state
            .hub
            .inspect(|hub| hub.desktop_tls_status().map(|status| present(&status)))
            .flatten()
    }
    #[cfg(not(windows))]
    {
        let _ = state;
        None
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use banto_hub_core::hub_tls_prepare::desktop::watch::DesktopTlsStatus;
    #[test]
    fn temporary_pause_and_panic_have_different_recovery_without_reason_parsing() {
        let mut status = DesktopTlsStatus {
            address: None,
            reason: Some("same reason".into()),
            requires_recreation: false,
            can_resume: true,
        };
        let temporary = present(&status);
        assert!(temporary.remedy.contains("自動復帰"));
        status.can_resume = false;
        let permanent = present(&status);
        assert!(permanent.label.contains("再開できません"));
        assert!(permanent.remedy.contains("再起動"));
        assert_ne!(temporary, permanent);
        status.requires_recreation = true;
        assert!(present(&status).label.contains("作り直し"));
    }
}
