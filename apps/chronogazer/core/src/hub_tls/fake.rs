//! テスト用の偽の置き場（[`HubTlsPlaces`]）。サービス・利用者の置き場の証明書と 2 つの失効の
//! 記録、整える手順の結果を試験が書き換え、どの口が何回呼ばれたかを数える。`hub_tls` の単体
//! 試験と、`crate::hub` の場面試験（`hub/tests/squat_scenarios.rs`）が使う。

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use banto_hub_tls_trust::{CertThumbprint, ReconcileOutcome, RevokedCertPlace};

use super::places::{
    CertRead, HubTlsPlaces, ReconcileRead, ServiceRecordRead, UnusableCert, UserRecordRead,
};
use super::HubTlsPolicy;

/// 口ごとの呼ばれた回数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Calls {
    pub service_cert: usize,
    pub service_record: usize,
    pub user_cert: usize,
    pub user_record: usize,
    pub reconcile: usize,
    /// 書き足しを頼まれた拇印と置き場（頼まれた順）。
    pub record: Vec<(CertThumbprint, RevokedCertPlace)>,
}

struct Inner {
    service: CertRead,
    service_record: ServiceRecordRead,
    user: CertRead,
    user_revoked: BTreeSet<CertThumbprint>,
    /// ロック無しの読みで、記録 A・B が整って見えるか。
    user_record_settled: bool,
    /// 整える手順が結果を読んだ後、返す前に呼ぶ（試験が待たせる・記録を書き換える）。引数は
    /// 何回目の整える手順か（1 始まり）。
    after_reconcile_read: Option<ReconcileHook>,
    /// 利用者の記録を読んだ後（読んだ値を返す前）に呼ぶ口。引数は何回目の読みか（1 始まり）。
    after_user_record_read: Option<ReconcileHook>,
    /// 次の整える手順の結果（空なら `reconcile_outcome` で警告なし）。
    scripted: VecDeque<(ReconcileOutcome, bool, Vec<String>)>,
    reconcile_outcome: ReconcileOutcome,
    record_fails: bool,
    calls: Calls,
}

type ReconcileHook = Arc<dyn Fn(usize) + Send + Sync>;

pub(crate) struct FakePlaces(Mutex<Inner>);

/// 証明書（PEM）の拇印。
pub(crate) fn thumbprint(pem: &str) -> CertThumbprint {
    CertThumbprint::of_pem(pem.as_bytes()).expect("a certificate")
}

/// 確かめに通った証明書。
pub(crate) fn usable(pem: &str) -> CertRead {
    CertRead::Usable(pem.as_bytes().to_vec())
}

/// あるが通らない（露出ではない）。
pub(crate) fn unusable(reason: &str, remedy: &str) -> CertRead {
    CertRead::Unusable(UnusableCert {
        reason: reason.to_owned(),
        remedy: remedy.to_owned(),
        revoke: None,
    })
}

/// 露出・所有者の違いを見た（形が読めた証明書は `pem`）。
pub(crate) fn exposed(reason: &str, pem: &str) -> CertRead {
    CertRead::Unusable(UnusableCert {
        reason: reason.to_owned(),
        remedy: String::new(),
        revoke: Some(thumbprint(pem)),
    })
}

impl FakePlaces {
    /// どちらの置き場も無く、記録は空で、整える手順は「整っている」。
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Inner {
            service: CertRead::Absent,
            service_record: ServiceRecordRead::Revoked(BTreeSet::new()),
            user: CertRead::Absent,
            user_revoked: BTreeSet::new(),
            user_record_settled: true,
            after_reconcile_read: None,
            after_user_record_read: None,
            scripted: VecDeque::new(),
            reconcile_outcome: ReconcileOutcome::Consistent,
            record_fails: false,
            calls: Calls::default(),
        })))
    }

    /// ChronoGazer デスクトップの決まり（`HubTlsPolicy::production()` と同じ判定）を、この偽物で。
    pub(crate) fn desktop_policy(self: &Arc<Self>) -> HubTlsPolicy {
        HubTlsPolicy::two_places_with(Arc::clone(self) as Arc<dyn HubTlsPlaces>)
    }

    /// `chronogazer-server` の決まり（`HubTlsPolicy::service_only()` と同じ判定）を、この偽物で。
    pub(crate) fn server_policy(self: &Arc<Self>) -> HubTlsPolicy {
        HubTlsPolicy::service_only_with(Arc::clone(self) as Arc<dyn HubTlsPlaces>)
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap()
    }

    pub(crate) fn set_service(&self, cert: CertRead) {
        self.inner().service = cert;
    }

    pub(crate) fn set_user(&self, cert: CertRead) {
        self.inner().user = cert;
    }

    pub(crate) fn set_service_record(&self, record: ServiceRecordRead) {
        self.inner().service_record = record;
    }

    /// サービスの記録に拇印を足す（サービスの Hub が露出を見た）。
    pub(crate) fn revoke_in_service_record(&self, thumbprint: CertThumbprint) {
        let mut inner = self.inner();
        let mut revoked = match &inner.service_record {
            ServiceRecordRead::Revoked(revoked) => revoked.clone(),
            ServiceRecordRead::Unusable { .. } => BTreeSet::new(),
        };
        revoked.insert(thumbprint);
        inner.service_record = ServiceRecordRead::Revoked(revoked);
    }

    /// 利用者の記録に拇印を足す（Hub のデスクトップ版が露出を見た）。
    pub(crate) fn revoke_in_user_record(&self, thumbprint: CertThumbprint) {
        self.inner().user_revoked.insert(thumbprint);
    }

    /// 利用者の記録 A・B を消す（拇印が無くなり、ロック無しの読みでは「整っていない」）。
    pub(crate) fn wipe_user_record(&self) {
        let mut inner = self.inner();
        inner.user_revoked.clear();
        inner.user_record_settled = false;
    }

    /// ロック無しの読みで、記録 A・B が整って見えるか（既定は見える）。
    pub(crate) fn set_user_record_settled(&self, settled: bool) {
        self.inner().user_record_settled = settled;
    }

    /// 整える手順が結果を読んだ**後**、返す前に呼ぶ口（偽物のロックの外で呼ぶ）。試験が
    /// ここで待たせると、「先に読んだ結果を後から返す」回を作れる。引数は何回目か（1 始まり）。
    pub(crate) fn after_reconcile_read(&self, hook: impl Fn(usize) + Send + Sync + 'static) {
        self.inner().after_reconcile_read = Some(Arc::new(hook));
    }

    /// 判定が利用者の記録を読んだ**後**、返す前に呼ぶ口（偽物のロックの外で呼ぶ）。試験が
    /// ここで待たせると、「古い記録を読んだ判定が止まっている」間を作れる。
    pub(crate) fn after_user_record_read(&self, hook: impl Fn(usize) + Send + Sync + 'static) {
        self.inner().after_user_record_read = Some(Arc::new(hook));
    }

    pub(crate) fn user_revoked(&self) -> BTreeSet<CertThumbprint> {
        self.inner().user_revoked.clone()
    }

    /// 以後の整える手順の結果（警告なし）。
    pub(crate) fn set_reconcile(&self, outcome: ReconcileOutcome) {
        self.inner().reconcile_outcome = outcome;
    }

    /// 次の 1 回だけの整える手順の結果（消失を見たか・警告つき）。
    pub(crate) fn push_reconcile(&self, outcome: ReconcileOutcome, saw_loss: bool, warning: &str) {
        self.inner()
            .scripted
            .push_back((outcome, saw_loss, vec![warning.to_owned()]));
    }

    /// 書き足しを失敗させる（記録に残らない）。
    pub(crate) fn fail_records(&self, fails: bool) {
        self.inner().record_fails = fails;
    }

    pub(crate) fn calls(&self) -> Calls {
        self.inner().calls.clone()
    }
}

impl HubTlsPlaces for FakePlaces {
    fn service_cert(&self) -> CertRead {
        let mut inner = self.inner();
        inner.calls.service_cert += 1;
        inner.service.clone()
    }

    fn service_revocations(&self) -> ServiceRecordRead {
        let mut inner = self.inner();
        inner.calls.service_record += 1;
        inner.service_record.clone()
    }

    fn user_cert(&self) -> CertRead {
        let mut inner = self.inner();
        inner.calls.user_cert += 1;
        inner.user.clone()
    }

    fn user_revocations(&self) -> UserRecordRead {
        let mut inner = self.inner();
        inner.calls.user_record += 1;
        let read = UserRecordRead {
            revoked: inner.user_revoked.clone(),
            settled: inner.user_record_settled,
        };
        let (hook, nth) = (
            inner.after_user_record_read.clone(),
            inner.calls.user_record,
        );
        drop(inner);
        if let Some(hook) = hook {
            hook(nth);
        }
        read
    }

    fn reconcile_user_revocations(&self) -> ReconcileRead {
        let mut inner = self.inner();
        inner.calls.reconcile += 1;
        let (outcome, saw_loss, warnings) =
            inner
                .scripted
                .pop_front()
                .unwrap_or((inner.reconcile_outcome, false, Vec::new()));
        let read = ReconcileRead {
            outcome,
            saw_loss,
            warnings,
            revoked: inner.user_revoked.clone(),
        };
        let (hook, nth) = (inner.after_reconcile_read.clone(), inner.calls.reconcile);
        drop(inner);
        if let Some(hook) = hook {
            hook(nth);
        }
        read
    }

    fn record_user_revocation(
        &self,
        thumbprint: CertThumbprint,
        place: RevokedCertPlace,
    ) -> ReconcileRead {
        let mut inner = self.inner();
        inner.calls.record.push((thumbprint, place));
        if !inner.record_fails {
            inner.user_revoked.insert(thumbprint);
        }
        ReconcileRead {
            outcome: if inner.record_fails {
                ReconcileOutcome::Incomplete
            } else {
                ReconcileOutcome::Consistent
            },
            saw_loss: false,
            warnings: Vec::new(),
            revoked: inner.user_revoked.clone(),
        }
    }
}
