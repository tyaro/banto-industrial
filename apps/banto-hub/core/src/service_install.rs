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
//! ## 挙動は一切変えていない（T17-2 移設時点）
//!
//! T17-2 スライス2の時点では、[`install`]/[`uninstall`] の中身は元の
//! `win_service.rs::install`/`uninstall`（println!/eprintln! の文言・
//! 分岐・`ServiceInfo` の内容・起動種別が常に `AutoStart` + 遅延自動開始
//! であること）をそのまま移しただけで、1バイトも変えていなかった
//! （P4「Demand 化」は当時 T17-4 のスコープとして未着手だった）。
//! `win_service.rs::install`/`uninstall` は現在この2関数への1行委譲に
//! なっており、`banto-hub.exe install`/`uninstall` の出力・挙動は
//! 移設前と同一（下記 T17-4 の変更を除く）。
//!
//! ## T17-4（P4「Demand 化」、2026-08-10）
//!
//! [`install`] の既定起動種別を `ServiceStartType::AutoStart`（+
//! `set_delayed_auto_start(true)`）から `ServiceStartType::OnDemand`
//! （手動開始）へ変更した（docs/banto-hub-t17-design.md §1「P4」・
//! §11「T17-4 実装メモ」参照）。目的は「OS 再起動だけで収集が始まる」
//! ことの防止 - サービスが実際に開始したときにサービス本体
//! （`win_service.rs::HubProgram`）が即座に `Configured` 収集を
//! 開始する既存挙動自体は変えていない。`OnDemand` には遅延自動開始の
//! 概念が存在しない（`set_delayed_auto_start`は`AutoStart`にのみ意味を
//! 持つ Windows API 仕様）ため、`OnDemand`確定後は同呼び出しを行わない。
//! 自動起動を有効化する経路（`service_manager::WindowsServiceManager::
//! set_auto_start(true)`）は本スライスでは変更していない - そちらは
//! 引き続き `AutoStart` + 遅延自動開始を組み立てる（管理 UI 等からの
//! 明示操作でのみ有効化される想定、design §1 P4 参照）。
//!
//! アップグレード時に既存サービスの起動種別を意図せず上書きしないよう
//! （design §5「上書きインストール時の既存サービス設定保持」）、
//! [`install`] は SCM に同名サービスが既に存在する場合、起動種別・
//! 実行ファイル・ACL は変えずに早期リターンする。#589 以降、その経路でも
//! OS シャットダウンの待ち時間（PRESHUTDOWN）だけは希望値でなければ更新し、
//! 読み戻して確認する（`wait_hint` では延びない。既定は build 15063 以降
//! 10 秒）。
//!
//! 唯一の差分は [`install`] が `exe_path_override` 引数を取れるようにした
//! ことだけ - `banto-hub.exe install`（`win_service::install`が`None`を渡す）
//! は今までどおり `std::env::current_exe()` で自分自身を登録対象にするが、
//! `banto-hub-elev.exe`（`service_elevated::service_install`）は自分自身
//! ではなく同じディレクトリの `banto-hub.exe` を登録対象にする必要がある
//! ため、明示的にパスを渡せるようにした。
//!
//! ## #392 B3-1（2026-10-15）: 手順を共有 crate へ
//!
//! SCM の手順（接続・既に登録済みの判定・`ServiceInfo`・説明文・PRESHUTDOWN・
//! 停止してから削除）は共有 crate `banto-service-control` の
//! `install_service`/`uninstall_service` へ移した（banto-hub-sink と同じ手順を
//! 使う）。このファイルに残るのは、結果を利用者に出す文言
//! （`install_lines`/`uninstall_lines`）と終了コードだけで、どちらも移す前と
//! 同じ（文言・出力先・順序は golden テスト、失敗は終了コード 1 -
//! `banto-hub-elev service-install` とインストーラのフックが見る）。
//!
//! 登録内容（名前・表示名・説明文・起動引数・手動開始・PRESHUTDOWN）は
//! [`crate::service_manager::hub_service_spec`] の 1 か所に寄せた（それまでは
//! 表示名などがこのファイル・`win_service.rs`・`service_manager.rs` の
//! 3 か所に複製されていた）。`WindowsServiceManager` の`set_auto_start`の
//! 再登録も同じ spec から作る（`service_manager::hub_service_manager`）。
//!
//! 唯一の差は、自分の実行ファイルのパスを SCM への接続より先に求めるように
//! なったこと（`std::env::current_exe()` の失敗と SCM への接続の失敗が
//! 同時に起きたときだけ、先に出る文言が変わる。実際には起きない組み合わせ）。

use std::path::Path;

use banto_service_control::{
    preshutdown_console_line, ConsoleLine, InstallError, InstallKind, InstallOutcome, StopAttempt,
    UninstallError, UninstallOutcome,
};

use crate::service_manager::{SERVICE_DISPLAY_NAME, SERVICE_NAME};

/// 文言の先頭に付ける製品名。
const LABEL: &str = "banto-hub";

/// Windows サービスとして登録する（管理者権限が必要 - 失敗時はその旨を
/// 案内して終了する）。登録するバイナリパスには`RUN_SERVICE_ARG`を起動
/// 引数として含める（[`crate::service_manager::hub_service_spec`]）。
///
/// `exe_path_override`: `None`なら`std::env::current_exe()`で自分自身を
/// 登録対象にする（`banto-hub.exe install`の従来どおりの挙動）。`Some`が
/// 渡された場合はそのパスをそのまま登録対象にする（`banto-hub-elev.exe`が
/// 同じディレクトリの`banto-hub.exe`を指すために使う - モジュール doc
/// 参照）。
///
/// **既に同名のサービスが登録済みの場合、起動種別・実行ファイル・ACL は
/// 変えずに早期リターンする**（T17-4、design §5「上書きインストール時の
/// 既存サービス設定保持」）。アップグレード時に NSIS post-install フックが
/// 毎回この関数を呼ぶため（`installer/hooks/service-hooks.nsh`）、ここで
/// 新規作成せず既存の `ServiceInfo`（起動種別を含む - オーナーが手動で
/// `AutoStart` に変更している場合も含む）をそのまま保持する。設定
/// （実行ファイルパス・起動種別等）を変更したい場合は、先に
/// [`uninstall`]してから明示的に`install`し直すか、管理 UI /
/// `sc.exe config`を使うこと（docs/banto-hub-operations.md に手順を記載）。
///
/// 例外は OS シャットダウンの待ち時間（#589）。新規作成でも既設でも
/// `SERVICE_CONFIG_PRESHUTDOWN_INFO` だけを希望値に揃え、読み戻す。揃え
/// られなくても install は失敗にしない（警告 1 行を標準エラー出力へ。
/// `service-install` はこの後に Operators・ACL の付与を続ける）。起動している
/// 旧バイナリは再起動するまで PRESHUTDOWN を受けない。
///
/// 失敗時は`eprintln!`で案内した上で`std::process::exit(1)`する（CLI ツール
/// としての既存挙動そのまま - モジュール doc「挙動は一切変えていない」）。
#[cfg(windows)]
pub fn install(exe_path_override: Option<&Path>) {
    use banto_service_control::{install_service, print_console_lines};

    let exe_path = match exe_path_override {
        Some(path) => path.to_path_buf(),
        None => match std::env::current_exe() {
            Ok(path) => path,
            Err(err) => fail(&format!(
                "{LABEL}: 自身の実行ファイルパスの取得に失敗しました: {err}"
            )),
        },
    };
    let result = install_service(&crate::service_manager::hub_service_spec(exe_path.clone()));
    print_console_lines(&install_lines(&exe_path, &result));
    if result.is_err() {
        std::process::exit(1);
    }
}

/// サービス登録を解除する（管理者権限が必要）。実行中なら先に停止する。
/// 失敗したら終了コード 1。
#[cfg(windows)]
pub fn uninstall() {
    use banto_service_control::{print_console_lines, uninstall_service};

    let result = uninstall_service(SERVICE_NAME);
    print_console_lines(&uninstall_lines(&result));
    if result.is_err() {
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// `install` の結果を利用者に出す行にする（移す前の文言そのまま）。
#[cfg_attr(not(windows), allow(dead_code))]
fn install_lines(
    exe_path: &Path,
    result: &Result<InstallOutcome, InstallError>,
) -> Vec<ConsoleLine> {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(InstallError::Connect(err)) => {
            return vec![ConsoleLine::Stderr(format!(
                "{LABEL}: Service Control Manager への接続に失敗しました: {err}"
            ))];
        }
        Err(InstallError::Create(err)) => {
            return vec![ConsoleLine::Stderr(format!(
                "{LABEL}: サービスの登録に失敗しました: {err}"
            ))];
        }
    };
    let mut lines = Vec::new();
    match outcome.kind {
        // アップグレード時に既存の起動種別を上書きしないため（T17-4、design
        // §5）、既に登録済みなら作り直していない。
        InstallKind::AlreadyRegistered => {
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: Windows サービス '{SERVICE_NAME}' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   起動種別を変える場合は管理 UI、または `sc.exe config {SERVICE_NAME} start=demand|auto` を使ってください"
            )));
            lines.push(preshutdown_console_line(LABEL, &outcome.preshutdown));
        }
        InstallKind::Created => {
            if let Some(err) = &outcome.description_error {
                lines.push(ConsoleLine::Stderr(format!(
                    "{LABEL}: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}"
                )));
            }
            lines.push(preshutdown_console_line(LABEL, &outcome.preshutdown));
            // OnDemand には遅延自動開始の概念が無い（Windows API 仕様 -
            // `set_delayed_auto_start`は`AutoStart`にのみ意味を持つ）ため、
            // T17-4 以降は呼び出さない。
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: Windows サービス '{SERVICE_NAME}' を登録しました"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   表示名: {SERVICE_DISPLAY_NAME}"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   実行ファイル: {}",
                exe_path.display()
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   起動種別: 手動（Demand） - OS 再起動だけでは開始しません"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: `Start-Service {SERVICE_NAME}` または管理 UI から明示的に開始してください"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: （OS 起動時に自動開始させたい場合は管理 UI の自動起動設定を有効にしてください）"
            )));
        }
    }
    lines
}

/// `uninstall` の結果を利用者に出す行にする（移す前の文言そのまま）。
#[cfg_attr(not(windows), allow(dead_code))]
fn uninstall_lines(result: &Result<UninstallOutcome, UninstallError>) -> Vec<ConsoleLine> {
    let stop_line = |stop: &StopAttempt| match stop {
        StopAttempt::NotRunning => None,
        StopAttempt::StopRequested => Some(ConsoleLine::Stdout(format!(
            "{LABEL}: サービスを停止しました"
        ))),
        StopAttempt::StopFailed(err) => Some(ConsoleLine::Stderr(format!(
            "{LABEL}: サービスの停止に失敗しました: {err}"
        ))),
        StopAttempt::QueryFailed(err) => Some(ConsoleLine::Stderr(format!(
            "{LABEL}: サービス状態の取得に失敗しました: {err}"
        ))),
    };
    match result {
        Ok(outcome) => {
            let mut lines: Vec<ConsoleLine> = stop_line(&outcome.stop).into_iter().collect();
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: Windows サービス '{SERVICE_NAME}' の登録を解除しました"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: （このプロセスのハンドルが閉じるまで完全な削除が遅延することがあります - Windows API の仕様）"
            )));
            lines
        }
        Err(UninstallError::Connect(err)) => vec![ConsoleLine::Stderr(format!(
            "{LABEL}: Service Control Manager への接続に失敗しました: {err}"
        ))],
        Err(UninstallError::Open(err)) => vec![ConsoleLine::Stderr(format!(
            "{LABEL}: サービス '{SERVICE_NAME}' が見つかりません（既に未登録の可能性）: {err}"
        ))],
        Err(UninstallError::Delete { stop, error }) => {
            let mut lines: Vec<ConsoleLine> = stop_line(stop).into_iter().collect();
            lines.push(ConsoleLine::Stderr(format!(
                "{LABEL}: サービスの削除に失敗しました: {error}"
            )));
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    //! install / uninstall の文言の golden（#392 B3-1）。期待値は共有 crate へ
    //! 移す前の`install`/`uninstall`/`fail`が`println!`/`eprintln!`していた
    //! 文字列をそのまま書き写したもの。順序（標準出力と標準エラー出力の
    //! 並び）も移す前と同じ。

    use super::*;
    use banto_service_control::PreshutdownReport;
    use std::path::PathBuf;

    fn out(text: &str) -> ConsoleLine {
        ConsoleLine::Stdout(text.to_string())
    }

    fn err(text: &str) -> ConsoleLine {
        ConsoleLine::Stderr(text.to_string())
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\BantoHub\banto-hub.exe")
    }

    #[test]
    fn install_golden_already_registered() {
        let result = Ok(InstallOutcome {
            kind: InstallKind::AlreadyRegistered,
            description_error: None,
            preshutdown: PreshutdownReport::Warning("PRESHUTDOWN の警告".to_string()),
        });
        assert_eq!(
            install_lines(&exe(), &result),
            vec![
                out("banto-hub: Windows サービス 'BantoHub' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"),
                out("banto-hub:   起動種別を変える場合は管理 UI、または `sc.exe config BantoHub start=demand|auto` を使ってください"),
                err("banto-hub: warn: PRESHUTDOWN の警告"),
            ]
        );
    }

    #[test]
    fn install_golden_created() {
        let result = Ok(InstallOutcome {
            kind: InstallKind::Created,
            description_error: Some("os error 5".to_string()),
            preshutdown: PreshutdownReport::Applied("PRESHUTDOWN の行".to_string()),
        });
        assert_eq!(
            install_lines(&exe(), &result),
            vec![
                err("banto-hub: サービスの説明文の設定に失敗しました（登録自体は完了）: os error 5"),
                out("banto-hub: PRESHUTDOWN の行"),
                out("banto-hub: Windows サービス 'BantoHub' を登録しました"),
                out("banto-hub:   表示名: banto-hub タグサーバー"),
                out(r"banto-hub:   実行ファイル: C:\Program Files\BantoHub\banto-hub.exe"),
                out("banto-hub:   起動種別: 手動（Demand） - OS 再起動だけでは開始しません"),
                out("banto-hub: `Start-Service BantoHub` または管理 UI から明示的に開始してください"),
                out("banto-hub: （OS 起動時に自動開始させたい場合は管理 UI の自動起動設定を有効にしてください）"),
            ]
        );
        let without_warning = Ok(InstallOutcome {
            kind: InstallKind::Created,
            description_error: None,
            preshutdown: PreshutdownReport::Applied("PRESHUTDOWN の行".to_string()),
        });
        assert_eq!(install_lines(&exe(), &without_warning).len(), 7);
    }

    /// 失敗は 1 行だけ（Hub の`fail`は管理者権限の案内を足さない）。
    #[test]
    fn install_golden_failures() {
        assert_eq!(
            install_lines(
                &exe(),
                &Err(InstallError::Connect("os error 5".to_string()))
            ),
            vec![err(
                "banto-hub: Service Control Manager への接続に失敗しました: os error 5"
            )]
        );
        assert_eq!(
            install_lines(
                &exe(),
                &Err(InstallError::Create("os error 1073".to_string()))
            ),
            vec![err(
                "banto-hub: サービスの登録に失敗しました: os error 1073"
            )]
        );
    }

    #[test]
    fn uninstall_golden() {
        let removed = [
            out("banto-hub: Windows サービス 'BantoHub' の登録を解除しました"),
            out("banto-hub: （このプロセスのハンドルが閉じるまで完全な削除が遅延することがあります - Windows API の仕様）"),
        ];
        let cases = [
            (StopAttempt::NotRunning, None),
            (
                StopAttempt::StopRequested,
                Some(out("banto-hub: サービスを停止しました")),
            ),
            (
                StopAttempt::StopFailed("os error 1061".to_string()),
                Some(err(
                    "banto-hub: サービスの停止に失敗しました: os error 1061",
                )),
            ),
            (
                StopAttempt::QueryFailed("os error 6".to_string()),
                Some(err(
                    "banto-hub: サービス状態の取得に失敗しました: os error 6",
                )),
            ),
        ];
        for (stop, line) in cases {
            let mut expected: Vec<ConsoleLine> = line.into_iter().collect();
            expected.extend(removed.iter().cloned());
            assert_eq!(uninstall_lines(&Ok(UninstallOutcome { stop })), expected);
        }
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Connect("os error 5".to_string()))),
            vec![err(
                "banto-hub: Service Control Manager への接続に失敗しました: os error 5"
            )]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Open("os error 1060".to_string()))),
            vec![err(
                "banto-hub: サービス 'BantoHub' が見つかりません（既に未登録の可能性）: os error 1060"
            )]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Delete {
                stop: StopAttempt::StopRequested,
                error: "os error 1072".to_string(),
            })),
            vec![
                out("banto-hub: サービスを停止しました"),
                err("banto-hub: サービスの削除に失敗しました: os error 1072"),
            ]
        );
    }
}
