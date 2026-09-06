fn main() {
    // T16: リモート origin（`remote.urls` で許可した navigate 先）からは、
    // ACL マニフェストにアプリ自身のコマンドが登録されていないと
    // `generate_handler!` に載せただけでは呼べず "Plugin not found" になる
    // （tauri 2.11.5 `ipc/authority.rs` の ACL 解決ロジックで確認済み）。
    // `AppManifest::commands` でこの crate のコマンドを宣言し、
    // `allow-<command>` permission を自動生成させて capabilities 側から
    // 許可できるようにする。
    //
    // S6（docs/banto-hub-external-db-design.md §5.5・§7 row S6、2026-09-07）:
    // `sink_service_status`/`sink_service_start`/`sink_service_stop`
    // （`sink_service_ipc.rs`）を追加した - 既存4コマンドと同じ理由
    // （navigate 後の webview から呼ぶには ACL マニフェストへの宣言が要る、
    // 上のコメント参照）。
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "host_switch_status",
            "switch_to_service",
            "switch_to_desktop",
            "set_service_autostart",
            "sink_service_status",
            "sink_service_start",
            "sink_service_stop",
        ]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
