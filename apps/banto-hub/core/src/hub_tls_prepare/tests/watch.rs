//! サービスの Hub の周期の確かめ（[`crate::hub_tls_prepare::watch`]、#1121 PR2c）を、
//! `prepare-hub-tls` と同じ偽物の [`HubTlsOps`] で確かめる（全 OS）。設計 §4.4.2 の場面の番号を
//! 試験の名前に入れてある。

use super::*;
use crate::hub_tls_prepare::watch::{ServiceTlsWatch, WatchLine, WatchRound};

/// 起動のときに確かめた組を置き、その証明書の拇印で見張りを始める。
fn watching() -> (FakeOps, ServiceTlsWatch, CertThumbprint) {
    let (ops, identity) = FakeOps::with_identity();
    let serving = CertThumbprint::of_pem(identity.cert_pem.as_bytes()).unwrap();
    (ops, ServiceTlsWatch::new(serving), serving)
}

fn text(round: &WatchRound) -> String {
    round
        .lines
        .iter()
        .map(WatchLine::text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn warns(round: &WatchRound) -> usize {
    round
        .lines
        .iter()
        .filter(|line| matches!(line, WatchLine::Warn(_)))
        .count()
}

/// 何も変わっていなければ、何も書かず、何も出さず、止めない。`tls\` には書かない。
#[test]
fn a_healthy_place_keeps_serving_quietly() {
    let (ops, mut watch, _) = watching();
    for _ in 0..3 {
        let round = watch.check(&ops);
        assert_eq!(round, WatchRound::default(), "{round:#?}");
    }
    assert_eq!(ops.writes(), Vec::<String>::new());
    assert!(!watch.is_stopped());
}

/// 場面 8: 露出（鍵・証明書・`tls\`・ルートの許可）を見たら、いま待ち受けている証明書の拇印を
/// 記録に足して止める。
#[test]
fn scene_8_exposure_records_the_serving_certificate_and_stops() {
    type Edit = Box<dyn Fn(&FakeOps)>;
    let cases: Vec<(Edit, &str)> = vec![
        (
            Box::new(|ops| {
                ops.edit_file(KEY_FILE, |f| {
                    f.security.dacl.aces.as_mut().unwrap().push(ace(
                        USERS,
                        READ_EXECUTE_ACCESS_MASK,
                        0,
                    ))
                })
            }),
            "秘密鍵",
        ),
        (
            Box::new(|ops| ops.edit_file(KEY_FILE, |f| f.security.dacl.aces = None)),
            "秘密鍵",
        ),
        (
            Box::new(|ops| {
                ops.edit_file(CERT_FILE, |f| {
                    f.security
                        .dacl
                        .aces
                        .as_mut()
                        .unwrap()
                        .push(ace(USERS, MODIFY_ACCESS_MASK, 0))
                })
            }),
            "証明書（",
        ),
        (
            Box::new(|ops| {
                ops.dirs
                    .borrow_mut()
                    .get_mut(&tls())
                    .unwrap()
                    .aces
                    .push(ace(USERS, MODIFY_ACCESS_MASK, 0x03))
            }),
            "Hub の TLS の置き場",
        ),
    ];
    for (edit, needle) in cases {
        let (ops, mut watch, serving) = watching();
        assert!(!watch.check(&ops).stop);
        edit(&ops);
        let round = watch.check(&ops);
        assert!(round.stop, "{round:#?}");
        assert!(round.finished, "{round:#?}");
        let shown = text(&round);
        assert!(shown.contains(needle), "{shown}");
        assert!(shown.contains("書きました"), "{shown}");
        assert!(shown.contains("再開しません"), "{shown}");
        assert!(ops.record().unwrap().contains(&serving), "{shown}");
        assert_eq!(ops.writes(), vec!["WRITE replace revocations.txt"]);
        assert!(watch.is_stopped());
        // 止めた後は、もう一度止めない（再開もしない）。
        let again = watch.check(&ops);
        assert!(!again.stop && again.finished, "{again:#?}");
    }
}

/// 場面 8・オーナー決定 1: 所有者の違い（鍵・`tls\`）も失効に入れる。
#[test]
fn scene_8_foreign_owner_records_and_stops() {
    let (ops, mut watch, serving) = watching();
    ops.edit_file(KEY_FILE, |f| f.security.dacl.owner = EVIL.to_string());
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    assert!(ops.record().unwrap().contains(&serving));

    let (ops, mut watch, serving) = watching();
    ops.dirs.borrow_mut().get_mut(&tls()).unwrap().owner = EVIL.to_string();
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    assert!(text(&round).contains("所有者"), "{round:#?}");
    assert!(ops.record().unwrap().contains(&serving));
}

/// ディスク上の証明書が別の拇印（すり替えられた）なら、それも失効に入れる。
#[test]
fn scene_8_a_different_certificate_on_disk_is_recorded_too() {
    let (ops, mut watch, serving) = watching();
    let other = generate_identity(NOW).unwrap();
    let other_thumb = CertThumbprint::of_pem(other.cert_pem.as_bytes()).unwrap();
    ops.edit_file(CERT_FILE, |f| {
        f.contents = other.cert_pem.as_bytes().to_vec();
        f.security
            .dacl
            .aces
            .as_mut()
            .unwrap()
            .push(ace(USERS, MODIFY_ACCESS_MASK, 0));
    });
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    let record = ops.record().unwrap();
    assert!(record.contains(&serving) && record.contains(&other_thumb));
    assert!(record
        .entries()
        .iter()
        .all(|entry| entry.place == RevokedCertPlace::Service));
}

/// 失効に入れない種類（ハードリンク・リンク・場所の食い違い・無い・一時的な失敗）は、記録に
/// 書かず、止めず、警告を出す（同じ理由は繰り返さない）。
#[test]
fn links_missing_and_transient_failures_keep_serving_with_a_warning() {
    type Edit = Box<dyn Fn(&mut FakeOps)>;
    let cases: Vec<Edit> = vec![
        Box::new(|ops| ops.edit_file(KEY_FILE, |f| f.security.links = 2)),
        Box::new(|ops| ops.edit_file(CERT_FILE, |f| f.security.is_reparse_point = true)),
        Box::new(|ops| ops.edit_file(KEY_FILE, |f| f.real = Some(abs("Elsewhere/k.pem")))),
        Box::new(|ops| {
            ops.files.borrow_mut().remove(&tls().join(CERT_FILE));
        }),
        Box::new(|ops| {
            ops.dirs.borrow_mut().remove(&tls());
        }),
        Box::new(|ops| {
            ops.dirs
                .borrow_mut()
                .get_mut(&tls())
                .unwrap()
                .is_reparse_point = true;
        }),
        Box::new(|ops| {
            ops.open_error
                .insert(KEY_FILE.to_string(), FileOpenError::InUse);
        }),
    ];
    for edit in cases {
        let (mut ops, mut watch, _) = watching();
        edit(&mut ops);
        let round = watch.check(&ops);
        assert!(!round.stop && !round.finished, "{round:#?}");
        assert_eq!(warns(&round), 1, "{round:#?}");
        assert!(text(&round).contains("使い続けます"), "{round:#?}");
        assert_eq!(ops.writes(), Vec::<String>::new());
        assert_eq!(ops.record().unwrap().entries(), &[]);
        // 同じ理由は繰り返さない。
        let again = watch.check(&ops);
        assert_eq!(again, WatchRound::default(), "{again:#?}");
    }
}

/// 場面 8（別のプロセスが書いた）: 記録に既にある拇印なら止める（何も書かない）。
#[test]
fn scene_8_an_already_recorded_certificate_stops() {
    let (ops, mut watch, serving) = watching();
    assert!(!watch.check(&ops).stop);
    ops.put_record(Some(&record_with(&[thumb(1), serving])));
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    assert!(text(&round).contains("失効の記録にあります"), "{round:#?}");
    assert!(text(&round).contains(RENEW_FLAG), "{round:#?}");
    assert_eq!(ops.writes(), Vec::<String>::new());

    // ほかの拇印だけなら保つ。
    let (ops, mut watch, _) = watching();
    ops.put_record(Some(&record_with(&[thumb(1)])));
    assert_eq!(watch.check(&ops), WatchRound::default());
}

/// 記録が無ければ（動いている間に消された）、露出を見たときに拇印入りで置き換えずに作る。
#[test]
fn scene_8_an_absent_record_is_created_with_the_thumbprint() {
    let (ops, mut watch, serving) = watching();
    ops.put_record(None);
    // 無いだけなら空として保つ（作らない）。
    assert_eq!(watch.check(&ops), WatchRound::default());
    assert_eq!(ops.writes(), Vec::<String>::new());

    ops.edit_file(KEY_FILE, |f| f.security.dacl.aces = None);
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    assert_eq!(
        ops.writes(),
        vec!["CREATE record revocations.txt no-replace"]
    );
    assert!(ops.record().unwrap().contains(&serving));
}

/// オーナー決定 3: 書けなければ、拇印を覚えて使わず（止める）、毎周期書き直しを試みてログに出す。
/// 書けたら終わる。
#[test]
fn unwritable_record_remembers_and_retries_every_round() {
    let (mut ops, mut watch, serving) = watching();
    ops.create_error
        .insert("revocations.txt".to_string(), "ディスクが一杯".to_string());
    ops.edit_file(KEY_FILE, |f| f.security.dacl.aces = None);
    let round = watch.check(&ops);
    assert!(round.stop && !round.finished, "{round:#?}");
    assert!(text(&round).contains("書けませんでした"), "{round:#?}");
    assert!(text(&round).contains("ディスクが一杯"), "{round:#?}");
    assert_eq!(watch.unwritten(), vec![serving]);

    // 管理者が鍵の ACL を直しても、覚えた拇印は使わない。毎周期書き直しを試みてログに出す。
    ops.edit_file(KEY_FILE, |f| {
        f.security.dacl.aces = FileEntry::with(FileAcl::ServiceOnly, b"")
            .security
            .dacl
            .aces
    });
    for _ in 0..2 {
        let round = watch.check(&ops);
        assert!(!round.stop && !round.finished, "{round:#?}");
        assert!(text(&round).contains("書けませんでした"), "{round:#?}");
    }

    ops.create_error.clear();
    let round = watch.check(&ops);
    assert!(!round.stop && round.finished, "{round:#?}");
    assert!(text(&round).contains("書きました"), "{round:#?}");
    assert!(ops.record().unwrap().contains(&serving));
    assert!(watch.unwritten().is_empty());
}

/// 場面 26: 記録が壊れた・置き場が通らない（Untrusted）は止める（何も書かない）。一時的
/// （Transient）は保って毎周期警告する。
#[test]
fn scene_26_broken_or_untrusted_records_stop_transient_keeps() {
    let (ops, mut watch, _) = watching();
    ops.put_record(Some(b"garbage"));
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    assert!(text(&round).contains("形が壊れています"), "{round:#?}");
    assert!(text(&round).contains(RENEW_FLAG), "{round:#?}");
    assert_eq!(ops.writes(), Vec::<String>::new());

    let (ops, mut watch, _) = watching();
    ops.dirs.borrow_mut().get_mut(&records()).unwrap().owner = EVIL.to_string();
    let round = watch.check(&ops);
    assert!(round.stop, "{round:#?}");
    assert!(text(&round).contains("tls-revocations"), "{round:#?}");
    assert_eq!(ops.writes(), Vec::<String>::new());

    let (mut ops, mut watch, _) = watching();
    ops.open_error
        .insert("revocations.txt".to_string(), FileOpenError::InUse);
    for _ in 0..2 {
        let round = watch.check(&ops);
        assert!(!round.stop, "{round:#?}");
        assert_eq!(warns(&round), 1, "{round:#?}");
        assert!(
            text(&round).contains("次の周期にやり直します"),
            "{round:#?}"
        );
    }
}

/// 露出を見て、記録にも書けない置き場（ルートが露出）: 止め、書けないことを毎周期ログに出す。
#[test]
fn an_exposed_root_stops_even_though_nothing_can_be_written() {
    let (ops, mut watch, serving) = watching();
    ops.dirs
        .borrow_mut()
        .get_mut(&root())
        .unwrap()
        .aces
        .push(ace(USERS, 0x0001_0000, 0x00));
    let round = watch.check(&ops);
    assert!(round.stop && !round.finished, "{round:#?}");
    assert!(text(&round).contains("Hub のルート"), "{round:#?}");
    assert!(text(&round).contains("takeown"), "{round:#?}");
    assert_eq!(watch.unwritten(), vec![serving]);
    assert_eq!(ops.writes(), Vec::<String>::new());
}
