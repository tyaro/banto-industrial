//! サービスの既定の TLS の読み込み（[`crate::hub_tls_prepare::load`]、#644 PR1b）を、
//! `prepare-hub-tls` と同じ偽物の [`HubTlsOps`] で確かめる（全 OS）: 確かめの順序・書き込みも
//! 作成もしないこと・断る分岐ごとの理由と対処・鍵の中身を出さないこと。

use super::*;
use crate::hub_tls_prepare::load::{
    bind_failure_lines, load_default_identity, DefaultHubTls, DefaultTlsUnavailable,
    DEFAULT_TLS_PORT,
};

/// 鍵・証明書の形を変える手（表のテスト用）。
type FileEdit = Box<dyn Fn(&mut FileEntry)>;

/// 読めなかった: 理由・対処を返し、書き込み・作成・直列化の mutex を一切触っていない。
fn assert_unavailable(ops: &FakeOps, needle: &str) -> DefaultTlsUnavailable {
    let found = load_default_identity(ops).expect_err("読めないはず");
    assert!(found.reason.contains(needle), "{needle} が無い: {found:#?}");
    assert_eq!(ops.writes(), Vec::<String>::new(), "{found:#?}");
    assert!(
        !ops.calls().iter().any(|c| c == "lock"),
        "{:#?}",
        ops.calls()
    );
    for line in found.log_lines() {
        assert!(line.starts_with("[WARN] banto-hub: "), "{line}");
        assert!(!line.contains("PRIVATE KEY"), "{line}");
    }
    found
}

#[test]
fn default_tls_loads_a_prepared_identity_in_order_without_writing() {
    let (ops, identity) = FakeOps::with_identity();
    let loaded = load_default_identity(&ops).unwrap();
    assert_eq!(loaded.port(), DEFAULT_TLS_PORT);
    assert_eq!(loaded.cert_pem(), identity.cert_pem.as_bytes());
    assert_eq!(loaded.cert_path(), tls().join(CERT_FILE));
    let info = inspect_identity(
        identity.cert_pem.as_bytes(),
        identity.key_pem.as_bytes(),
        NOW,
    )
    .unwrap();
    assert_eq!(loaded.info(), &info);
    // 書き込み・作成・mutex なし。ルート → tls\ → 鍵 → 証明書の順に、辿らずに開く。
    assert_eq!(
        ops.calls(),
        vec![
            "pin BantoHub",
            "open_dir tls",
            "open_file hub-key.pem",
            "open_file hub-cert.pem",
        ]
    );
    // Debug にもログの行にも鍵を出さない。
    let shown = format!("{loaded:?}");
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(!shown.contains("PRIVATE KEY"), "{shown}");
    let line = loaded.listening_line("127.0.0.1:8723".parse().unwrap());
    assert!(line.contains("https://127.0.0.1:8723"), "{line}");
    assert!(line.contains(&info.fingerprint), "{line}");
    assert!(line.contains(&utc_text(info.not_after_unix)), "{line}");
    assert_no_key_material(&[line, shown], Some(&identity.key_pem));
}

/// 読んだ鍵と証明書（メモリ上の PEM）をそのまま `tls_listener` に渡し、Sink と同じ pin で
/// 握手できる。
#[tokio::test]
async fn default_tls_identity_serves_tls_from_memory_to_a_pinned_client() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let identity = generate_identity(real_now()).unwrap();
    let mut loaded = DefaultHubTls::from_checked_pem(
        0,
        identity.cert_pem.as_bytes().to_vec(),
        Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
        tls().join(CERT_FILE),
        real_now(),
    )
    .unwrap();
    let (cert, key) = loaded.take_pem();
    assert!(loaded.cert_pem().is_empty());
    let config = crate::tls_listener::TlsConfig::in_memory(0, cert, key);
    let acceptor = config.acceptor().unwrap();
    let pins =
        banto_tagclient::PinnedHubTls::from_pem(std::slice::from_ref(&identity.cert_pem)).unwrap();
    let client_config = match pins.websocket_connector() {
        tokio_tungstenite::Connector::Rustls(config) => config,
        _ => panic!("PinnedHubTls は rustls の設定を返す"),
    };
    let connector = tokio_rustls::TlsConnector::from(client_config);
    let (client_io, server_io) = tokio::io::duplex(16 * 1024);
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
    let (mut client, mut server) = tokio::try_join!(
        connector.connect(name, client_io),
        acceptor.accept(server_io),
    )
    .unwrap();
    client.write_all(b"pinned-ok").await.unwrap();
    let mut marker = [0; 9];
    server.read_exact(&mut marker).await.unwrap();
    assert_eq!(&marker, b"pinned-ok");
}

#[test]
fn default_tls_is_unavailable_when_nothing_is_prepared() {
    // tls\ の中が空。
    let ops = FakeOps::new();
    let found = assert_unavailable(&ops, "ありません");
    assert!(found.remedy.contains("prepare-hub-tls"), "{found:#?}");
    assert!(!found.remedy.contains("--renew"), "{found:#?}");

    // tls\ が無い。
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&tls());
    let found = assert_unavailable(&ops, "Hub の TLS の置き場");
    assert!(found.remedy.contains("prepare-hub-tls"), "{found:#?}");

    // ルートが無い（作らない）。
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&tls());
    ops.dirs.borrow_mut().remove(&root());
    let found = assert_unavailable(&ops, "Hub のルート");
    assert!(found.remedy.contains("prepare-hub-tls"), "{found:#?}");
    assert_eq!(ops.calls(), vec!["pin BantoHub"]);

    // ProgramData を取れない。
    let mut ops = FakeOps::new();
    ops.env.program_data = Err("取れません".to_string());
    assert_unavailable(&ops, "ProgramData");
    assert_eq!(ops.calls(), Vec::<String>::new());
}

#[test]
fn default_tls_refuses_only_one_of_the_pair() {
    for missing in [CERT_FILE, KEY_FILE] {
        let (ops, _identity) = FakeOps::with_identity();
        ops.files.borrow_mut().remove(&tls().join(missing));
        let found = assert_unavailable(&ops, "片方だけ");
        assert!(found.remedy.contains("--renew"), "{found:#?}");
    }
}

#[test]
fn default_tls_refuses_an_exposed_key_or_certificate() {
    let key_cases: Vec<FileEdit> = vec![
        Box::new(|f| {
            f.security
                .dacl
                .aces
                .as_mut()
                .unwrap()
                .push(ace(USERS, READ_EXECUTE_ACCESS_MASK, 0))
        }),
        Box::new(|f| f.security.dacl.owner = EVIL.to_string()),
        Box::new(|f| f.security.links = 2),
        Box::new(|f| f.security.dacl.aces = None),
    ];
    for edit in key_cases {
        let (ops, identity) = FakeOps::with_identity();
        ops.edit_file(KEY_FILE, |f| edit(f));
        let found = assert_unavailable(&ops, "秘密鍵");
        assert!(found.reason.contains("保護されていません"), "{found:#?}");
        assert!(found.remedy.contains("--renew"), "{found:#?}");
        assert!(found.remedy.contains("hub_tls_pinned_certs"), "{found:#?}");
        assert_no_key_material(&found.log_lines(), Some(&identity.key_pem));
    }

    let (ops, _identity) = FakeOps::with_identity();
    ops.edit_file(CERT_FILE, |f| {
        f.security
            .dacl
            .aces
            .as_mut()
            .unwrap()
            .push(ace(USERS, MODIFY_ACCESS_MASK, 0))
    });
    let found = assert_unavailable(&ops, "証明書（");
    assert!(found.remedy.contains("--renew"), "{found:#?}");
}

#[test]
fn default_tls_refuses_broken_or_expired_identities() {
    let (mut ops, _identity) = FakeOps::with_identity();
    ops.now = not_after_for(NOW).unwrap() + 1;
    let found = assert_unavailable(&ops, "期限が切れています");
    assert!(found.remedy.contains("--renew"), "{found:#?}");

    let (ops, identity) = FakeOps::with_identity();
    ops.edit_file(KEY_FILE, |f| f.contents = b"broken".to_vec());
    let found = assert_unavailable(&ops, "秘密鍵の PEM を読めません");
    assert_no_key_material(&found.log_lines(), Some(&identity.key_pem));

    let (ops, _identity) = FakeOps::with_identity();
    let other = generate_identity(NOW).unwrap();
    ops.edit_file(KEY_FILE, |f| f.contents = other.key_pem.as_bytes().to_vec());
    assert_unavailable(&ops, "対になっていません");

    // 上限を超える大きさは読まない。
    let (ops, _identity) = FakeOps::with_identity();
    ops.edit_file(CERT_FILE, |f| f.contents = vec![b'A'; MAX_PEM_BYTES + 1]);
    assert_unavailable(&ops, "読めませんでした");
}

#[test]
fn default_tls_refuses_directories_links_misplaced_or_busy_files() {
    for name in [KEY_FILE, CERT_FILE] {
        let (ops, _identity) = FakeOps::with_identity();
        ops.edit_file(name, |f| f.security.is_dir = true);
        assert_unavailable(&ops, "フォルダです");

        let (ops, _identity) = FakeOps::with_identity();
        ops.edit_file(name, |f| f.security.is_reparse_point = true);
        assert_unavailable(&ops, "reparse point");

        let (ops, _identity) = FakeOps::with_identity();
        ops.edit_file(name, |f| f.real = Some(abs("Elsewhere").join(name)));
        assert_unavailable(&ops, "期待と違います");

        let (mut ops, _identity) = FakeOps::with_identity();
        ops.open_error
            .insert(name.to_string(), FileOpenError::InUse);
        assert_unavailable(&ops, "書き込みに開かれています");
    }
}

/// 一般の利用者が作った・書ける `tls\`（実行した管理者が所有者のものも、サービスは本人を
/// 知らないので断る）・リンク・別の場所: 中のファイルは開かず、消して作り直す案内。
#[test]
fn default_tls_refuses_a_foreign_exposed_or_linked_tls_dir() {
    let cases: Vec<(DirEdit, &str)> = vec![
        (Box::new(|e| e.owner = EVIL.to_string()), "所有者"),
        (Box::new(|e| e.owner = ME.to_string()), "所有者"),
        (
            Box::new(|e| e.aces.push(ace(USERS, MODIFY_ACCESS_MASK, 0x03))),
            "保護されていません",
        ),
        (
            Box::new(|e| e.aces.push(ace("S-1-1-0", READ_EXECUTE_ACCESS_MASK, 0x03))),
            "保護されていません",
        ),
        (Box::new(|e| e.is_reparse_point = true), "reparse point"),
        (
            Box::new(|e| e.real = Some(abs("Elsewhere/tls"))),
            "期待と違います",
        ),
    ];
    for (edit, needle) in cases {
        let (ops, _identity) = FakeOps::with_identity();
        let mut entry = DirEntry::users_read();
        edit(&mut entry);
        ops.dirs.borrow_mut().insert(tls(), entry);
        let found = assert_unavailable(&ops, needle);
        assert!(found.remedy.contains("消して"), "{found:#?}");
        assert!(
            !ops.calls().iter().any(|c| c.starts_with("open_file")),
            "{:#?}",
            ops.calls()
        );
    }
}

/// 保護されていないルート（一般の利用者・実行した管理者が所有者、乗っ取りの権利）: `tls\` を
/// 開かずに断る。ProgramData から継承した既定の形は通す。
#[test]
fn default_tls_refuses_an_unprotected_root_without_opening_the_tls_dir() {
    let cases: Vec<DirEdit> = vec![
        Box::new(|e| e.aces.push(ace(USERS, 0x0000_0040, 0x13))),
        Box::new(|e| e.aces.push(ace(USERS, 0x0001_0000, 0x00))),
        Box::new(|e| e.aces.push(ace("S-1-1-0", 0x1000_0000, 0x03))),
        Box::new(|e| e.owner = ME.to_string()),
        Box::new(|e| e.is_reparse_point = true),
    ];
    for edit in cases {
        let (ops, _identity) = FakeOps::with_identity();
        let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
        entry.aces = default_root_aces();
        edit(&mut entry);
        ops.dirs.borrow_mut().insert(root(), entry);
        let found = assert_unavailable(&ops, "Hub のルート");
        assert!(found.remedy.contains("takeown"), "{found:#?}");
        assert!(
            !ops.calls().iter().any(|c| c.contains("tls")),
            "{:#?}",
            ops.calls()
        );
    }

    let (ops, _identity) = FakeOps::with_identity();
    let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
    entry.aces = default_root_aces();
    ops.dirs.borrow_mut().insert(root(), entry);
    assert!(load_default_identity(&ops).is_ok());
}

#[test]
fn default_tls_bind_failure_lines_name_the_port_and_the_remedy() {
    let lines = bind_failure_lines(DEFAULT_TLS_PORT, "使用中です");
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.starts_with("[WARN] banto-hub: ")));
    assert!(lines[0].contains("127.0.0.1:8723"), "{lines:#?}");
    assert!(lines[0].contains("使用中です"), "{lines:#?}");
    assert!(lines[1].contains("netstat"), "{lines:#?}");
}
