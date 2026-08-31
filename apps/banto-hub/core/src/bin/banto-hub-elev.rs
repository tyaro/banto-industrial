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
//! する固定の9種類のみ（`setup-operators`/`grant-service-acl`/
//! `grant-profile-acl`/`service-install`/`service-uninstall`/
//! `autostart-enable`/`autostart-disable`/`reset-password`/
//! `revert-to-commissioning`）- フリーフォームなコマンド文字列は
//! 一切受け付けない（昇格プロセスへ任意コマンドを渡させないための
//! セキュリティ境界）。`setup-operators`は追加ユーザー名を1個まで、
//! `grant-profile-acl`は`[username] [profile-id]`を2個まで、
//! `reset-password`は`<username> [profile-id]`を1〜2個（`username`は省略
//! 不可）、`revert-to-commissioning`は`[profile-id]`を1個まで、それぞれ
//! 引数として受け付ける（省略時は現在の対話ユーザー／既定 profile -
//! `service_elevated`のモジュール doc参照）。**`reset-password`の新パスワード
//! 自体は引数に含めない** - プロセス一覧・シェル履歴に残さないため、実行後に
//! 標準入力から対話的に読む（`service_elevated`モジュール doc「ロックダウン
//! 回復アクション」節参照）。
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
            "banto-hub-elev: 不明な action '{action_arg}' です（固定の9種類のみ受け付けます）"
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
    eprintln!(
        "banto-hub-elev:   例: banto-hub-elev.exe grant-profile-acl [ユーザー名] [profile-id]（両方省略時は現在の対話ユーザー・既定 profile）"
    );
    eprintln!(
        "banto-hub-elev:   例: banto-hub-elev.exe reset-password <ユーザー名> [profile-id]（新パスワードは実行後に標準入力から入力、profile-id省略時は既定 profile）"
    );
    eprintln!(
        "banto-hub-elev:   例: banto-hub-elev.exe revert-to-commissioning [profile-id]（ロックダウン済み→試運転モードへ復帰、profile-id省略時は既定 profile）"
    );
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "banto-hub-elev: このツールは Windows 専用です（UAC 昇格・ローカルグループ・SCM 操作は Windows API のみに依存します）"
    );
    std::process::exit(0);
}
