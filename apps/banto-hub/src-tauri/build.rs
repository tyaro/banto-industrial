fn main() {
    // T16: リモート origin（`remote.urls` で許可した navigate 先）からは、
    // ACL マニフェストにアプリ自身のコマンドが登録されていないと
    // `generate_handler!` に載せただけでは呼べず "Plugin not found" になる
    // （tauri 2.11.5 `ipc/authority.rs` の ACL 解決ロジックで確認済み）。
    // `AppManifest::commands` でこの crate の4コマンドを宣言し、
    // `allow-<command>` permission を自動生成させて capabilities 側から
    // 許可できるようにする。
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "host_switch_status",
            "switch_to_service",
            "switch_to_desktop",
            "set_service_autostart",
        ]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}
