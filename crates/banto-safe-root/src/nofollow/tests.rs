//! [`super`] のテスト。純関数は全 OS、実物は Windows（ジャンクション・ハードリンクは権限なしで
//! 作れるので必ず走る。シンボリックリンクは作れなければ手元は skip、`CI` があれば落とす）。

use super::*;

#[test]
fn strip_verbatim_drops_the_verbatim_prefixes() {
    assert_eq!(
        strip_verbatim(Path::new(r"\\?\C:\Data\X.lock")),
        PathBuf::from(r"C:\Data\X.lock")
    );
    assert_eq!(
        strip_verbatim(Path::new(r"\\?\UNC\srv\share\a")),
        PathBuf::from(r"\\srv\share\a")
    );
    assert_eq!(
        strip_verbatim(Path::new(r"C:\plain")),
        PathBuf::from(r"C:\plain")
    );
}

#[cfg(windows)]
#[test]
fn normalized_expectations_match_case_prefix_and_trailing_separators() {
    use crate::guard::paths_match;
    // 旧 `banto-instance-lock` の `comparable` のテストを、`paths_match` に統一した形で残す。
    assert!(paths_match(
        &strip_verbatim(Path::new(r"\\?\C:\Data\X.lock")),
        Path::new(r"c:\data\x.lock")
    ));
    assert!(paths_match(
        &strip_verbatim(Path::new(r"\\?\UNC\srv\share\a")),
        Path::new(r"\\SRV\share\a\")
    ));
    assert!(!paths_match(
        Path::new(r"C:\data\x.lock"),
        Path::new(r"C:\outside\x.lock")
    ));
}

#[cfg(not(windows))]
#[test]
fn non_windows_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let err = open_file_verified(&dir.path().join("x"), OpenHow::Write).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(!dir.path().join("x").exists(), "何も作らない");
}

#[cfg(windows)]
mod windows {
    use super::super::*;
    use crate::acl::tests::windows::junction;
    use std::io::{Read, Write};

    const VICTIM: &[u8] = b"outside content";

    fn victim(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, VICTIM).unwrap();
        path
    }

    fn assert_untouched(path: &Path) {
        assert_eq!(
            std::fs::read(path).unwrap(),
            VICTIM,
            "外のファイルが変わった"
        );
    }

    /// シンボリックリンクを作る。作れなければ手元は `false`（skip）、`CI` では落とす。
    fn try_symlink(make: impl FnOnce() -> std::io::Result<()>) -> bool {
        match make() {
            Ok(()) => true,
            Err(err) if std::env::var_os("CI").is_some() => {
                panic!("CI でシンボリックリンクを作れない: {err}")
            }
            Err(err) => {
                eprintln!("skip: シンボリックリンクを作れません（権限?）: {err}");
                false
            }
        }
    }

    #[test]
    fn a_plain_file_is_created_then_reopened_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        match open_file_verified(&path, OpenHow::Write).unwrap() {
            Verified::File { mut file, created } => {
                assert!(created);
                file.write_all(b"hello").unwrap();
            }
            other => panic!("{other:?}"),
        }
        match open_file_verified(&path, OpenHow::Write).unwrap() {
            Verified::File { created, .. } => assert!(!created, "既にある物は作らない"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"hello",
            "開いただけで切り詰めない"
        );
        match open_file_verified(&path, OpenHow::Read).unwrap() {
            Verified::File { mut file, created } => {
                assert!(!created);
                let mut text = String::new();
                file.read_to_string(&mut text).unwrap();
                assert_eq!(text, "hello");
            }
            other => panic!("{other:?}"),
        }
        let missing = open_file_verified(&dir.path().join("none"), OpenHow::Read).unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_hard_link_is_refused_and_the_outside_is_not_touched() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = victim(outside.path(), "v.txt");
        let link = dir.path().join("x.lock");
        std::fs::hard_link(&target, &link).unwrap();
        for how in [OpenHow::Write, OpenHow::Read] {
            match open_file_verified(&link, how).unwrap() {
                Verified::Refused {
                    refusal: Refusal::HardLinked { links: 2 },
                    left_behind: None,
                } => {}
                other => panic!("{how:?}: {other:?}"),
            }
        }
        assert_untouched(&target);
    }

    #[test]
    fn a_junction_in_the_middle_is_refused_and_nothing_is_left_outside() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let existing = victim(outside.path(), "x.lock");
        let j = dir.path().join("j");
        junction(&j, outside.path());
        // 既にある外のファイル: 開き直すが、本当のパスが違うので書かずに断る。
        match open_file_verified(&j.join("x.lock"), OpenHow::Write).unwrap() {
            Verified::Refused {
                refusal: Refusal::Elsewhere { actual },
                left_behind: None,
            } => assert!(
                crate::guard::paths_match(&actual, &normalize_expected(&existing)),
                "{actual:?}"
            ),
            other => panic!("{other:?}"),
        }
        assert_untouched(&existing);
        // 無かった名前: 作ってしまうが、確かめに合わないのでハンドルで消す（外に残さない）。
        match open_file_verified(&j.join("fresh.lock"), OpenHow::Write).unwrap() {
            Verified::Refused {
                refusal: Refusal::Elsewhere { .. },
                left_behind: None,
            } => {}
            other => panic!("{other:?}"),
        }
        assert!(
            !outside.path().join("fresh.lock").exists(),
            "ジャンクションの先に作り残した"
        );
    }

    #[test]
    fn a_directory_symlink_in_the_middle_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let existing = victim(outside.path(), "x.lock");
        let link = dir.path().join("s");
        if !try_symlink(|| std::os::windows::fs::symlink_dir(outside.path(), &link)) {
            return;
        }
        match open_file_verified(&link.join("x.lock"), OpenHow::Write).unwrap() {
            Verified::Refused {
                refusal: Refusal::Elsewhere { .. },
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        assert_untouched(&existing);
        let _ = open_file_verified(&link.join("fresh.lock"), OpenHow::Write).unwrap();
        assert!(!outside.path().join("fresh.lock").exists());
    }

    #[test]
    fn a_file_symlink_is_refused_with_its_tag() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = victim(outside.path(), "v.txt");
        let link = dir.path().join("x.lock");
        if !try_symlink(|| std::os::windows::fs::symlink_file(&target, &link)) {
            return;
        }
        for how in [OpenHow::Write, OpenHow::Read] {
            match open_file_verified(&link, how).unwrap() {
                // IO_REPARSE_TAG_SYMLINK = 0xA000000C。
                Verified::Refused {
                    refusal: Refusal::ReparsePoint { tag: 0xA000_000C },
                    left_behind: None,
                } => {}
                other => panic!("{how:?}: {other:?}"),
            }
        }
        assert_untouched(&target);
    }

    /// フォルダは `FILE_FLAG_BACKUP_SEMANTICS` なしでは開けない（拒否。旧 `diag_file` と同じ）。
    #[test]
    fn a_directory_cannot_be_opened() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("d");
        std::fs::create_dir(&sub).unwrap();
        for how in [OpenHow::Write, OpenHow::Read] {
            let err = open_file_verified(&sub, how).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{how:?}");
        }
    }

    /// 8.3 の短い名前のパスでも、長い名前に直して比べるので通る（短い名前を作らないボリュームでは
    /// skip）。
    #[test]
    fn a_short_name_path_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let long_dir = dir.path().join("a long directory name");
        std::fs::create_dir(&long_dir).unwrap();
        let Some(short_dir) = crate::root::tests::short_path(&long_dir) else {
            eprintln!("skip: このボリュームは 8.3 の短い名前を作らない");
            return;
        };
        match open_file_verified(&short_dir.join("x.lock"), OpenHow::Write).unwrap() {
            Verified::File { created, .. } => assert!(created),
            other => panic!("{other:?}"),
        }
        assert!(long_dir.join("x.lock").exists());
    }
}
