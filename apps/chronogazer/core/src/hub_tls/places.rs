//! #1121 PR4a: Hub の証明書の 2 つの置き場と失効の記録を読む口（[`HubTlsPlaces`]）。
//!
//! 判定（`super` の `Pinned::evaluate`）は OS を触らず、この口が返した「置き場ごとの結果」だけで
//! 決める。本番の実装（[`SystemPlaces`]）は `banto_hub_tls_trust` の確かめをそのまま呼ぶだけで、
//! 確かめの手順はここに持たない（Hub と同じコードを使う。設計 §4.1・§4.4）。テストは偽物
//! （`super::fake`）に差し替える。
//!
//! | 口                              | 読むもの                                   | 書くか                   |
//! | ------------------------------- | ------------------------------------------ | ------------------------ |
//! | [`HubTlsPlaces::service_cert`]  | サービスの `tls\` の証明書                 | 書かない                 |
//! | [`HubTlsPlaces::service_revocations`] | サービスの失効の記録                 | 書かない                 |
//! | [`HubTlsPlaces::user_cert`]     | 利用者の `tls\` の証明書（鍵の露出も見る） | 書かない                 |
//! | [`HubTlsPlaces::user_revocations`] | 利用者の失効の記録 A・B の和（ロック無し） | 書かない              |
//! | [`HubTlsPlaces::reconcile_user_revocations`] | 利用者の記録を整える（ロックを待つ） | 記録だけ書く |
//! | [`HubTlsPlaces::record_user_revocation`] | 利用者の記録に拇印を足す（ロックを待つ） | 記録だけ書く |
//!
//! 下の 2 つはロックを上限付き（既定 5 秒）で待つので、1 回の操作ごとの判定からは呼ばない
//! （呼ぶのは見張りの周期と起動の初回だけ。`super` のモジュール doc「いつ読むか」）。
//! `chronogazer-server` の決まり（`HubTlsPolicy::service_only`）は上の 2 つしか呼ばない。

use std::collections::BTreeSet;

use banto_hub_tls_trust::{CertThumbprint, ReconcileOutcome, RevokedCertPlace};

/// 置き場の証明書を読んだ結果。「無い」と「あるが通らない」を分ける（設計 §4.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CertRead {
    /// 無い（置き場か証明書がまだ作られていない）。黙って外す。
    Absent,
    /// 確かめに通った証明書（PEM のバイト列）。Windows 以外の本番の実装は作らない（試験の偽物だけ）。
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    Usable(Vec<u8>),
    /// あるが通らない。pin に入れず、接続できても警告を出す。
    Unusable(UnusableCert),
}

/// 「あるが通らない」証明書。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnusableCert {
    /// 何が合わなかったか（表示用。鍵の中身は含まない）。
    pub reason: String,
    /// 直し方（Hub の動かし方ごとの文言。無ければ空）。
    pub remedy: String,
    /// 露出・所有者の違いを見て、形が読めた証明書の拇印。`Some` なら失効に入れる
    /// （ACL が直っても二度と使わない。設計 §4.4「露出を見た証明書は二度と使わない」）。
    pub revoke: Option<CertThumbprint>,
}

/// サービスの失効の記録を読んだ結果（「無い」は空）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceRecordRead {
    /// 確かめに通って読めた拇印（記録が無ければ空）。
    Revoked(BTreeSet<CertThumbprint>),
    /// 使えない（置き場が通らない・形が壊れている・読めなかった）。サービスの証明書を
    /// 使わない（設計 §4.4.1「サービスの記録」）。Windows 以外の本番の実装は作らない。
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
    Unusable {
        /// 理由（表示用）。
        reason: String,
        /// 形が壊れていても行ごとに読めた拇印。
        salvaged: BTreeSet<CertThumbprint>,
    },
}

/// 利用者の記録を整える・書き足す手順の結果（`banto_hub_tls_trust::ReconcileReport` の、判定に
/// 要る分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReconcileRead {
    /// 整っている・通らない・この回はできなかった。
    pub outcome: ReconcileOutcome,
    /// この回に消失を見たか（直せたかどうかに依らない）。
    pub saw_loss: bool,
    /// 警告（表示用）。
    pub warnings: Vec<String>,
    /// 手順の後に読み直した記録の拇印（A・B の和）。
    pub revoked: BTreeSet<CertThumbprint>,
}

/// Hub の証明書の置き場と失効の記録を読む口（モジュール doc の表）。
pub(crate) trait HubTlsPlaces: Send + Sync {
    /// サービスの置き場の証明書。
    fn service_cert(&self) -> CertRead;
    /// サービスの失効の記録。
    fn service_revocations(&self) -> ServiceRecordRead;
    /// 利用者の置き場の証明書。
    fn user_cert(&self) -> CertRead;
    /// 利用者の失効の記録 A・B の和（ロック無し。確かめに通らない置き場のものも、形が読めた分は
    /// 入る）。
    fn user_revocations(&self) -> BTreeSet<CertThumbprint>;
    /// 利用者の記録を整える（ロックを上限付きで待つ）。
    fn reconcile_user_revocations(&self) -> ReconcileRead;
    /// 利用者の記録に `thumbprint` を足す（ロックを上限付きで待つ）。書けたかどうかは、返った
    /// [`ReconcileRead::revoked`] に拇印があるかで分かる。
    fn record_user_revocation(
        &self,
        thumbprint: CertThumbprint,
        place: RevokedCertPlace,
    ) -> ReconcileRead;
}

/// #644 PR3b の読み口（サービスの置き場の証明書 1 枚）。**テスト用**に残してある
/// （`HubTlsPolicy::with_reader`）: 証明書の PEM か、使えない理由を返す。
#[cfg(test)]
pub(crate) trait HubCertReader: Send + Sync {
    fn read_cert_pem(&self) -> Result<Vec<u8>, String>;
}

/// [`HubCertReader`] を [`HubTlsPlaces`] に合わせる（サービスの置き場だけ。記録は空）。
#[cfg(test)]
pub(super) struct ReaderPlaces(pub(super) std::sync::Arc<dyn HubCertReader>);

#[cfg(test)]
impl HubTlsPlaces for ReaderPlaces {
    fn service_cert(&self) -> CertRead {
        match self.0.read_cert_pem() {
            Ok(pem) => CertRead::Usable(pem),
            Err(reason) => CertRead::Unusable(UnusableCert {
                reason,
                remedy: String::new(),
                revoke: None,
            }),
        }
    }

    fn service_revocations(&self) -> ServiceRecordRead {
        ServiceRecordRead::Revoked(BTreeSet::new())
    }

    fn user_cert(&self) -> CertRead {
        CertRead::Absent
    }

    fn user_revocations(&self) -> BTreeSet<CertThumbprint> {
        BTreeSet::new()
    }

    fn reconcile_user_revocations(&self) -> ReconcileRead {
        consistent_and_empty()
    }

    fn record_user_revocation(
        &self,
        _thumbprint: CertThumbprint,
        _place: RevokedCertPlace,
    ) -> ReconcileRead {
        consistent_and_empty()
    }
}

#[cfg(any(test, not(windows)))]
fn consistent_and_empty() -> ReconcileRead {
    ReconcileRead {
        outcome: ReconcileOutcome::Consistent,
        saw_loss: false,
        warnings: Vec::new(),
        revoked: BTreeSet::new(),
    }
}

/// 本番の読み方（モジュール doc）。
pub(super) struct SystemPlaces;

#[cfg(windows)]
mod system {
    use banto_hub_tls_trust::win::WindowsTlsTrustOps;
    use banto_hub_tls_trust::{
        DefaultUserTlsPlace, ReconcileOptions, ReconcileReport, Remedy, RevocationSide,
        RevocationWarning, UserCertPin, UserPlaceProblem, UserRemedy,
    };

    use super::*;

    /// Hub のサービスの置き場を直す文言（管理者の操作。`banto-hub` のシェルの
    /// `service_tls::remedy_text` と揃える）。ChronoGazer は毎回読み直すので、pin を書き換える
    /// 手順は要らない。
    pub(super) fn remedy_text(remedy: &Remedy) -> String {
        const PREPARE: &str = "banto-hub-elev.exe prepare-hub-tls";
        match remedy {
            Remedy::Retry => "原因を取り除いてからやり直してください。続くときは、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行して置き場を確かめてください".to_owned(),
            Remedy::Prepare => format!(
                "Hub をサービスで動かし、管理者の PowerShell で {PREPARE} を実行してから、Hub のサービスを再起動してください"
            ),
            Remedy::FixRoot => "Hub の置き場の所有者を Administrators にし、SYSTEM・Administrators 以外への書き込み・削除・ACL と所有者の変更の許可を外してください".to_owned(),
            Remedy::RecreateDir(tls_path) => format!(
                "{} の中身を確かめてこのフォルダを消し、管理者の PowerShell で {PREPARE} を実行してから、Hub のサービスを再起動してください",
                tls_path.display()
            ),
            Remedy::Renew => format!(
                "管理者の PowerShell で {PREPARE} --renew を実行して作り直し、Hub のサービスを再起動してください"
            ),
        }
    }

    /// 利用者の置き場（Hub のデスクトップ版）を直す文言。Hub のシェルの案内
    /// （`desktop_tls_status`・`hub_tls_prepare::desktop`）と同じ手順を指す: 作り直しは
    /// トレイの「TLS の証明書を作り直す」、フォルダの異常は中身を確かめて `tls\` ごと消す。
    /// 失効の記録は消さない。
    pub(super) fn user_remedy_text(remedy: &UserRemedy) -> String {
        match remedy {
            UserRemedy::Retry => "原因を取り除いてください（次の確かめで読み直します）。続くときは、Hub のデスクトップ版の画面とログで理由を確かめてください".to_owned(),
            UserRemedy::FixAnchor(path) => format!(
                "{} の場所・所有者・アクセス許可を確かめ、利用者本人・SYSTEM・Administrators 以外への書き込み・削除・ACL と所有者の変更の許可を外してください",
                path.display()
            ),
            UserRemedy::RecreateDir(path) => format!(
                "Hub のデスクトップ版を閉じ、{} の中身・所有者・アクセス許可を確かめてこのフォルダごと消してから、Hub のデスクトップ版を起動し直してください（失効の記録は消さないでください）",
                path.display()
            ),
            UserRemedy::Renew => "Hub のデスクトップ版で、TLS フォルダと鍵・証明書の所有者・アクセス許可を確かめ、異常があれば直してから、トレイの「TLS の証明書を作り直す」で作り直してください（失効の記録は消さないでください）".to_owned(),
        }
    }

    fn side_name(side: RevocationSide) -> &'static str {
        match side {
            RevocationSide::A => "A",
            RevocationSide::B => "B",
        }
    }

    /// 整える手順の警告の文言。
    fn warning_text(warning: &RevocationWarning) -> String {
        match warning {
            RevocationWarning::Layout(reason) => {
                format!("利用者の失効の記録の置き場の指定が使えません: {reason}")
            }
            RevocationWarning::LockRefused(problem) => format!(
                "利用者の失効の記録のロックファイルが確かめに通らないため、何も書いていません: {}。{}",
                problem.reason,
                user_remedy_text(&problem.remedy)
            ),
            RevocationWarning::LockTimeout(waited) => format!(
                "利用者の失効の記録のロックを {} 秒待っても取れませんでした（ほかのプロセスが握っています。次の周期でやり直します）",
                waited.as_secs()
            ),
            RevocationWarning::Lost { side, reason } => format!(
                "利用者の失効の記録 {} が消された・壊れたのを見ました: {reason}",
                side_name(*side)
            ),
            RevocationWarning::ExistingCertRevoked(thumbprint) => format!(
                "失効の記録を作り直したため、すでにあった利用者の証明書（拇印 {thumbprint}）を失効に入れました。Hub のデスクトップ版のトレイの「TLS の証明書を作り直す」で作り直してください"
            ),
            RevocationWarning::NotRepaired { side, reason } => format!(
                "利用者の失効の記録 {} の置き場が確かめに通らないため、直していません: {reason}",
                side_name(*side)
            ),
            RevocationWarning::Io(reason) => {
                format!("利用者の失効の記録の読み書きが終わりませんでした: {reason}")
            }
        }
    }

    fn user_place() -> Result<DefaultUserTlsPlace, UserPlaceProblem> {
        DefaultUserTlsPlace::resolve(&WindowsTlsTrustOps)
    }

    fn report_of(report: ReconcileReport) -> ReconcileRead {
        ReconcileRead {
            outcome: report.outcome,
            saw_loss: report.saw_loss(),
            warnings: report.warnings.iter().map(warning_text).collect(),
            revoked: report
                .revocations
                .entries()
                .map(|entry| entry.thumbprint)
                .collect(),
        }
    }

    /// 置き場の場所（`%LOCALAPPDATA%`・利用者の SID）を取れなかった回。状態を見たのではなく、
    /// 操作が終わらなかっただけ。
    fn unresolved(problem: UserPlaceProblem) -> ReconcileRead {
        ReconcileRead {
            outcome: ReconcileOutcome::Incomplete,
            saw_loss: false,
            warnings: vec![format!(
                "利用者の失効の記録の置き場を決められませんでした: {}",
                problem.reason
            )],
            revoked: BTreeSet::new(),
        }
    }

    impl HubTlsPlaces for SystemPlaces {
        fn service_cert(&self) -> CertRead {
            let ops = WindowsTlsTrustOps;
            match banto_hub_tls_trust::load_pinned_hub_cert(&ops) {
                Ok(pin) => CertRead::Usable(pin.cert_pem),
                // ルート・`tls\`・証明書のどれかがまだ無い（デスクトップ版だけの PC では普通）。
                Err(found) if found.remedy == Remedy::Prepare => CertRead::Absent,
                Err(found) => CertRead::Unusable(UnusableCert {
                    remedy: remedy_text(&found.remedy),
                    // 露出・所有者の違いなら、ディスク上の証明書の拇印を形だけ読んで失効に
                    // 入れる（拇印が増えるだけなので、安全側にしか動かない）。
                    revoke: found
                        .fault
                        .revokes()
                        .then(|| banto_hub_tls_trust::peek_service_cert(&ops))
                        .flatten(),
                    reason: found.reason,
                }),
            }
        }

        fn service_revocations(&self) -> ServiceRecordRead {
            let thumbprints = |entries: &[banto_hub_tls_trust::RevokedCert]| {
                entries.iter().map(|entry| entry.thumbprint).collect()
            };
            match banto_hub_tls_trust::read_service_revocations(&WindowsTlsTrustOps) {
                Ok(record) => ServiceRecordRead::Revoked(thumbprints(record.entries())),
                Err(problem) => ServiceRecordRead::Unusable {
                    salvaged: thumbprints(&problem.salvaged),
                    reason: problem.reason,
                },
            }
        }

        fn user_cert(&self) -> CertRead {
            let place = match user_place() {
                Ok(place) => place,
                Err(problem) => {
                    return CertRead::Unusable(UnusableCert {
                        remedy: user_remedy_text(&problem.remedy),
                        reason: problem.reason,
                        revoke: None,
                    })
                }
            };
            match banto_hub_tls_trust::load_pinned_user_cert(&WindowsTlsTrustOps, &place) {
                UserCertPin::Absent => CertRead::Absent,
                UserCertPin::Usable(pin) => CertRead::Usable(pin.cert_pem),
                UserCertPin::Unusable(found) => CertRead::Unusable(UnusableCert {
                    revoke: found.revocation_thumbprint(),
                    remedy: user_remedy_text(&found.problem.remedy),
                    reason: found.problem.reason,
                }),
            }
        }

        fn user_revocations(&self) -> BTreeSet<CertThumbprint> {
            // 場所を決められない回は空（利用者の証明書もその回は「通らない」なので使わない。
            // サービスの証明書の照合はサービスの記録で続く）。
            let Ok(place) = user_place() else {
                return BTreeSet::new();
            };
            banto_hub_tls_trust::read_user_revocations(&WindowsTlsTrustOps, &place)
                .entries()
                .map(|entry| entry.thumbprint)
                .collect()
        }

        fn reconcile_user_revocations(&self) -> ReconcileRead {
            match user_place() {
                Ok(place) => report_of(banto_hub_tls_trust::reconcile_user_revocations(
                    &WindowsTlsTrustOps,
                    &place,
                    &ReconcileOptions::default(),
                )),
                Err(problem) => unresolved(problem),
            }
        }

        fn record_user_revocation(
            &self,
            thumbprint: CertThumbprint,
            place: RevokedCertPlace,
        ) -> ReconcileRead {
            match user_place() {
                Ok(user) => report_of(banto_hub_tls_trust::record_user_revocation(
                    &WindowsTlsTrustOps,
                    &user,
                    &ReconcileOptions::default(),
                    thumbprint,
                    place,
                )),
                Err(problem) => unresolved(problem),
            }
        }
    }
}

/// Windows 以外では Hub の置き場を読めない（Hub のサービス・デスクトップ版の TLS は Windows
/// 専用）。サービスの置き場を「通らない」にして理由を出し、利用者の置き場は「無い」。
#[cfg(not(windows))]
impl HubTlsPlaces for SystemPlaces {
    fn service_cert(&self) -> CertRead {
        CertRead::Unusable(UnusableCert {
            reason: "この OS では同じ PC の Hub の証明書を読めないため、https の loopback には接続できません（Hub の TLS は Windows 専用です）".to_owned(),
            remedy: String::new(),
            revoke: None,
        })
    }

    fn service_revocations(&self) -> ServiceRecordRead {
        ServiceRecordRead::Revoked(BTreeSet::new())
    }

    fn user_cert(&self) -> CertRead {
        CertRead::Absent
    }

    fn user_revocations(&self) -> BTreeSet<CertThumbprint> {
        BTreeSet::new()
    }

    fn reconcile_user_revocations(&self) -> ReconcileRead {
        consistent_and_empty()
    }

    fn record_user_revocation(
        &self,
        _thumbprint: CertThumbprint,
        _place: RevokedCertPlace,
    ) -> ReconcileRead {
        consistent_and_empty()
    }
}
