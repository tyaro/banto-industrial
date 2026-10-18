//! [`super`] の純関数のテスト（全 OS）。Windows の実物（締める・緩める・印・確かめ直し）は
//! `win_tests`。

use super::*;
use crate::acl::{check_dacl, ObservedAce};
use crate::guard::{plan_carry_over, plan_carry_over_at_least, AccountKind, DropReason};

#[cfg(windows)]
mod win_tests;

const ME: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
const OTHER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
const THIRD: &str = "S-1-5-21-1111111111-2222222222-3333333333-1003";
/// `FILE_GENERIC_READ`（実行なし。「読み取りと実行」に足りない）。
const READ_ONLY_MASK: u32 = 0x0012_0089;

fn sid(s: &str) -> InstallUserSid {
    InstallUserSid::parse(s).unwrap()
}

fn ace(sid: &str, mask: u32) -> ObservedAce {
    ObservedAce {
        ace_type: ACCESS_ALLOWED_ACE_TYPE,
        flags: INHERIT_FLAGS,
        mask,
        sid: sid.to_owned(),
    }
}

/// 保護された DACL（SYSTEM・Administrators に FA）に `users` の ACE を足した記述子。
fn protected(owner: &str, users: &[(&str, u32)]) -> ObservedDacl {
    let mut aces = vec![
        ace(SYSTEM_SID, FULL_ACCESS_MASK),
        ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
    ];
    aces.extend(users.iter().map(|(sid, mask)| ace(sid, *mask)));
    ObservedDacl {
        protected: true,
        owner: owner.to_owned(),
        aces: Some(aces),
    }
}

#[test]
fn readers_access_has_the_read_execute_mask_and_round_trips() {
    let readers = DirAccess::with_readers([sid(ME), sid(OTHER), sid(ME)]);
    assert_eq!(readers.right(), UserRight::Read);
    assert_eq!(readers.install_users(), &[sid(ME), sid(OTHER)][..]);
    assert_eq!(
        readers.sddl(),
        format!(
            "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1200a9;;;{ME})(A;OICI;0x1200a9;;;{OTHER})"
        )
    );
    let setting = readers.to_right(UserRight::Modify);
    assert_eq!(
        setting,
        DirAccess::with_install_users([sid(ME), sid(OTHER)])
    );
    assert_eq!(setting.to_right(UserRight::Read), readers);
    // 利用者が 0 人なら権利は常に Modify に揃える（DACL が同じなので値も同じ）。
    assert_eq!(DirAccess::with_readers([]), DirAccess::service_only());
    assert_eq!(
        DirAccess::service_only().to_right(UserRight::Read),
        DirAccess::service_only()
    );
    assert_eq!(DirAccess::with_readers([]).right(), UserRight::Modify);
    assert_eq!(UserRight::Read.mask(), READ_EXECUTE_ACCESS_MASK);
    assert_eq!(UserRight::Modify.mask(), MODIFY_ACCESS_MASK);
    // 「読み取りと実行」は名前の作成・属性の変更・DELETE・ACL の変更を含まない。
    for bit in [0x2u32, 0x4, 0x100, 0x1_0000, 0x4_0000, 0x8_0000] {
        assert_eq!(READ_EXECUTE_ACCESS_MASK & bit, 0, "{bit:#x}");
    }
}

/// `check_dacl` の運用モードの形（設計 §2.2）: 利用者の ACE は「読み取りと実行」、所有者は
/// SYSTEM・Administrators だけ（「読み取り」の利用者が所有者なら `DisallowedOwner`）。
#[test]
fn check_dacl_operation_shape_table() {
    let operation = DirAccess::with_readers([sid(ME)]);
    let setting = DirAccess::with_install_user(sid(ME));
    let read = [(ME, READ_EXECUTE_ACCESS_MASK)];
    let modify = [(ME, MODIFY_ACCESS_MASK)];

    assert_eq!(
        check_dacl(&protected(SYSTEM_SID, &read), &operation),
        Ok(())
    );
    assert_eq!(
        check_dacl(&protected(ADMINISTRATORS_SID, &read), &operation),
        Ok(())
    );
    // 「読み取り」の利用者が所有者 = 暗黙の WRITE_DAC で戻せる → 許さない。
    assert_eq!(
        check_dacl(&protected(ME, &read), &operation),
        Err(AclMismatch::DisallowedOwner { sid: ME.to_owned() })
    );
    // 「変更」の利用者なら所有者でよい（今までどおり）。
    assert_eq!(check_dacl(&protected(ME, &modify), &setting), Ok(()));
    // 形の取り違え（設定モードの形を運用モードの期待で、逆も）は UnexpectedAce。
    assert!(matches!(
        check_dacl(&protected(SYSTEM_SID, &modify), &operation),
        Err(AclMismatch::UnexpectedAce {
            mask: MODIFY_ACCESS_MASK,
            ..
        })
    ));
    assert!(matches!(
        check_dacl(&protected(SYSTEM_SID, &read), &setting),
        Err(AclMismatch::UnexpectedAce {
            mask: READ_EXECUTE_ACCESS_MASK,
            ..
        })
    ));
}

#[test]
fn mode_access_needs_at_least_one_user() {
    assert_eq!(ModeAccess::for_users([]), None);
    let mode = ModeAccess::for_users([sid(OTHER), sid(ME), sid(OTHER)]).unwrap();
    assert_eq!(mode.users(), &[sid(OTHER), sid(ME)][..]);
    assert_eq!(
        mode.setting(),
        &DirAccess::with_install_users([sid(OTHER), sid(ME)])
    );
    assert_eq!(
        mode.operation(),
        &DirAccess::with_readers([sid(OTHER), sid(ME)])
    );
    assert_eq!(mode.setting().right(), UserRight::Modify);
    assert_eq!(mode.operation().right(), UserRight::Read);
}

/// `classify_mode` の総当たり（設計 §2.10・§8 PR-1c の純関数）。継承の既定の DACL
/// （`create_dir_all` で作ったフォルダ）→ `Unexpected`、開けない → `NoAccessForMe` を含む
/// （Fable-2 A-2・B-1）。
#[test]
fn classify_mode_table() {
    use ModeObservation as M;
    let me = sid(ME);
    let present =
        |owner: &str, users: &[(&str, u32)]| RootObservation::Present(protected(owner, users));
    let read = READ_EXECUTE_ACCESS_MASK;
    let modify = MODIFY_ACCESS_MASK;

    let cases: Vec<(&str, RootObservation, M)> = vec![
        ("無い", RootObservation::Absent, M::Absent),
        ("開けない", RootObservation::Denied, M::NoAccessForMe),
        (
            "設定・所有者は自分",
            present(ME, &[(ME, modify)]),
            M::Setting,
        ),
        (
            "設定・所有者 Administrators",
            present(ADMINISTRATORS_SID, &[(ME, modify)]),
            M::Setting,
        ),
        (
            "設定・所有者 SYSTEM",
            present(SYSTEM_SID, &[(ME, modify)]),
            M::Setting,
        ),
        (
            "設定・複数の利用者（Hub の owner の組）",
            present(OTHER, &[(OTHER, modify), (ME, modify)]),
            M::Setting,
        ),
        (
            "設定・所有者が組に無い別の利用者",
            present(THIRD, &[(ME, modify)]),
            M::Unexpected(AclMismatch::DisallowedOwner {
                sid: THIRD.to_owned(),
            }),
        ),
        (
            "設定の形だが自分の ACE が無い",
            present(OTHER, &[(OTHER, modify)]),
            M::NoAccessForMe,
        ),
        (
            "運用・自分が読める",
            present(ADMINISTRATORS_SID, &[(ME, read)]),
            M::Operation,
        ),
        (
            "運用・複数の利用者",
            present(SYSTEM_SID, &[(OTHER, read), (ME, read)]),
            M::Operation,
        ),
        (
            "運用の形だが自分の ACE が無い → それでも Operation（Desktop にしない）",
            present(ADMINISTRATORS_SID, &[(OTHER, read)]),
            M::Operation,
        ),
        (
            "運用の形で所有者が利用者（締めた木ではありえない）",
            present(ME, &[(ME, read)]),
            M::Unexpected(AclMismatch::DisallowedOwner { sid: ME.to_owned() }),
        ),
        (
            "SYSTEM・Administrators だけ（利用者の ACE 無し）",
            present(ADMINISTRATORS_SID, &[]),
            M::NoAccessForMe,
        ),
        (
            "継承の既定の DACL（create_dir_all）",
            RootObservation::Present(ObservedDacl {
                protected: false,
                owner: ME.to_owned(),
                aces: Some(vec![
                    ObservedAce {
                        flags: INHERIT_FLAGS | crate::acl::INHERITED_ACE_FLAG,
                        ..ace(SYSTEM_SID, FULL_ACCESS_MASK)
                    },
                    ObservedAce {
                        flags: INHERIT_FLAGS | crate::acl::INHERITED_ACE_FLAG,
                        ..ace("S-1-5-32-545", 0x1200a9)
                    },
                ]),
            }),
            M::Unexpected(AclMismatch::NotProtected),
        ),
        (
            "保護されているが継承の ACE が残る",
            RootObservation::Present(ObservedDacl {
                aces: Some(vec![
                    ace(SYSTEM_SID, FULL_ACCESS_MASK),
                    ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
                    ObservedAce {
                        flags: INHERIT_FLAGS | crate::acl::INHERITED_ACE_FLAG,
                        ..ace(ME, modify)
                    },
                ]),
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::InheritedAce { sid: ME.to_owned() }),
        ),
        (
            "null DACL",
            RootObservation::Present(ObservedDacl {
                aces: None,
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::NoDacl),
        ),
        (
            "SYSTEM の FA が無い",
            RootObservation::Present(ObservedDacl {
                aces: Some(vec![
                    ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
                    ace(ME, modify),
                ]),
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::MissingAce {
                sid: SYSTEM_SID.to_owned(),
                mask: FULL_ACCESS_MASK,
            }),
        ),
        (
            "Administrators の FA が無い",
            RootObservation::Present(ObservedDacl {
                aces: Some(vec![ace(SYSTEM_SID, FULL_ACCESS_MASK), ace(ME, modify)]),
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::MissingAce {
                sid: ADMINISTRATORS_SID.to_owned(),
                mask: FULL_ACCESS_MASK,
            }),
        ),
        (
            "Administrators が FA でない",
            RootObservation::Present(ObservedDacl {
                aces: Some(vec![
                    ace(SYSTEM_SID, FULL_ACCESS_MASK),
                    ace(ADMINISTRATORS_SID, modify),
                    ace(ME, modify),
                ]),
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: ACCESS_ALLOWED_ACE_TYPE,
                flags: INHERIT_FLAGS,
                mask: modify,
                sid: ADMINISTRATORS_SID.to_owned(),
            }),
        ),
        (
            "拒否の ACE がある",
            RootObservation::Present(ObservedDacl {
                aces: Some(vec![
                    ObservedAce {
                        ace_type: 1,
                        ..ace(OTHER, FULL_ACCESS_MASK)
                    },
                    ace(SYSTEM_SID, FULL_ACCESS_MASK),
                    ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
                    ace(ME, modify),
                ]),
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: 1,
                flags: INHERIT_FLAGS,
                mask: FULL_ACCESS_MASK,
                sid: OTHER.to_owned(),
            }),
        ),
        (
            "OI だけの ACE",
            RootObservation::Present(ObservedDacl {
                aces: Some(vec![
                    ace(SYSTEM_SID, FULL_ACCESS_MASK),
                    ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK),
                    ObservedAce {
                        flags: 0x01,
                        ..ace(ME, modify)
                    },
                ]),
                ..protected(ME, &[])
            }),
            M::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: ACCESS_ALLOWED_ACE_TYPE,
                flags: 0x01,
                mask: modify,
                sid: ME.to_owned(),
            }),
        ),
        (
            "混ざっている（自分は変更・他は読み取り）",
            present(ADMINISTRATORS_SID, &[(ME, modify), (OTHER, read)]),
            M::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: ACCESS_ALLOWED_ACE_TYPE,
                flags: INHERIT_FLAGS,
                mask: modify,
                sid: ME.to_owned(),
            }),
        ),
        (
            "利用者にフルコントロール",
            present(ADMINISTRATORS_SID, &[(ME, FULL_ACCESS_MASK)]),
            M::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: ACCESS_ALLOWED_ACE_TYPE,
                flags: INHERIT_FLAGS,
                mask: FULL_ACCESS_MASK,
                sid: ME.to_owned(),
            }),
        ),
        (
            "Users に読み取り（旧版・手で足した）",
            present(ADMINISTRATORS_SID, &[(ME, modify), ("S-1-5-32-545", read)]),
            M::Unexpected(AclMismatch::UnexpectedAce {
                ace_type: ACCESS_ALLOWED_ACE_TYPE,
                flags: INHERIT_FLAGS,
                mask: modify,
                sid: ME.to_owned(),
            }),
        ),
    ];
    for (name, observation, expected) in cases {
        assert_eq!(classify_mode(&observation, &me), expected, "{name}");
    }
    // 不変条件: 「運用モードの形」と判定するのは、利用者の ACE がすべて「読み取りと実行」で
    // 書き込みの ACE が 1 つも無いときだけ。書き込みを持つ利用者が 1 人でもいれば Operation に
    // ならない（Desktop を断る唯一の判定なので、締まっていない木を締まっていると見ない方向は
    // 問題にならないが、逆 = 緩い木を Operation と見ると利用者が起動できない）。
    for mask in [modify, FULL_ACCESS_MASK, read | 0x2, read | 0x1_0000] {
        assert_ne!(
            classify_mode(
                &present(ADMINISTRATORS_SID, &[(OTHER, read), (ME, mask)]),
                &me
            ),
            M::Operation,
            "{mask:#x}"
        );
    }
}

/// `plan_carry_over_at_least`（設計 §2.2・§2.7。Hub の緩めるときの相手 = 締まった profile の
/// `Read` の ACE の組）: 下限 Read なら「読み取りと実行」以上、下限 Modify なら今までどおり。
#[test]
fn carry_over_at_least_read_picks_readers_and_modify_holders() {
    let dacl = protected(
        ADMINISTRATORS_SID,
        &[
            (ME, READ_EXECUTE_ACCESS_MASK),
            (OTHER, MODIFY_ACCESS_MASK),
            (THIRD, READ_ONLY_MASK),
        ],
    );
    let readers = plan_carry_over_at_least(&dacl, UserRight::Read, |_| AccountKind::User);
    assert_eq!(readers.carried, vec![sid(ME), sid(OTHER)]);
    assert_eq!(readers.dropped.len(), 1);
    assert_eq!(readers.dropped[0].ace.sid, THIRD);
    assert_eq!(readers.dropped[0].reason, DropReason::LessThanModify);

    let modifiers = plan_carry_over_at_least(&dacl, UserRight::Modify, |_| AccountKind::User);
    assert_eq!(modifiers.carried, vec![sid(OTHER)]);
    assert_eq!(
        modifiers
            .dropped
            .iter()
            .map(|d| (d.ace.sid.as_str(), d.reason.clone()))
            .collect::<Vec<_>>(),
        vec![
            (ME, DropReason::LessThanModify),
            (THIRD, DropReason::LessThanModify)
        ]
    );
    // `plan_carry_over` は Modify の下限と同じ。
    assert_eq!(plan_carry_over(&dacl, |_| AccountKind::User), modifiers);
}

#[test]
fn tree_problem_reports_its_path() {
    let path = std::path::PathBuf::from("x");
    let problems = [
        TreeProblem::ReparsePoint {
            path: path.clone(),
            tag: 0xA000_0003,
        },
        TreeProblem::Hardlink {
            path: path.clone(),
            links: 2,
        },
        TreeProblem::Denied { path: path.clone() },
        TreeProblem::Busy {
            path: path.clone(),
            os_error: 32,
        },
        TreeProblem::Encrypted { path: path.clone() },
        TreeProblem::TooDeep { path: path.clone() },
    ];
    for problem in problems {
        assert_eq!(problem.path(), path.as_path());
        assert!(problem.to_string().starts_with("x: "), "{problem}");
    }
    assert!(TreeProblem::ReparsePoint {
        path,
        tag: 0xA000_0003
    }
    .to_string()
    .contains("0xa0000003"));
}

/// 印のファイルはサービス自身の掃除でも消さない（`is_removal_candidate`・`remove`）。Plain は
/// 印を置かず（`Skipped`）、確かめ直しは常に通る。Plain の `final_path` は verbatim（`\\?\`）
/// 形ではない（#677 のレビュー N-3）。
#[test]
fn plain_root_skips_the_marker_and_never_removes_it() {
    use crate::root::{is_removal_candidate, open_root, Entry, EntryKind, Mode, RootSpec};
    let entry = Entry {
        name: MARKER_NAME.to_owned(),
        kind: EntryKind::File,
        reparse_tag: None,
        len: 0,
    };
    assert!(!is_removal_candidate(&entry, &|_| true));

    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&RootSpec {
        expected: dir.path().join("root"),
        known: Default::default(),
        owners: Vec::new(),
        create_access: None,
        create: true,
        mode: Mode::Plain,
    })
    .unwrap();
    assert!(
        !root.final_path().to_string_lossy().starts_with(r"\\?\"),
        "{}",
        root.final_path().display()
    );
    std::fs::write(root.final_path().join(MARKER_NAME), b"").unwrap();
    assert!(root.as_dir().remove(MARKER_NAME).is_err());
    assert!(root.final_path().join(MARKER_NAME).exists());
    assert_eq!(root.as_dir().ensure_marker(), Ok(MarkerOutcome::Skipped));
    let expected = DirAccess::with_readers([sid(ME)]);
    assert_eq!(root.as_dir().recheck(&expected), Ok(()));
    let reopen = root
        .as_dir()
        .reopen_path_checked("db.sqlite3", &["-wal"], &expected)
        .unwrap();
    assert_eq!(reopen.path(), root.final_path().join("db.sqlite3"));
    let report = root.as_dir().remove_where(&|_| true).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
}
