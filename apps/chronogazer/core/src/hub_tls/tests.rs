//! [`HubTlsPolicy`] の単体試験。置き場を読む口は偽物（[`FakePlaces`]）。場面の番号は
//! docs/hub/hub-desktop-tls-design.md §4.4.2。

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::fake::{exposed, thumbprint, unusable, usable, FakePlaces};
use super::*;

const HUB: &str = "https://127.0.0.1:8723";

fn cert_pem() -> String {
    rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .cert
        .pem()
}

fn url(raw: &str) -> Url {
    Url::parse(raw).unwrap()
}

fn desktop(places: &Arc<FakePlaces>) -> HubTlsPolicy {
    places.desktop_policy()
}

fn server(places: &Arc<FakePlaces>) -> HubTlsPolicy {
    places.server_policy()
}

/// 起動の初回の整える手順まで済ませた、デスクトップの決まり。
async fn started_desktop(places: &Arc<FakePlaces>) -> HubTlsPolicy {
    let policy = desktop(places);
    policy.ensure_started(HUB).await;
    policy
}

fn refusal_of(
    result: banto_hub_bootstrap::Result<Option<PinnedHubTls>>,
) -> (BootstrapErrorKind, String) {
    let error = result.expect_err("refused");
    (error.kind(), error.detail().unwrap_or_default().to_owned())
}

fn connects(policy: &HubTlsPolicy) -> bool {
    policy.pins_for(&url(HUB)).is_ok_and(|pins| pins.is_some())
}

fn refusal(policy: &HubTlsPolicy) -> String {
    let (kind, detail) = refusal_of(policy.pins_for(&url(HUB)));
    assert_eq!(kind, BootstrapErrorKind::HubTls);
    detail
}

fn marker(policy: &HubTlsPolicy) -> Option<u64> {
    policy.marker_for(HUB).expect("a usable certificate")
}

fn warnings(policy: &HubTlsPolicy) -> Vec<String> {
    policy.warnings_for(HUB)
}

#[test]
fn loopback_http_is_refused_with_the_guidance_and_reads_nothing() {
    let places = FakePlaces::new();
    places.set_service(usable(&cert_pem()));
    for (policy, guidance) in [
        (desktop(&places), LOOPBACK_HTTP_REFUSED),
        (server(&places), LOOPBACK_HTTP_REFUSED_SERVICE),
    ] {
        for raw in [
            "http://127.0.0.1:8722/",
            "http://127.0.0.2:8722/",
            "http://127.1:8722/",
            "http://localhost:8722/",
            "http://LOCALHOST./",
            "http://hub.localhost:8722/",
            "http://[::1]:8722/",
            "http://[::ffff:127.0.0.1]:8722/",
        ] {
            assert_eq!(
                refusal_of(policy.pins_for(&url(raw))),
                (BootstrapErrorKind::HubTls, guidance.to_owned()),
                "{raw}"
            );
        }
        assert_eq!(
            policy
                .marker_for("http://127.0.0.1:8722")
                .unwrap_err()
                .kind(),
            BootstrapErrorKind::HubTls
        );
        assert!(policy.warnings_for("http://127.0.0.1:8722").is_empty());
    }
    assert!(LOOPBACK_HTTP_REFUSED.contains("デスクトップ版"));
    assert!(!LOOPBACK_HTTP_REFUSED_SERVICE.contains("デスクトップ版"));
    assert_eq!(
        places.calls(),
        Default::default(),
        "http is refused without reading any place"
    );
}

#[test]
fn the_lan_and_the_dev_policy_read_nothing() {
    let places = FakePlaces::new();
    places.set_service(usable(&cert_pem()));
    let policy = desktop(&places);
    // http: pin なしで受ける。https: pin なし（= bootstrap の規則で InvalidEndpoint）。
    for raw in [
        "http://192.168.11.5:8722/",
        "https://192.168.11.5:8723/",
        "http://hub.example:8722/",
        "http://[fe80::1]:8722/",
        "http://[2001:db8::1]:8722/",
    ] {
        assert!(policy.pins_for(&url(raw)).unwrap().is_none(), "{raw}");
        assert_eq!(policy.marker_for(raw).unwrap(), None, "{raw}");
        assert!(policy.warnings_for(raw).is_empty());
    }
    assert_eq!(places.calls(), Default::default());

    let dev = HubTlsPolicy::plain_loopback_http_for_dev();
    for raw in ["http://127.0.0.1:8722/", "https://127.0.0.1:8723/"] {
        assert!(dev.pins_for(&url(raw)).unwrap().is_none(), "{raw}");
    }
}

/// 整える手順（記録のロックを待つ）は、相手が同じ PC の Hub のときだけ走らせる。LAN の Hub
/// だけを使う利用者の `%LOCALAPPDATA%` に記録を作らない。
#[tokio::test]
async fn the_reconcile_runs_only_for_a_loopback_https_endpoint() {
    let places = FakePlaces::new();
    let policy = desktop(&places);
    for raw in [
        "http://192.168.11.5:8722",
        "http://127.0.0.1:8722",
        "not a url",
    ] {
        policy.ensure_started(raw).await;
        policy.refresh(raw).await;
    }
    assert_eq!(places.calls().reconcile, 0);

    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 1);
    policy.ensure_started(HUB).await;
    policy
        .clone()
        .ensure_started("https://localhost:8723")
        .await;
    assert_eq!(
        places.calls().reconcile,
        1,
        "the first run happens once (clones share the state)"
    );
    policy.refresh(HUB).await;
    assert_eq!(places.calls().reconcile, 2, "every supervisor cycle");

    // 開発用の決まりは何もしない。
    let dev = HubTlsPolicy::plain_loopback_http_for_dev();
    dev.ensure_started(HUB).await;
    dev.refresh(HUB).await;
}

/// 無い×無い: 断る。デスクトップでは Hub のデスクトップ版を主に案内する。
#[tokio::test]
async fn no_place_at_all_refuses_with_the_guidance_of_the_host() {
    let places = FakePlaces::new();
    let policy = started_desktop(&places).await;
    let detail = refusal(&policy);
    assert!(detail.contains("Hub のデスクトップ版を起動"), "{detail}");
    assert!(detail.contains("prepare-hub-tls"), "{detail}");
    assert!(
        warnings(&policy).is_empty(),
        "an absent place is not a warning"
    );

    let detail = refusal(&server(&places));
    assert!(!detail.contains("デスクトップ版"), "{detail}");
    assert!(
        detail.contains("banto-hub-elev.exe prepare-hub-tls"),
        "{detail}"
    );
}

/// 片方だけ使える: 無いほうは黙って外す（場面 3 と、デスクトップ版だけの PC）。
#[tokio::test]
async fn one_usable_place_is_pinned_and_the_absent_one_is_silent() {
    let places = FakePlaces::new();
    let policy = started_desktop(&places).await;

    places.set_service(usable(&cert_pem()));
    assert!(connects(&policy));
    assert!(warnings(&policy).is_empty());
    let service_only = marker(&policy);

    places.set_service(CertRead::Absent);
    places.set_user(usable(&cert_pem()));
    assert!(connects(&policy));
    assert!(warnings(&policy).is_empty());
    let user_only = marker(&policy);
    assert_ne!(service_only, user_only);

    // 両方: 2 枚を pin にする。
    places.set_service(usable(&cert_pem()));
    assert!(connects(&policy));
    assert!(warnings(&policy).is_empty());
    assert_ne!(marker(&policy), user_only);
}

/// 同じ証明書を別の置き場に置いたものは、PEM の連結の hash なら同じになる。置き場ごとの
/// 値の組なら取り違えない。
#[tokio::test]
async fn the_marker_tells_the_places_apart() {
    let pem = cert_pem();
    let places = FakePlaces::new();
    let policy = started_desktop(&places).await;

    places.set_service(usable(&pem));
    let in_service = marker(&policy);
    assert_eq!(marker(&policy), in_service, "stable while nothing changes");

    places.set_service(CertRead::Absent);
    places.set_user(usable(&pem));
    let in_user = marker(&policy);
    assert_ne!(in_service, in_user);

    // 同じ証明書が両方にあっても pin にできる（1 枚として数える）。
    places.set_service(usable(&pem));
    assert!(connects(&policy));
    assert_ne!(marker(&policy), in_user);
}

#[tokio::test]
async fn the_places_are_read_again_on_every_call() {
    let places = FakePlaces::new();
    places.set_service(usable(&cert_pem()));
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    let before = places.calls();

    assert!(connects(&policy));
    let first = marker(&policy);
    let after = places.calls();
    assert_eq!(after.service_cert, before.service_cert + 2);
    assert_eq!(after.service_record, before.service_record + 2);
    assert_eq!(after.user_cert, before.user_cert + 2);
    assert_eq!(after.user_record, before.user_record + 2);
    assert_eq!(
        after.reconcile, before.reconcile,
        "an operation never waits for the record lock"
    );
    assert!(after.record.is_empty());

    // 作り直し（`prepare-hub-tls --renew`・トレイ）は、再起動なしで次の呼び出しが読む。
    places.set_service(usable(&cert_pem()));
    assert_ne!(marker(&policy), first);
    places.set_user(usable(&cert_pem()));
    assert!(connects(&policy));
}

/// 片方が通らない: もう片方で接続し、**接続できても**警告を出す。ACL だけが変わった
/// （PEM は同じ）ときも目印が変わる。
#[tokio::test]
async fn an_unusable_place_is_left_out_and_warned_about_while_connected() {
    let service = cert_pem();
    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_service(usable(&service));
    places.set_user(usable(&user));
    let policy = started_desktop(&places).await;
    let healthy = marker(&policy);

    places.set_user(unusable("鍵をほかの相手が読めます", "直してください"));
    assert!(connects(&policy));
    let found = warnings(&policy);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("Hub のデスクトップ版の証明書"),
        "{found:?}"
    );
    assert!(found[0].contains("鍵をほかの相手が読めます"), "{found:?}");
    assert!(found[0].contains("直してください"), "{found:?}");
    let user_unusable = marker(&policy);
    assert_ne!(healthy, user_unusable, "the marker moves without a new PEM");

    places.set_user(usable(&user));
    places.set_service(unusable("所有者が違います", "管理者が直してください"));
    assert!(connects(&policy));
    let found = warnings(&policy);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("Hub のサービスの証明書"), "{found:?}");
    assert_ne!(marker(&policy), user_unusable);

    // 直れば警告は消え、目印も戻る。
    places.set_service(usable(&service));
    assert_eq!(marker(&policy), healthy);
    assert!(warnings(&policy).is_empty());

    // 両方とも通らなければ断り、両方の理由を出す。
    places.set_service(unusable("所有者が違います", "管理者が直してください"));
    places.set_user(unusable("鍵をほかの相手が読めます", "直してください"));
    let detail = refusal(&policy);
    assert!(detail.contains("所有者が違います"), "{detail}");
    assert!(detail.contains("鍵をほかの相手が読めます"), "{detail}");
}

/// PEM として pin にできないもの（確かめを抜けた壊れ）は使わない。
#[tokio::test]
async fn a_certificate_that_cannot_be_pinned_is_left_out() {
    let places = FakePlaces::new();
    let policy = started_desktop(&places).await;
    for broken in [b"not a pem".to_vec(), vec![0xff, 0xfe]] {
        places.set_service(CertRead::Usable(broken.clone()));
        places.set_user(CertRead::Absent);
        let detail = refusal(&policy);
        assert!(detail.contains("prepare-hub-tls --renew"), "{detail}");

        places.set_service(CertRead::Absent);
        places.set_user(CertRead::Usable(broken));
        let detail = refusal(&policy);
        assert!(detail.contains("TLS の証明書を作り直す"), "{detail}");
    }
}

/// 失効: サービスの証明書は、サービスの記録と利用者の記録の両方と照合する。利用者の証明書は
/// 利用者の記録と照合する。記録だけが足された（PEM は同じ）ときも目印が変わる（場面 6・8）。
#[tokio::test]
async fn a_revoked_certificate_is_left_out_and_the_marker_moves() {
    let service = cert_pem();
    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_service(usable(&service));
    places.set_user(usable(&user));
    let policy = started_desktop(&places).await;
    let healthy = marker(&policy);

    // 場面 8: サービスの Hub がサービスの記録に書いた。
    places.revoke_in_service_record(thumbprint(&service));
    assert!(connects(&policy), "the user certificate still connects");
    let after_service = marker(&policy);
    assert_ne!(healthy, after_service);
    let found = warnings(&policy);
    assert!(
        found.iter().any(|w| w.contains("サービスの失効の記録")
            && w.contains(&thumbprint(&service).to_string())
            && w.contains("prepare-hub-tls --renew")),
        "{found:?}"
    );

    // 場面 6: Hub のデスクトップ版が利用者の記録に書いた。
    places.revoke_in_user_record(thumbprint(&user));
    let detail = refusal(&policy);
    assert!(detail.contains("失効している"), "{detail}");
    assert!(detail.contains("TLS の証明書を作り直す"), "{detail}");

    // 作り直せば戻る（新しい証明書は記録に無い）。
    places.set_user(usable(&cert_pem()));
    assert!(connects(&policy));
    assert_ne!(marker(&policy), after_service);
}

/// 場面 9 の後: サービスの証明書の拇印が**利用者の記録**にあれば、サービスの記録に無くても
/// 使わない。`chronogazer-server` は利用者の記録を読まないので、照合しない。
#[tokio::test]
async fn the_service_certificate_is_matched_against_the_user_record_too() {
    let service = cert_pem();
    let places = FakePlaces::new();
    places.set_service(usable(&service));
    places.revoke_in_user_record(thumbprint(&service));
    let policy = started_desktop(&places).await;
    let detail = refusal(&policy);
    assert!(detail.contains("管理者に"), "{detail}");
    assert!(detail.contains("prepare-hub-tls --renew"), "{detail}");

    assert!(connects(&server(&places)));
}

/// サービスの記録が使えない（置き場が通らない・壊れている）ときは、サービスの証明書を
/// 使わない（場面 26）。利用者の証明書は止めない。
#[tokio::test]
async fn an_unusable_service_record_withholds_the_service_certificate() {
    let service = cert_pem();
    let places = FakePlaces::new();
    places.set_service(usable(&service));
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    let healthy = marker(&policy);

    places.set_service_record(ServiceRecordRead::Unusable {
        reason: "形が壊れています".to_owned(),
        salvaged: BTreeSet::new(),
    });
    assert!(connects(&policy));
    assert_ne!(marker(&policy), healthy);
    let found = warnings(&policy);
    assert!(
        found
            .iter()
            .any(|w| w.contains("サービスの失効の記録を確かめられない")
                && w.contains("形が壊れています")),
        "{found:?}"
    );

    places.set_user(CertRead::Absent);
    assert!(refusal(&policy).contains("形が壊れています"));

    // 壊れた記録から行ごとに読めた拇印に入っていれば、失効として扱う。
    places.set_service_record(ServiceRecordRead::Unusable {
        reason: "形が壊れています".to_owned(),
        salvaged: BTreeSet::from([thumbprint(&service)]),
    });
    assert!(refusal(&policy).contains("サービスの失効の記録にある"));
}

/// 利用者の証明書を**使い始める**のは、最後の整える手順が「整っている」のときだけ
/// （§4.4.1。起動のときは「通らない」も「この回はできなかった」も閉じる側）。サービスの
/// 証明書は、その結果で止めない。
#[tokio::test]
async fn the_user_certificate_is_first_used_only_after_a_consistent_reconcile() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = desktop(&places);

    // まだ 1 回も走らせていない。
    assert!(refusal(&policy).contains("まだ確かめていない"));

    for outcome in [ReconcileOutcome::Refused, ReconcileOutcome::Incomplete] {
        places.set_reconcile(outcome);
        policy.refresh(HUB).await;
        let detail = refusal(&policy);
        assert!(
            detail.contains("利用者の失効の記録"),
            "{outcome:?}: {detail}"
        );
    }

    // サービスの証明書は、利用者の記録を失っていても使う（警告は出す。場面 27）。
    places.set_service(usable(&cert_pem()));
    assert!(connects(&policy));
    let withheld = marker(&policy);
    assert!(!warnings(&policy).is_empty());

    places.set_reconcile(ReconcileOutcome::Consistent);
    policy.refresh(HUB).await;
    assert!(connects(&policy));
    assert!(warnings(&policy).is_empty());
    assert_ne!(
        marker(&policy),
        withheld,
        "the user certificate joined the pins"
    );
}

/// 起動の初回の整える手順が「できなかった」で終わっても、次の `ensure_started` で走らせ直す
/// （保存済みの接続が無い PC では見張りが `refresh` を呼ばない）。「整っている」になれば、
/// 新しい間（30 秒）は走らせない。
#[tokio::test]
async fn ensure_started_reruns_until_the_reconcile_is_consistent() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    places.set_reconcile(ReconcileOutcome::Incomplete);
    let policy = desktop(&places);

    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 1);
    assert!(refusal(&policy).contains("利用者の失効の記録"));

    places.set_reconcile(ReconcileOutcome::Consistent);
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 2);
    assert!(connects(&policy));

    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 2, "consistent: not run again");
}

/// 消失を見た整える手順の代わり: `nth` 回目の整える手順が、今ある利用者の証明書を失効に入れて
/// 記録を作り直す（設計 §4.4.1「記録を新しく作るときの決まり」）。結果は試験が別に決める。
fn repair_on_reconcile(places: &Arc<FakePlaces>, nth: usize, existing: CertThumbprint) {
    let weak = Arc::downgrade(places);
    places.after_reconcile_read(move |call| {
        if call == nth {
            let places = weak.upgrade().expect("the places");
            places.revoke_in_user_record(existing);
            places.set_user_record_settled(true);
        }
    });
}

/// 入口は、古い「整っている」を信じない: 保存済みの接続が無い PC（見張りが `refresh` を
/// 呼ばない）で、「整っている」の後に記録を消されても、見張りの周期（30 秒）を過ぎた入口が
/// 整え直すので、失効していた利用者の証明書を空の記録で使い始めない。30 秒以内は省く。
#[tokio::test]
async fn an_entry_reconciles_again_once_the_last_result_is_older_than_a_cycle() {
    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_user(usable(&user));
    places.revoke_in_user_record(thumbprint(&user));
    let policy = started_desktop(&places).await;
    assert!(refusal(&policy).contains("失効"));
    assert_eq!(places.calls().reconcile, 1);

    // 30 秒以内の入口（状態の取得のポーリングなど）は走らせない。
    policy.advance_clock(RECONCILE_FRESH_FOR - Duration::from_secs(1));
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 1, "fresh: not run again");

    // 記録が消された。次の入口は 30 秒を過ぎているので、Hub と話す前に整え直す。
    places.wipe_user_record();
    places.push_reconcile(ReconcileOutcome::Refused, true, "記録が消された");
    repair_on_reconcile(&places, 2, thumbprint(&user));
    policy.advance_clock(Duration::from_secs(1));
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 2, "stale: run again at the entry");
    assert!(refusal(&policy).contains("失効"));
    assert!(warnings(&policy)
        .iter()
        .any(|warning| warning.contains("記録が消された")));
}

/// 30 秒以内でも、ロック無しの読みが記録の「整っていない」（消された・壊れた）を見たら、
/// 最後の結果が「整っている」のままでも利用者の証明書を使い始めない。次の入口は 30 秒を
/// 待たずに整え直す。
#[tokio::test]
async fn a_wiped_record_keeps_a_revoked_user_certificate_out_even_while_fresh() {
    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_user(usable(&user));
    places.revoke_in_user_record(thumbprint(&user));
    let policy = started_desktop(&places).await;
    assert!(refusal(&policy).contains("失効"));

    // 記録が消された直後の操作: 入口は省かれる（新しい「整っている」）が、判定のロック無しの
    // 読みが見る。
    places.wipe_user_record();
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 1);
    let detail = refusal(&policy);
    assert!(detail.contains("整っていない"), "{detail}");

    // サービスの証明書は止めない（利用者の証明書は入らないまま）。
    places.set_service(usable(&cert_pem()));
    assert!(connects(&policy));
    assert!(warnings(&policy)
        .iter()
        .any(|warning| warning.contains("整っていない")));

    // 次の入口は、30 秒を待たずに整え直す（消失の直しで、今ある証明書が失効に入る）。
    places.push_reconcile(ReconcileOutcome::Refused, true, "記録が消された");
    repair_on_reconcile(&places, 2, thumbprint(&user));
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 2);
    assert!(connects(&policy));
    assert!(warnings(&policy)
        .iter()
        .any(|warning| warning.contains("失効")));
}

/// すでに pin に使っている利用者の証明書は、ロック無しの読みが「整っていない」を見ても、次の
/// 整える手順まで保つ（ほかのプロセスが書いている途中にも起きる）。次の入口がすぐ整え直し、
/// その結果が「通らない」なら外す。
#[tokio::test]
async fn a_user_certificate_in_use_is_kept_until_the_next_reconcile_decides() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));

    places.set_user_record_settled(false);
    assert!(connects(&policy), "kept until the reconcile has looked");

    // 書いている途中だっただけ: 整え直して「整っている」。
    places.set_user_record_settled(true);
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 2, "run again although fresh");
    assert!(connects(&policy));
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 2);

    // 本当に消されていた: 整え直した結果が「通らない」なので、すぐ外す。
    places.set_user_record_settled(false);
    assert!(connects(&policy));
    places.set_reconcile(ReconcileOutcome::Refused);
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 3);
    assert!(refusal(&policy).contains("確かめに通らない"));
}

/// 整える手順を待たせる口: `nth` 回目が結果を読んだ後で止まり、止まったことを知らせる。
/// 返した送り口に送る（か捨てる）と進む。
fn hold_reconcile(
    places: &Arc<FakePlaces>,
    nth: usize,
    then: ReconcileOutcome,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let weak = Arc::downgrade(places);
    places.after_reconcile_read(move |call| {
        if call == nth {
            // この回が読んだ後に、記録の状態が変わる（以後の整える手順の結果）。
            weak.upgrade().expect("the places").set_reconcile(then);
            entered_tx.lock().unwrap().send(()).unwrap();
            let _ = release_rx.lock().unwrap().recv();
        }
    });
    (entered_rx, release_tx)
}

async fn wait_for(entered: std::sync::mpsc::Receiver<()>) {
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the reconcile started")
    })
    .await
    .unwrap();
}

/// 整える手順は 1 つずつ走り、読んだ順に状態へ書く: 先に読んだ「整っている」を持ったまま
/// 遅れた回が、後から読んだ「通らない」を上書きして、利用者の証明書を使える状態に戻さない。
#[tokio::test]
async fn concurrent_reconciles_are_applied_in_the_order_they_read() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));

    // 2 回目は「整っている」を読んでから止まる。その後、記録は「通らない」になる。
    let (entered, release) = hold_reconcile(&places, 2, ReconcileOutcome::Refused);
    let first = tokio::spawn({
        let policy = policy.clone();
        async move { policy.refresh(HUB).await }
    });
    wait_for(entered).await;
    let second = tokio::spawn({
        let policy = policy.clone();
        async move { policy.refresh(HUB).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        places.calls().reconcile,
        2,
        "the later reconcile waits for the earlier one"
    );

    release.send(()).unwrap();
    first.await.unwrap();
    second.await.unwrap();
    assert_eq!(places.calls().reconcile, 3);
    assert!(
        refusal(&policy).contains("確かめに通らない"),
        "the result read last stays"
    );
}

/// 入口が直列化のロックを待つ間に、別の呼び出しが新しい結果を作っていたら、入口はそれを
/// 使って走らせない（「できなかった」が続く間に、入口が並んで待ち続けない）。
#[tokio::test]
async fn an_entry_that_waited_uses_the_result_made_meanwhile() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    // 入口が走らせたい状態（30 秒を過ぎた）。
    policy.advance_clock(RECONCILE_FRESH_FOR);

    let (entered, release) = hold_reconcile(&places, 2, ReconcileOutcome::Incomplete);
    let cycle = tokio::spawn({
        let policy = policy.clone();
        async move { policy.refresh(HUB).await }
    });
    wait_for(entered).await;
    let entry = tokio::spawn({
        let policy = policy.clone();
        async move { policy.ensure_started(HUB).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    release.send(()).unwrap();
    cycle.await.unwrap();
    entry.await.unwrap();
    assert_eq!(places.calls().reconcile, 2, "the entry did not run its own");
    assert!(connects(&policy));
}

/// すでに pin に使っている利用者の証明書: 「この回はできなかった」は 3 周期続くまで保ち
/// （警告）、続いたら外す。「整っている」に戻れば再び使う（場面 23）。
#[tokio::test]
async fn an_incomplete_reconcile_keeps_the_user_pin_for_three_cycles() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));
    let in_use = marker(&policy);

    places.set_reconcile(ReconcileOutcome::Incomplete);
    for cycle in 1..INCOMPLETE_LIMIT {
        policy.refresh(HUB).await;
        assert!(connects(&policy), "kept on cycle {cycle}");
        assert_eq!(marker(&policy), in_use, "no reconnect on cycle {cycle}");
        let found = warnings(&policy);
        assert!(
            found.iter().any(|w| w.contains("終わりませんでした")),
            "{found:?}"
        );
    }
    policy.refresh(HUB).await;
    let detail = refusal(&policy);
    assert!(detail.contains("3 周期続けて"), "{detail}");
    // 外した後は「使い始める」の条件に戻る: もう 1 回できなくても、使わないまま。
    policy.refresh(HUB).await;
    assert!(!connects(&policy));

    places.set_reconcile(ReconcileOutcome::Consistent);
    policy.refresh(HUB).await;
    assert!(connects(&policy));
    assert_eq!(marker(&policy), in_use);
    assert!(warnings(&policy).is_empty());

    // 連続数は「整っている」で 0 に戻る。
    places.set_reconcile(ReconcileOutcome::Incomplete);
    policy.refresh(HUB).await;
    assert!(connects(&policy));
}

/// 操作の入口の走らせ直し（`ensure_started`）は「できなかった」の連続数を進めない:
/// 3 周期は見張りの周期だけで数える。
#[tokio::test]
async fn operation_entries_do_not_advance_the_incomplete_streak() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));

    places.set_reconcile(ReconcileOutcome::Incomplete);
    policy.refresh(HUB).await;
    for _ in 0..5 {
        policy.ensure_started(HUB).await;
        assert!(connects(&policy), "operation entries keep the pin");
    }
    policy.refresh(HUB).await;
    assert!(connects(&policy), "second cycle");
    policy.refresh(HUB).await;
    let detail = refusal(&policy);
    assert!(detail.contains("3 周期続けて"), "{detail}");
}

/// 「この回はできなかった」の間に証明書が作り直されたら、新しい証明書は「使い始める」に
/// あたるので、整うまで使わない。
#[tokio::test]
async fn a_renewed_user_certificate_is_not_first_used_while_incomplete() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));

    places.set_reconcile(ReconcileOutcome::Incomplete);
    policy.refresh(HUB).await;
    assert!(connects(&policy));
    places.set_user(usable(&cert_pem()));
    assert!(!connects(&policy));
}

/// すでに pin に使っている利用者の証明書も、「通らない」（消失を見た・置き場が通らない）は
/// すぐ外す（場面 13・15・27）。
#[tokio::test]
async fn a_refused_reconcile_drops_the_user_pin_at_once() {
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));

    places.push_reconcile(ReconcileOutcome::Refused, true, "記録 A が消されました");
    policy.refresh(HUB).await;
    let detail = refusal(&policy);
    assert!(detail.contains("確かめに通らない"), "{detail}");
    let found = warnings(&policy);
    assert!(
        found.iter().any(|w| w.contains("記録 A が消されました")),
        "{found:?}"
    );
    assert_eq!(
        places.calls().reconcile,
        2,
        "a running cycle does not run it twice"
    );

    // 次の周期で整えば（証明書が失効に入っていなければ）戻る。
    policy.refresh(HUB).await;
    assert!(connects(&policy));
    assert!(warnings(&policy).is_empty());
}

/// 起動の初回にその回で消失を直したら、続けてもう 1 回走らせる（設計 §7 PR2 決定 2）。直した
/// ことの警告は残す。
#[tokio::test]
async fn the_first_run_repeats_once_after_repairing_a_loss() {
    let places = FakePlaces::new();
    places.set_service(usable(&cert_pem()));
    places.set_user(usable(&cert_pem()));
    places.push_reconcile(ReconcileOutcome::Refused, true, "記録 B が消されました");
    let policy = started_desktop(&places).await;
    assert_eq!(places.calls().reconcile, 2);
    assert!(connects(&policy));
    let found = warnings(&policy);
    assert!(
        found.iter().any(|w| w.contains("記録 B が消されました")),
        "the repaired loss stays visible: {found:?}"
    );

    // 直らなかった（2 回目も通らない）なら、利用者の証明書は使わない。
    let places = FakePlaces::new();
    places.set_user(usable(&cert_pem()));
    places.push_reconcile(ReconcileOutcome::Refused, true, "記録 B が消されました");
    places.push_reconcile(ReconcileOutcome::Refused, true, "置き場が通りません");
    let policy = started_desktop(&places).await;
    assert_eq!(places.calls().reconcile, 2);
    assert!(!connects(&policy));
}

/// 場面 7: ChronoGazer が利用者の証明書の露出を見た。利用者の記録に書き、ACL が直っても
/// 二度と使わない。
#[tokio::test]
async fn an_exposed_user_certificate_is_recorded_and_never_used_again() {
    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_service(usable(&cert_pem()));
    places.set_user(usable(&user));
    let policy = started_desktop(&places).await;
    let healthy = marker(&policy);

    places.set_user(exposed("鍵を Users が読めます", &user));
    assert!(connects(&policy), "the service certificate still connects");
    let dropped = marker(&policy);
    assert_ne!(healthy, dropped);
    assert!(
        places.calls().record.is_empty(),
        "an operation does not wait for the record lock"
    );

    policy.refresh(HUB).await;
    assert_eq!(
        places.calls().record,
        vec![(thumbprint(&user), RevokedCertPlace::User)]
    );
    assert!(places.user_revoked().contains(&thumbprint(&user)));

    // ACL だけが直った: 同じ証明書は使わない。目印も「失効」のまま動かない。
    places.set_user(usable(&user));
    assert_eq!(marker(&policy), dropped);
    let found = warnings(&policy);
    assert!(
        found.iter().any(|w| w.contains("失効している")),
        "{found:?}"
    );
    policy.refresh(HUB).await;
    assert_eq!(places.calls().record.len(), 1, "written once");
}

/// 場面 9: ChronoGazer デスクトップがサービスの証明書の露出を先に見た。**利用者の記録**に
/// 書き、管理者に `prepare-hub-tls --renew` を頼むよう案内する。
#[tokio::test]
async fn an_exposed_service_certificate_goes_to_the_user_record() {
    let service = cert_pem();
    let places = FakePlaces::new();
    places.set_service(exposed("tls\\ に Users が書けます", &service));
    places.set_user(usable(&cert_pem()));
    let policy = started_desktop(&places).await;
    assert_eq!(
        places.calls().record,
        vec![(thumbprint(&service), RevokedCertPlace::Service)],
        "the first run already saw and recorded it"
    );
    assert!(connects(&policy));
    let found = warnings(&policy);
    assert!(
        found
            .iter()
            .any(|w| w.contains("管理者に") && w.contains("prepare-hub-tls --renew")),
        "{found:?}"
    );

    // 管理者が ACL だけを直した: 記録にあるので使わない。
    places.set_service(usable(&service));
    places.set_user(CertRead::Absent);
    assert!(refusal(&policy).contains("管理者に"));
}

/// 場面 23: 露出を見たが記録に書けない。書けなくてもすぐ外し、プロセスの中に覚えて使わず、
/// 周期ごとに書き直しを試みる。
#[tokio::test]
async fn an_exposure_that_cannot_be_recorded_is_remembered_and_retried() {
    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_user(usable(&user));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy));

    places.fail_records(true);
    places.set_user(exposed("鍵を Users が読めます", &user));
    policy.refresh(HUB).await;
    assert_eq!(places.calls().record.len(), 1);
    assert!(places.user_revoked().is_empty());
    let found = warnings(&policy);
    assert!(
        found
            .iter()
            .any(|w| w.contains("失効の記録に書けていません")
                && w.contains(&thumbprint(&user).to_string())),
        "{found:?}"
    );

    // ACL が直っても、記録に無くても、覚えているので使わない。
    places.set_user(usable(&user));
    assert!(refusal(&policy).contains("失効している"));
    policy.refresh(HUB).await;
    assert_eq!(places.calls().record.len(), 2, "retried every cycle");
    assert!(!connects(&policy));

    // 書けるようになったら書いて、書き直しをやめる。
    places.fail_records(false);
    policy.refresh(HUB).await;
    assert_eq!(places.calls().record.len(), 3);
    assert!(places.user_revoked().contains(&thumbprint(&user)));
    policy.refresh(HUB).await;
    assert_eq!(places.calls().record.len(), 3);
    assert!(
        !warnings(&policy)
            .iter()
            .any(|w| w.contains("書けていません")),
        "{:?}",
        warnings(&policy)
    );
    assert!(!connects(&policy));
}

/// 保存済みの接続が無い PC（見張りが `refresh` を呼ばない）では、整える手順が「整っている」でも、
/// 書けなかった失効は操作の入口（`ensure_started`）が書き直す。書けたら、もう何も呼ばない。
#[tokio::test]
async fn ensure_started_retries_an_unwritten_revocation_until_it_is_recorded() {
    let user = cert_pem();
    let places = FakePlaces::new();
    places.fail_records(true);
    places.set_user(exposed("鍵を Users が読めます", &user));
    let policy = desktop(&places);

    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, 1);
    assert_eq!(places.calls().record.len(), 1);
    assert!(warnings(&policy)
        .iter()
        .any(|w| w.contains("失効の記録に書けていません")));

    // 整っていても、書けていない失効があるので走らせ直す。
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().record.len(), 2);

    places.fail_records(false);
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().record.len(), 3);
    assert!(places.user_revoked().contains(&thumbprint(&user)));
    assert!(!warnings(&policy)
        .iter()
        .any(|w| w.contains("書けていません")));

    // 書けたので、次は整える手順も書き足しも呼ばない。
    let (reconcile, record) = (places.calls().reconcile, places.calls().record.len());
    policy.ensure_started(HUB).await;
    assert_eq!(places.calls().reconcile, reconcile);
    assert_eq!(places.calls().record.len(), record);
}

/// ほかのプロセス（Hub のデスクトップ版）が先に同じ拇印を書いていれば、書き足しを頼まない。
#[tokio::test]
async fn an_exposure_already_in_the_record_is_not_written_again() {
    let user = cert_pem();
    let places = FakePlaces::new();
    places.revoke_in_user_record(thumbprint(&user));
    places.set_user(exposed("鍵を Users が読めます", &user));
    let policy = started_desktop(&places).await;
    policy.refresh(HUB).await;
    assert!(places.calls().record.is_empty());
    assert!(!warnings(&policy)
        .iter()
        .any(|w| w.contains("書けていません")));
}

/// `chronogazer-server`: サービスの置き場とサービスの記録だけを読む。利用者の置き場・記録は
/// 読まず、書かない。露出を見た拇印はプロセスの間だけ覚える（場面 10）。
#[tokio::test]
async fn the_server_policy_reads_only_the_service_place() {
    let service = cert_pem();
    let places = FakePlaces::new();
    places.set_service(usable(&service));
    // 利用者の置き場に何があっても見ない。
    places.set_user(unusable("見ないはず", ""));
    places.revoke_in_user_record(thumbprint(&service));
    places.set_reconcile(ReconcileOutcome::Refused);
    let policy = server(&places);

    policy.ensure_started(HUB).await;
    policy.refresh(HUB).await;
    assert!(connects(&policy));
    assert!(warnings(&policy).is_empty());
    let healthy = marker(&policy);

    // サービスの記録は見る（場面 8）。
    places.revoke_in_service_record(thumbprint(&service));
    let detail = refusal(&policy);
    assert!(detail.contains("サービスの失効の記録"), "{detail}");
    assert!(!detail.contains("管理者に"), "{detail}");

    // 露出を先に見た: 記録には書かず、プロセスの間だけ覚える。
    let renewed = cert_pem();
    places.set_service(usable(&renewed));
    assert!(connects(&policy));
    assert_ne!(marker(&policy), healthy);
    places.set_service(exposed("証明書を Users が書けます", &renewed));
    let detail = refusal(&policy);
    assert!(detail.contains("露出"), "{detail}");
    assert!(!detail.contains("頼んで"), "{detail}");
    policy.refresh(HUB).await;
    places.set_service(usable(&renewed));
    assert!(
        refusal(&policy).contains("露出"),
        "remembered for the process"
    );
    // 別のプロセス（再起動）は覚えていない（受容。設計 §4.4.1「保証の範囲」）。
    assert!(connects(&server(&places)));

    let calls = places.calls();
    assert_eq!(calls.user_cert, 0);
    assert_eq!(calls.user_record, 0);
    assert_eq!(calls.reconcile, 0);
    assert!(calls.record.is_empty());
}

/// 警告は、同じ理由が続く間は状態に 1 つだけ持つ（増え続けない）。
#[tokio::test]
async fn the_warnings_are_replaced_on_every_evaluation() {
    let places = FakePlaces::new();
    places.set_service(usable(&cert_pem()));
    places.set_user(unusable("所有者が違います", ""));
    let policy = started_desktop(&places).await;
    for _ in 0..3 {
        assert!(connects(&policy));
        assert_eq!(warnings(&policy).len(), 1);
    }
    // clone は同じ状態を見る。
    assert_eq!(warnings(&policy.clone()), warnings(&policy));
}

/// #644 PR3b の読み口（`with_reader`）は、サービスの置き場 1 枚の決まりとして動く。
#[test]
fn the_reader_policy_pins_the_one_service_certificate() {
    struct Counting {
        answer: Result<Vec<u8>, String>,
        reads: AtomicUsize,
    }
    impl HubCertReader for Counting {
        fn read_cert_pem(&self) -> Result<Vec<u8>, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }
    let cert = Arc::new(Counting {
        answer: Ok(cert_pem().into_bytes()),
        reads: AtomicUsize::new(0),
    });
    let policy = HubTlsPolicy::with_reader(Arc::clone(&cert) as Arc<dyn HubCertReader>);
    assert!(connects(&policy));
    assert_eq!(cert.reads.load(Ordering::SeqCst), 1);
    assert!(marker(&policy).is_some());
    assert_eq!(cert.reads.load(Ordering::SeqCst), 2);
    assert_eq!(
        refusal_of(policy.pins_for(&url("http://127.0.0.1:8722"))).1,
        LOOPBACK_HTTP_REFUSED
    );

    let cert = Arc::new(Counting {
        answer: Err("置き場がありません".to_owned()),
        reads: AtomicUsize::new(0),
    });
    let policy = HubTlsPolicy::with_reader(cert as Arc<dyn HubCertReader>);
    assert!(refusal(&policy).contains("置き場がありません"));
}

#[cfg(not(windows))]
#[test]
fn other_systems_cannot_read_the_certificate() {
    for policy in [HubTlsPolicy::production(), HubTlsPolicy::service_only()] {
        let (kind, detail) = refusal_of(policy.pins_for(&url(HUB)));
        assert_eq!(kind, BootstrapErrorKind::HubTls);
        assert!(detail.contains("この OS"), "{detail}");
    }
}

/// 判定は 1 つずつ: 古い（失効の無い）記録を読んで止まっている判定 A の間に、別の判定 B が
/// 追加された失効を見て pin から外しても、B は A が終わるまで待ち、A の（古い）結果が B の結果を
/// 上書きしない。直列化が無いと B が先に終わり、後から再開した A が `user_in_use` を戻す。
#[tokio::test]
async fn a_stalled_evaluation_does_not_overwrite_a_later_one_that_saw_a_revocation() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::Mutex as StdMutex;

    let user = cert_pem();
    let places = FakePlaces::new();
    places.set_user(usable(&user));
    let policy = started_desktop(&places).await;
    assert!(connects(&policy), "the user certificate is in use");
    let in_use = || match &policy.loopback {
        Loopback::Pinned(pinned) => pinned.state().user_in_use,
        Loopback::PlainHttp => unreachable!("a pinned policy"),
    };
    assert_eq!(in_use(), Some(thumbprint(&user)));

    // 次の記録の読み（判定 A）で止まる。
    let stalled_at = places.calls().user_record + 1;
    let (reached_tx, reached_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = StdMutex::new(release_rx);
    let reached_tx = StdMutex::new(reached_tx);
    places.after_user_record_read(move |call| {
        if call == stalled_at {
            reached_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }
    });

    let b_done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| connects(&policy));
        reached_rx.recv().unwrap();
        // A は古い記録（失効なし）を読んだところで止まっている。ここで失効が足された。
        places.revoke_in_user_record(thumbprint(&user));
        let b = scope.spawn(|| {
            let detail = refusal(&policy);
            b_done.store(true, Ordering::SeqCst);
            detail
        });
        std::thread::sleep(Duration::from_millis(300));
        // 先に A を再開してから確かめる（確かめが落ちても A が止まったままにならない）。
        let b_finished_early = b_done.load(Ordering::SeqCst);
        release_tx.send(()).unwrap();
        assert!(
            !b_finished_early,
            "B waits until the stalled evaluation has finished"
        );
        assert!(a.join().unwrap(), "A saw the old record");
        assert!(b.join().unwrap().contains("失効"));
    });

    // 最後に反映されたのは B（失効を見た側）: その証明書は使われていない。
    assert_eq!(in_use(), None);
    assert!(refusal(&policy).contains("失効"));
}
