//! T17-2 スライス2（docs/banto-hub-t17-design.md §3「T17-2」）: `bin/banto_hub/
//! win_service.rs` にあった `install`/`uninstall` の本体をこの lib crate へ
//! 移設したもの。
//!
//! ## なぜ移設したか
//!
//! 新設の UAC 昇格ヘルパー `banto-hub-elev.exe`（`src/bin/banto-hub-elev.rs`、
//! [`crate::service_elevated`] 参照）は `service-install`/`service-uninstall`
//! アクションで同じ SCM 登録・登録解除ロジックを実行する必要がある。
//! `banto-hub-elev.exe` は `banto-hub.exe` とは別のバイナリターゲット
//! （どちらも `apps/banto-hub/core` パッケージの `[[bin]]`）であり、
//! `src/bin/banto_hub/win_service.rs` は `banto-hub.exe` 側の crate root
//! （`src/bin/banto-hub.rs`）だけが `mod win_service;` する非公開モジュール
//! なので、そのままでは `banto-hub-elev.exe` から呼べない。この lib crate
//! （両バイナリが依存する `banto-hub-core`）へ実装を移設し、両方から呼べる
//! 単一のソースにした。
//!
//! ## 挙動は一切変えていない
//!
//! [`install`]/[`uninstall`] の中身は、元の `win_service.rs::install`/
//! `uninstall`（println!/eprintln! の文言・分岐・`ServiceInfo` の内容・
//! 起動種別が常に `AutoStart` + 遅延自動開始であること）をそのまま移した
//! だけで、**1バイトも変えていない**（P4「Demand 化」は T17-4 のスコープの
//! ままで、このスライスでは着手しない）。`win_service.rs::install`/
//! `uninstall` は現在この2関数への1行委譲になっており、
//! `banto-hub.exe install`/`uninstall` の出力・挙動は移設前と同一。
//!
//! 唯一の差分は [`install`] が `exe_path_override` 引数を取れるようにした
//! ことだけ - `banto-hub.exe install`（`win_service::install`が`None`を渡す）
//! は今までどおり `std::env::current_exe()` で自分自身を登録対象にするが、
//! `banto-hub-elev.exe`（`service_elevated::service_install`）は自分自身
//! ではなく同じディレクトリの `banto-hub.exe` を登録対象にする必要がある
//! ため、明示的にパスを渡せるようにした。
//!
//! `SERVICE_DISPLAY_NAME`/`SERVICE_TYPE` は `win_service.rs` のトップレベル
//! 定数、`service_manager.rs` の `windows_impl::SERVICE_DISPLAY_NAME`/
//! `SERVICE_TYPE`（`set_auto_start`の再登録用）と同じ値を持つ - 既に
//! `service_manager.rs`のモジュール doc が「両ファイルを同時に直すこと」と
//! 明記している2箇所複製に、このファイルで3箇所目が増える。値を変える
//! ときは3箇所とも直すこと。

#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows_service::service::{
    ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState, ServiceType,
};
#[cfg(windows)]
use windows_service::service_manager::{ServiceManager as WinScm, ServiceManagerAccess};

#[cfg(windows)]
use crate::service_manager::{RUN_SERVICE_ARG, SERVICE_NAME};

/// `win_service.rs`のモジュール doc「サービス名・表示名・起動種別」参照。
#[cfg(windows)]
const SERVICE_DISPLAY_NAME: &str = "banto-hub タグサーバー";
#[cfg(windows)]
const SERVICE_DESCRIPTION: &str =
    "banto-hub（産業用タグサーバー）を常駐実行します。PLC からタグを収集し、REST/WebSocket/MQTT/gRPC で外部へ公開します。docs/tag-server-design.md 参照。";
#[cfg(windows)]
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// Windows サービスとして登録する（管理者権限が必要 - 失敗時はその旨を
/// 案内して終了する）。登録するバイナリパスには`RUN_SERVICE_ARG`を起動
/// 引数として含める。
///
/// `exe_path_override`: `None`なら`std::env::current_exe()`で自分自身を
/// 登録対象にする（`banto-hub.exe install`の従来どおりの挙動）。`Some`が
/// 渡された場合はそのパスをそのまま登録対象にする（`banto-hub-elev.exe`が
/// 同じディレクトリの`banto-hub.exe`を指すために使う - モジュール doc
/// 参照）。
///
/// 冪等ではない - 既に同名のサービスが存在する場合は`create_service`が
/// 失敗する（Windows API の挙動そのまま）。再登録したい場合は先に
/// [`uninstall`]すること（docs/banto-hub-operations.md に手順を記載）。
///
/// 失敗時は`eprintln!`で案内した上で`std::process::exit(1)`する（CLI ツール
/// としての既存挙動そのまま - モジュール doc「挙動は一切変えていない」）。
#[cfg(windows)]
pub fn install(exe_path_override: Option<&Path>) {
    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = match WinScm::local_computer(None::<&str>, manager_access) {
        Ok(manager) => manager,
        Err(err) => fail(&format!(
            "banto-hub: Service Control Manager への接続に失敗しました: {err}"
        )),
    };

    let exe_path: PathBuf = match exe_path_override {
        Some(path) => path.to_path_buf(),
        None => match std::env::current_exe() {
            Ok(path) => path,
            Err(err) => fail(&format!(
                "banto-hub: 自身の実行ファイルパスの取得に失敗しました: {err}"
            )),
        },
    };

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path.clone(),
        launch_arguments: vec![OsString::from(RUN_SERVICE_ARG)],
        dependencies: vec![],
        // LocalSystem として実行（`win_service.rs`のモジュール doc参照）。
        account_name: None,
        account_password: None,
    };

    let service = match service_manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)
    {
        Ok(service) => service,
        Err(err) => fail(&format!("banto-hub: サービスの登録に失敗しました: {err}")),
    };

    if let Err(err) = service.set_description(SERVICE_DESCRIPTION) {
        eprintln!("banto-hub: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}");
    }
    // 遅延自動開始（`win_service.rs`のモジュール doc参照）。
    if let Err(err) = service.set_delayed_auto_start(true) {
        eprintln!("banto-hub: 遅延自動開始の設定に失敗しました（登録自体は完了）: {err}");
    }

    println!("banto-hub: Windows サービス '{SERVICE_NAME}' を登録しました");
    println!("banto-hub:   表示名: {SERVICE_DISPLAY_NAME}");
    println!("banto-hub:   実行ファイル: {}", exe_path.display());
    println!("banto-hub:   起動種別: 自動（遅延開始）");
    println!("banto-hub: `Start-Service {SERVICE_NAME}` または OS 再起動で開始します");
}

/// サービス登録を解除する（管理者権限が必要）。実行中なら先に停止する。
#[cfg(windows)]
pub fn uninstall() {
    let manager_access = ServiceManagerAccess::CONNECT;
    let service_manager = match WinScm::local_computer(None::<&str>, manager_access) {
        Ok(manager) => manager,
        Err(err) => fail(&format!(
            "banto-hub: Service Control Manager への接続に失敗しました: {err}"
        )),
    };

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match service_manager.open_service(SERVICE_NAME, service_access) {
        Ok(service) => service,
        Err(err) => fail(&format!(
            "banto-hub: サービス '{SERVICE_NAME}' が見つかりません（既に未登録の可能性）: {err}"
        )),
    };

    match service.query_status() {
        Ok(status) if status.current_state != ServiceState::Stopped => {
            if let Err(err) = service.stop() {
                eprintln!("banto-hub: サービスの停止に失敗しました: {err}");
            } else {
                println!("banto-hub: サービスを停止しました");
            }
        }
        Ok(_) => {}
        Err(err) => eprintln!("banto-hub: サービス状態の取得に失敗しました: {err}"),
    }

    if let Err(err) = service.delete() {
        fail(&format!("banto-hub: サービスの削除に失敗しました: {err}"));
    }

    println!("banto-hub: Windows サービス '{SERVICE_NAME}' の登録を解除しました");
    println!(
        "banto-hub: （このプロセスのハンドルが閉じるまで完全な削除が遅延することがあります - Windows API の仕様）"
    );
}

#[cfg(windows)]
fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
