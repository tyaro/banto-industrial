//! T17-2 スライス2（docs/banto-hub-t17-design.md §3「T17-2」）: UAC 昇格
//! ヘルパー `banto-hub-elev.exe`。
//!
//! `apps/banto-hub/core/banto-hub-elev.manifest`（`build.rs`が
//! `embed_resource::compile_for`でこのバイナリにだけリンクする。
//! `Cargo.toml`の`[[bin]]`セクション参照）で
//! `requestedExecutionLevel level="requireAdministrator"`を埋め込んでいる
//! ため、実行するだけで OS が UAC 昇格プロンプトを出す（明示的な自己昇格
//! コードは書いていない。Windows ローダーが exe の埋め込みマニフェストを
//! 見て自動的に処理する）。
//!
//! ## CLI
//!
//! ```text
//! banto-hub-elev.exe <action> [args...]
//! ```
//!
//! `<action>`は[`banto_hub_core::service_elevated::ElevatedAction`]が定義
//! する固定の6種類のみ（`setup-operators`/`grant-service-acl`/
//! `service-install`/`service-uninstall`/`autostart-enable`/
//! `autostart-disable`）- フリーフォームなコマンド文字列は一切受け付けない
//! （昇格プロセスへ任意コマンドを渡させないためのセキュリティ境界）。
//! `setup-operators`だけ、追加ユーザー名を1個までの引数として受け付ける
//! （省略時は現在の対話ユーザー - `service_elevated`のモジュール doc参照）。
//!
//! 実装本体は[`banto_hub_core::service_elevated`]（lib crate 側に置き、
//! `banto-hub.exe`とテストコードの両方から検証しやすくしている）- この
//! ファイルは引数パース＋dispatch＋終了コードだけを持つ薄い CLI シェル。
//!
//! ## 非 Windows ビルド
//!
//! banto-hub は Windows 専用製品だが、このワークスペース自体は非 Windows
//! でも`cargo check --workspace --all-targets`が通る必要がある
//! （`bin/banto_hub/win_service.rs`と同じ事情）。非 Windows では引数を見ずに
//! 案内だけ出して`exit(0)`する（そもそも呼ばれる想定がない - 誤って
//! 実行された場合もエラー終了にはしない）。

#[cfg(windows)]
fn main() {
    use banto_hub_core::service_elevated::ElevatedAction;

    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some(action_arg) = args.first() else {
        print_usage();
        std::process::exit(1);
    };

    let Some(action) = ElevatedAction::parse(action_arg) else {
        eprintln!(
            "banto-hub-elev: 不明な action '{action_arg}' です（固定の6種類のみ受け付けます）"
        );
        print_usage();
        std::process::exit(1);
    };

    let extra_args = &args[1..];
    if let Err(err) = banto_hub_core::service_elevated::run(action, extra_args) {
        eprintln!("banto-hub-elev: {err}");
        std::process::exit(1);
    }

    println!("banto-hub-elev: '{action}' が完了しました");
}

#[cfg(windows)]
fn print_usage() {
    use banto_hub_core::service_elevated::ElevatedAction;

    eprintln!("banto-hub-elev: 使用法: banto-hub-elev.exe <action> [args...]");
    eprintln!(
        "banto-hub-elev:   action = {}",
        ElevatedAction::ALL_NAMES.join(" | ")
    );
    eprintln!(
        "banto-hub-elev:   例: banto-hub-elev.exe setup-operators [ユーザー名（省略時は現在の対話ユーザー）]"
    );
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "banto-hub-elev: このツールは Windows 専用です（UAC 昇格・ローカルグループ・SCM 操作は Windows API のみに依存します）"
    );
    std::process::exit(0);
}
