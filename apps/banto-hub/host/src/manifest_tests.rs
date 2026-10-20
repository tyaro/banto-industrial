//! R1-7a（docs/banto-driver-api-design.md §2.4）: `banto-hub-elev.exe` の
//! `requireAdministrator` マニフェストの埋め込みを `banto-hub-core` からこの package へ
//! 移したので、移動で埋め込みが落ちていないことを機械的に確かめる
//! （ChronoGazer の `apps/chronogazer/elev/src/tests.rs` の
//! `the_manifest_requires_administrator_and_the_rc_embeds_it` と同じ形）。
//!
//! `bin/banto-hub.rs` のテストとして読み込む（`#[cfg(test)] #[path] mod`）。
//! `banto-hub-elev` 自身のテストのバイナリには Windows でマニフェストが付き、
//! 非管理者ではテスト一覧の取得が os error 740 で失敗する
//! （`.config/nextest.toml` の「ローカル（非管理者）での注意」）ため、そちらには置かない。
//! `include_str!` だけなので全 OS で走る。

#[test]
fn the_manifest_requires_administrator_and_the_rc_embeds_it() {
    let manifest = include_str!("../banto-hub-elev.manifest");
    assert!(
        manifest.contains(
            r#"<requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>"#
        ),
        "{manifest}"
    );
    assert!(!manifest.contains("asInvoker"));
    assert!(!manifest.contains("highestAvailable"));
    let rc = include_str!("../banto-hub-elev.rc");
    assert!(rc.contains(r#"1 24 "banto-hub-elev.manifest""#), "{rc}");
    let build = include_str!("../build.rs");
    assert!(build.contains(r#"["banto-hub-elev"]"#));
    assert!(build.contains(".manifest_required()"));
}
