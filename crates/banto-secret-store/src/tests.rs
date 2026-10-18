//! [`FileSecretStore`] のテスト。全 OS（`PlainProtector`・ACL を確かめない開き方）と、
//! Windows（`DpapiMachine`・ACL を確かめる開き方）。

use std::path::Path;
use std::sync::Arc;

use banto_hub_bootstrap::error::ErrorKind;
use banto_hub_bootstrap::KeyStore;

use banto_safe_root::SafeRootError;

use crate::error::{CorruptKind, InvalidAccount, IoOp, SecretStoreError};
use crate::format;
use crate::store::{write_atomic, WriteStage};
use crate::{entropy_for, file_name_for, FileSecretStore, PlainProtector, Protector};

const SECRET: &str = "bh_TOPSECRET_9f2c1e7a_value";
const ACCOUNT_A: &str = "hub:127.0.0.1:8722:install-a";
const ACCOUNT_B: &str = "hub:127.0.0.1:8722:install-b";

fn store(dir: &Path) -> FileSecretStore<PlainProtector> {
    FileSecretStore::open_without_acl(dir, PlainProtector).unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_no_secret(shown: &str) {
    assert!(!shown.contains(SECRET), "秘密が出ている: {shown}");
    assert!(
        !shown.contains("TOPSECRET"),
        "秘密の一部が出ている: {shown}"
    );
}

#[test]
fn file_name_is_the_sha256_hex_of_the_account() {
    let name = file_name_for("abc");
    assert_eq!(
        name,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.bin"
    );
    assert_ne!(file_name_for(ACCOUNT_A), file_name_for(ACCOUNT_B));
    assert_eq!(entropy_for("abc"), b"banto-secret-store/v1:abc".to_vec());
}

#[test]
fn round_trips_overwrites_and_writes_the_documented_format() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    assert!(s.load(ACCOUNT_A).unwrap().is_none());

    s.store(ACCOUNT_A, SECRET).unwrap();
    assert_eq!(
        s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
        Some(SECRET)
    );
    assert_eq!(entries(root.path()), vec![file_name_for(ACCOUNT_A)]);

    let bytes = std::fs::read(s.path_for(ACCOUNT_A)).unwrap();
    let decoded = format::decode(&bytes).unwrap();
    assert_eq!(decoded.account, ACCOUNT_A);
    assert_eq!(&bytes[..4], b"BSS1");

    s.store(ACCOUNT_A, "second").unwrap();
    assert_eq!(
        s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
        Some("second")
    );
    assert_eq!(
        entries(root.path()),
        vec![file_name_for(ACCOUNT_A)],
        "一時ファイルが残った"
    );

    // 別のアカウントは別のファイル。
    s.store(ACCOUNT_B, "b").unwrap();
    assert_eq!(
        s.load(ACCOUNT_B).unwrap().as_deref().map(String::as_str),
        Some("b")
    );
    assert_eq!(
        s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
        Some("second")
    );
}

#[test]
fn delete_is_idempotent_and_a_missing_directory_reads_as_empty() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("hub-keys");
    let s = store(&dir);
    std::fs::remove_dir(&dir).unwrap();
    assert!(
        s.load(ACCOUNT_A).unwrap().is_none(),
        "フォルダが無い = 無い"
    );
    s.remove(ACCOUNT_A).unwrap();
    assert!(!dir.exists(), "読む・消すではフォルダを作らない");

    s.store(ACCOUNT_A, SECRET).unwrap();
    s.remove(ACCOUNT_A).unwrap();
    s.remove(ACCOUNT_A).unwrap();
    assert!(s.load(ACCOUNT_A).unwrap().is_none());
    assert!(entries(&dir).is_empty());
}

#[test]
fn rejects_invalid_accounts_and_oversized_secrets() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    assert_eq!(
        s.store("", SECRET),
        Err(SecretStoreError::InvalidAccount(InvalidAccount::Empty))
    );
    let long = "a".repeat(format::MAX_ACCOUNT_LEN + 1);
    assert_eq!(
        s.load(&long),
        Err(SecretStoreError::InvalidAccount(InvalidAccount::TooLong))
    );
    let huge = "x".repeat(format::MAX_SECRET_LEN + 1);
    assert_eq!(
        s.store(ACCOUNT_A, &huge),
        Err(SecretStoreError::SecretTooLarge)
    );
    assert!(entries(root.path()).is_empty());
}

/// ファイルをそのまま別のアカウントの名前へコピーしても読めない（中のアカウントの不一致）。
#[test]
fn a_file_copied_under_another_accounts_name_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    s.store(ACCOUNT_A, SECRET).unwrap();
    std::fs::copy(s.path_for(ACCOUNT_A), s.path_for(ACCOUNT_B)).unwrap();
    let err = s.load(ACCOUNT_B).unwrap_err();
    assert_eq!(err, SecretStoreError::AccountMismatch);
    assert_no_secret(&format!("{err} {err:?}"));
}

/// 中のアカウントの欄まで書き換えても（平文の欄なので誰でも書き換えられる）、
/// entropy が違うので復号できない。**entropy にアカウントを含めることの反証の対象**:
/// entropy を固定値にするとこのテストが落ちる（読めてしまう）。
#[test]
fn a_file_whose_account_field_was_rewritten_still_cannot_be_decrypted() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    s.store(ACCOUNT_A, SECRET).unwrap();
    let bytes = std::fs::read(s.path_for(ACCOUNT_A)).unwrap();
    let blob = format::decode(&bytes).unwrap().blob.to_vec();
    std::fs::write(s.path_for(ACCOUNT_B), format::encode(ACCOUNT_B, &blob)).unwrap();

    let err = s.load(ACCOUNT_B).unwrap_err();
    assert!(matches!(err, SecretStoreError::Unprotect { .. }), "{err:?}");
    assert_no_secret(&format!("{err} {err:?}"));
}

#[test]
fn corrupt_files_are_rejected_without_leaking_the_secret() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    s.store(ACCOUNT_A, SECRET).unwrap();
    let path = s.path_for(ACCOUNT_A);
    let good = std::fs::read(&path).unwrap();

    let mut cases: Vec<(Vec<u8>, CorruptKind)> = Vec::new();
    let mut magic = good.clone();
    magic[..4].copy_from_slice(b"XSS1");
    cases.push((magic, CorruptKind::BadMagic));
    let mut version = good.clone();
    version[4..6].copy_from_slice(&9u16.to_le_bytes());
    cases.push((version, CorruptKind::UnsupportedVersion(9)));
    cases.push((good[..good.len() - 1].to_vec(), CorruptKind::Truncated));
    let mut trailing = good.clone();
    trailing.extend_from_slice(b"junk");
    cases.push((trailing, CorruptKind::TrailingBytes));
    let mut long = good.clone();
    long[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
    cases.push((long, CorruptKind::AccountTooLong));
    cases.push((
        vec![0u8; format::MAX_FILE_LEN + 10],
        CorruptKind::FileTooLarge,
    ));

    for (bytes, kind) in cases {
        std::fs::write(&path, &bytes).unwrap();
        let err = s.load(ACCOUNT_A).unwrap_err();
        assert_eq!(err, SecretStoreError::Corrupt(kind));
        assert_no_secret(&format!("{err} {err:?}"));
        let via_keystore = KeyStore::get(&s, ACCOUNT_A).unwrap_err();
        assert_eq!(via_keystore.kind(), ErrorKind::KeyStore);
        assert_no_secret(&format!("{via_keystore} {via_keystore:?}"));
    }
}

#[test]
fn a_non_utf8_plaintext_is_reported_as_corrupt() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    let blob = PlainProtector
        .protect(&[0xFF, 0xFE], &entropy_for(ACCOUNT_A))
        .unwrap();
    std::fs::write(s.path_for(ACCOUNT_A), format::encode(ACCOUNT_A, &blob)).unwrap();
    assert_eq!(
        s.load(ACCOUNT_A),
        Err(SecretStoreError::Corrupt(CorruptKind::SecretNotUtf8))
    );
}

/// 書き込みの途中で失敗させても、元のファイルが残り、一時ファイルも残らない。
#[test]
fn a_failed_write_keeps_the_original_and_leaves_no_temp_file() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    s.store(ACCOUNT_A, SECRET).unwrap();
    let before = std::fs::read(s.path_for(ACCOUNT_A)).unwrap();

    for (stage, op) in [
        (WriteStage::TempCreated, IoOp::WriteTemp),
        (WriteStage::BeforeReplace, IoOp::Replace),
    ] {
        s.fail_writes_at(Some(stage));
        let err = s.store(ACCOUNT_A, "replacement").unwrap_err();
        assert!(
            matches!(err, SecretStoreError::Io { op: o, .. } if o == op),
            "{stage:?}: {err:?}"
        );
        assert_no_secret(&format!("{err} {err:?}"));
        assert_eq!(
            std::fs::read(s.path_for(ACCOUNT_A)).unwrap(),
            before,
            "{stage:?}"
        );
        assert_eq!(
            entries(root.path()),
            vec![file_name_for(ACCOUNT_A)],
            "{stage:?}"
        );
        assert_eq!(
            s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
            Some(SECRET)
        );
    }

    // 初めての書き込みが失敗しても、何も残らない。
    s.fail_writes_at(Some(WriteStage::BeforeReplace));
    assert!(s.store(ACCOUNT_B, "b").is_err());
    assert!(s.load(ACCOUNT_B).unwrap().is_none());
    assert_eq!(entries(root.path()), vec![file_name_for(ACCOUNT_A)]);

    s.fail_writes_at(None);
    s.store(ACCOUNT_A, "replacement").unwrap();
    assert_eq!(
        s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
        Some("replacement")
    );
}

/// 段階の hook の中で panic しても（取消しの代わり）、一時ファイルは残らない。
#[test]
fn a_panic_mid_write_leaves_no_temp_file() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();
    let result = std::panic::catch_unwind(|| {
        let _ = write_atomic(&dir, "x.bin", b"data", |stage| {
            if stage == WriteStage::BeforeReplace {
                panic!("取消し");
            }
            Ok(())
        });
    });
    assert!(result.is_err());
    assert!(
        entries(root.path()).is_empty(),
        "{:?}",
        entries(root.path())
    );
}

/// 並行の書き込み: どの瞬間もファイルは丸ごとのどれかの値で、最後に一時ファイルが残らない。
#[test]
fn concurrent_writes_and_reads_never_see_a_torn_file() {
    let root = tempfile::tempdir().unwrap();
    let s = Arc::new(store(root.path()));
    let values: Vec<String> = (0..8)
        .map(|i| format!("value-{i}-{}", "z".repeat(i * 37)))
        .collect();
    let mut threads = Vec::new();
    for value in values.clone() {
        let s = Arc::clone(&s);
        threads.push(std::thread::spawn(move || {
            for _ in 0..25 {
                s.store(ACCOUNT_A, &value).unwrap();
                s.store(ACCOUNT_B, &value).unwrap();
            }
        }));
    }
    for _ in 0..2 {
        let s = Arc::clone(&s);
        let values = values.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..100 {
                if let Some(v) = s.load(ACCOUNT_A).unwrap() {
                    assert!(values.contains(&*v), "壊れた値: {}", v.len());
                }
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    for account in [ACCOUNT_A, ACCOUNT_B] {
        let v = s.load(account).unwrap().unwrap();
        assert!(values.contains(&*v));
    }
    assert_eq!(entries(root.path()), {
        let mut v = vec![file_name_for(ACCOUNT_A), file_name_for(ACCOUNT_B)];
        v.sort();
        v
    });
}

#[test]
fn keystore_adapter_follows_the_contract() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    assert_eq!(KeyStore::get(&s, ACCOUNT_A).unwrap(), None);
    KeyStore::set(&s, ACCOUNT_A, SECRET).unwrap();
    assert_eq!(
        KeyStore::get(&s, ACCOUNT_A).unwrap().as_deref(),
        Some(SECRET)
    );
    KeyStore::delete(&s, ACCOUNT_A).unwrap();
    KeyStore::delete(&s, ACCOUNT_A).unwrap();
    assert_eq!(KeyStore::get(&s, ACCOUNT_A).unwrap(), None);

    // `Arc<dyn KeyStore>` として bootstrap に渡せる。
    let dyn_store: Arc<dyn KeyStore> = Arc::new(store(root.path()));
    dyn_store.set(ACCOUNT_B, "x").unwrap();

    let err = KeyStore::set(&s, "", SECRET).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::KeyStore);
    assert_no_secret(&format!("{err} {err:?}"));
}

#[test]
fn debug_output_never_contains_the_secret() {
    let root = tempfile::tempdir().unwrap();
    let s = store(root.path());
    s.store(ACCOUNT_A, SECRET).unwrap();
    let loaded = s.load(ACCOUNT_A).unwrap().unwrap();
    assert_eq!(&*loaded, SECRET);
    let shown = format!("{s:?}");
    assert_no_secret(&shown);
    assert!(shown.contains("FileSecretStore"), "{shown}");
}

/// 握ったフォルダ（`Mode::Plain`、全 OS）の `open_in`（#623 PR-3）: 読む・書く・消すが
/// ハンドル基準の `ManagedDir` で動き、一時ファイルを残さない。
#[test]
fn open_in_round_trips_through_a_held_plain_dir() {
    let root = tempfile::tempdir().unwrap();
    let held = banto_safe_root::open_root(&banto_safe_root::RootSpec {
        expected: root.path().join("root"),
        known: banto_safe_root::guard::KnownFolders::default(),
        owners: Vec::new(),
        create_access: None,
        create: true,
        mode: banto_safe_root::Mode::Plain,
    })
    .unwrap();
    let mut set = held
        .open_managed(
            &[banto_safe_root::ManagedSpec {
                key: "keys",
                parent: None,
                name: "keys",
                create: true,
            }],
            None,
        )
        .unwrap();
    let keys = set.take("keys").unwrap();
    let dir = keys.final_path().to_path_buf();
    // 確かめは `Mode::Plain` では何もしないので、どの OS でも `service_only` で通る。
    let s =
        FileSecretStore::open_in(keys, crate::DirAccess::service_only(), PlainProtector).unwrap();
    assert_eq!(s.dir(), dir.as_path());
    assert_eq!(s.load(ACCOUNT_A).unwrap(), None);
    s.store(ACCOUNT_A, SECRET).unwrap();
    assert_eq!(entries(&dir), vec![file_name_for(ACCOUNT_A)]);
    assert_eq!(
        s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
        Some(SECRET)
    );
    s.store(ACCOUNT_A, "second").unwrap();
    assert_eq!(
        s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
        Some("second")
    );
    assert_eq!(
        entries(&dir),
        vec![file_name_for(ACCOUNT_A)],
        "一時ファイルが残った"
    );
    s.remove(ACCOUNT_A).unwrap();
    s.remove(ACCOUNT_A).unwrap();
    assert!(entries(&dir).is_empty());
    assert_eq!(s.load(ACCOUNT_A).unwrap(), None);
}

#[cfg(not(windows))]
#[test]
fn opening_with_an_acl_fails_closed_off_windows() {
    let root = tempfile::tempdir().unwrap();
    let err = FileSecretStore::open(
        root.path(),
        crate::DirAccess::service_only(),
        PlainProtector,
    )
    .unwrap_err();
    assert_eq!(
        err,
        SecretStoreError::Root(SafeRootError::UnsupportedPlatform)
    );
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::{DirAccess, DpapiMachine};
    use banto_safe_root::acl::{
        current_user_sid, read_dir_dacl, reset_dir_acl, MODIFY_ACCESS_MASK,
    };
    use banto_safe_root::AclMismatch;

    fn access() -> DirAccess {
        DirAccess::with_install_user(current_user_sid().unwrap())
    }

    #[test]
    fn dpapi_store_round_trips_and_never_writes_the_plaintext() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("secrets").join("hub-keys");
        let s = FileSecretStore::open(&dir, access(), DpapiMachine).unwrap();
        s.store(ACCOUNT_A, SECRET).unwrap();
        let bytes = std::fs::read(s.path_for(ACCOUNT_A)).unwrap();
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
            "ファイルに平文が入っている"
        );
        assert_eq!(
            s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
            Some(SECRET)
        );

        // 開き直しても（別のインスタンス = 再起動の代わり）読める。
        let again = FileSecretStore::open(&dir, access(), DpapiMachine).unwrap();
        assert_eq!(
            KeyStore::get(&again, ACCOUNT_A).unwrap().as_deref(),
            Some(SECRET)
        );

        // 中のファイルはディレクトリの ACE を継承している（利用者に変更）。
        let file_dacl = read_dir_dacl(&s.path_for(ACCOUNT_A)).unwrap();
        let me = current_user_sid().unwrap();
        assert!(file_dacl
            .aces
            .unwrap()
            .iter()
            .any(|a| a.sid == me.as_str() && a.mask == MODIFY_ACCESS_MASK && a.flags & 0x10 != 0));
    }

    #[test]
    fn dpapi_rejects_a_rewritten_account_field() {
        let root = tempfile::tempdir().unwrap();
        let s = FileSecretStore::open(root.path().join("k"), access(), DpapiMachine).unwrap();
        s.store(ACCOUNT_A, SECRET).unwrap();
        let bytes = std::fs::read(s.path_for(ACCOUNT_A)).unwrap();
        let blob = format::decode(&bytes).unwrap().blob.to_vec();
        std::fs::write(s.path_for(ACCOUNT_B), format::encode(ACCOUNT_B, &blob)).unwrap();
        let err = s.load(ACCOUNT_B).unwrap_err();
        assert!(
            matches!(err, SecretStoreError::Unprotect { os_error: Some(_) }),
            "{err:?}"
        );
        assert_no_secret(&format!("{err} {err:?}"));
    }

    /// 開いた後で ACL が変えられたら、それ以降は読まない・書かない。
    #[test]
    fn a_changed_acl_stops_reads_and_writes() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("k");
        let s = FileSecretStore::open(&dir, access(), DpapiMachine).unwrap();
        s.store(ACCOUNT_A, SECRET).unwrap();

        // サービスだけの形に直す（= 期待と違う形）。所有者は自分なので書き換えられる。
        reset_dir_acl(&dir, &DirAccess::service_only()).unwrap();
        for err in [
            s.load(ACCOUNT_A).map(|_| ()).unwrap_err(),
            s.store(ACCOUNT_A, "x").unwrap_err(),
            s.remove(ACCOUNT_A).unwrap_err(),
        ] {
            assert!(
                matches!(
                    err,
                    SecretStoreError::Root(SafeRootError::AclMismatch(
                        AclMismatch::MissingAce { .. }
                    ))
                ),
                "{err:?}"
            );
            assert_no_secret(&format!("{err} {err:?}"));
        }

        // 明示的に直せば、また使える。
        reset_dir_acl(&dir, &access()).unwrap();
        assert_eq!(
            s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
            Some(SECRET)
        );
    }

    #[test]
    fn an_existing_directory_with_inherited_acl_is_refused_and_left_untouched() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("pre-existing");
        std::fs::create_dir(&dir).unwrap();
        let err = FileSecretStore::open(&dir, access(), DpapiMachine).unwrap_err();
        assert_eq!(
            err,
            SecretStoreError::Root(SafeRootError::AclMismatch(AclMismatch::NotProtected))
        );
        assert!(!read_dir_dacl(&dir).unwrap().protected, "黙って直さない");
        assert!(entries(&dir).is_empty());
    }

    /// #595 P2: グループの SID に「変更」を付けた**既存の**ディレクトリも、
    /// `FileSecretStore::open` は ACL の形の確認より前に SID の検証で拒む。
    /// （`banto-safe-root` の `acl` のテストから、`FileSecretStore` に依存する 1 件を
    /// #623 PR-1a で移した。）
    #[test]
    fn open_refuses_an_existing_directory_granting_a_group_sid() {
        let me = current_user_sid().unwrap();
        let machine = me.as_str().rsplit_once('-').unwrap().0;
        let group = banto_safe_root::InstallUserSid::parse(&format!("{machine}-513")).unwrap();
        let group_access = DirAccess::with_install_user(group);
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("granted-to-group");
        // open は ACL の形より前に SID を確かめるので、普通に作ったフォルダで足りる。
        std::fs::create_dir(&dir).unwrap();
        assert!(dir.exists());

        let err = FileSecretStore::open(&dir, group_access.clone(), PlainProtector).unwrap_err();
        assert!(
            matches!(
                err,
                SecretStoreError::Root(SafeRootError::InvalidUserSid(_))
            ),
            "{err:?}"
        );

        // 片付け（所有者は自分なので書き換えられる）。
        reset_dir_acl(&dir, &DirAccess::with_install_user(me)).unwrap();
    }

    /// 握ったフォルダ（`Mode::Guarded`）の `open_in`（#623 PR-3）: 開くときと読む・書く・消す
    /// たびに握ったハンドルで記述子を確かめ直し、外から ACL を変えられたら止まる
    /// （`a_changed_acl_stops_reads_and_writes` の握った版）。確かめは `recheck` なので、
    /// 期待の形で作ったフォルダは SID の確かめにも通る。
    #[test]
    fn open_in_rechecks_the_held_dir_on_every_access() {
        let root = tempfile::tempdir().unwrap();
        let held = banto_safe_root::open_root(&banto_safe_root::RootSpec {
            expected: root.path().join("root"),
            known: banto_safe_root::guard::KnownFolders::default(),
            owners: vec![current_user_sid().unwrap()],
            create_access: None,
            create: true,
            mode: banto_safe_root::Mode::Guarded,
        })
        .unwrap();
        // 作るのと同時に保護された記述子（所有者 Administrators）を付けるのは昇格した管理者
        // だけなので、ここでは継承で作ってから、所有者（自分）として期待の形に書き直す。
        let mut set = held
            .open_managed(
                &[banto_safe_root::ManagedSpec {
                    key: "keys",
                    parent: None,
                    name: "keys",
                    create: true,
                }],
                None,
            )
            .unwrap();
        let keys = set.take("keys").unwrap();
        // 2 本目は、書き直す前（自分が所有者で `WRITE_DAC` を暗黙に持つ間）に握っておく。
        let keys_again = set_take_again(&held);
        let dir = keys.final_path().to_path_buf();
        reset_dir_acl(&dir, &access()).unwrap();
        // 運用モードの形を求めると、設定モードの形のフォルダは開く時点で断る。
        let err = FileSecretStore::open_in(
            keys_again,
            access().to_right(banto_safe_root::UserRight::Read),
            DpapiMachine,
        )
        .unwrap_err();
        assert!(
            matches!(err, SecretStoreError::Root(SafeRootError::AclMismatch(_))),
            "{err:?}"
        );
        let s = FileSecretStore::open_in(keys, access(), DpapiMachine).unwrap();
        s.store(ACCOUNT_A, SECRET).unwrap();
        assert_eq!(
            s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
            Some(SECRET)
        );
        // 外から形を変える（所有者は自分なので書き換えられる）→ 以後は読まない・書かない・消さない。
        reset_dir_acl(&dir, &DirAccess::service_only()).unwrap();
        for err in [
            s.load(ACCOUNT_A).map(|_| ()).unwrap_err(),
            s.store(ACCOUNT_A, "x").unwrap_err(),
            s.remove(ACCOUNT_A).unwrap_err(),
        ] {
            assert!(
                matches!(
                    err,
                    SecretStoreError::Root(SafeRootError::AclMismatch(
                        AclMismatch::MissingAce { .. }
                    ))
                ),
                "{err:?}"
            );
            assert_no_secret(&format!("{err} {err:?}"));
        }
        reset_dir_acl(&dir, &access()).unwrap();
        assert_eq!(
            s.load(ACCOUNT_A).unwrap().as_deref().map(String::as_str),
            Some(SECRET)
        );
        s.remove(ACCOUNT_A).unwrap();
        assert!(entries(&dir).is_empty());
    }

    /// 同じ `keys` をもう 1 本握る（`open_in` の断りを見るため）。
    fn set_take_again(held: &banto_safe_root::SafeRoot) -> banto_safe_root::ManagedDir {
        held.open_managed(
            &[banto_safe_root::ManagedSpec {
                key: "keys",
                parent: None,
                name: "keys",
                create: false,
            }],
            None,
        )
        .unwrap()
        .take("keys")
        .unwrap()
    }
}
