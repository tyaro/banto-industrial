//! `chronogazer-server create-admin --username <名前> [--display-name <表示名>]`（banto v8。
//! 判断と DB の書き込みは `chronogazer_core::first_admin`）。
//!
//! サービス・コンソールの `chronogazer-server`、デスクトップのアプリを止めてから実行する
//! （起動と同じ A1 の排他で確かめ、取れなければ何も書かずに止める）。データのルートは起動と
//! 同じ解き方（`start::resolve_layout`。環境変数 `CHRONOGAZER_ROOT`、既定
//! `%ProgramData%\ChronoGazer`）。パスワードは引数に入れない: 端末なら入力を表示せずに
//! 2 回尋ね、パイプ・ファイルなら標準入力の 1 行目（Hub の `banto-hub create-admin` と
//! 同じ `password_input`）。
//!
//! 終了コード: 0 作った、1 作れなかった（理由を出す。何も書いていない）、64 引数が違う。

use chronogazer_core::first_admin::{run_create_admin, NewAdmin};
use chronogazer_core::server_host::SERVER_LABEL;

use crate::password_input as pw;

/// サブコマンドの名前（最初の引数）。
pub(crate) const CREATE_ADMIN_ARG: &str = "create-admin";

/// `create-admin` の引数。パスワードは入れない（プロセスの一覧と履歴に残る）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CreateAdminArgs {
    pub username: String,
    pub display_name: Option<String>,
}

pub(crate) fn parse_create_admin_args(args: &[String]) -> Result<CreateAdminArgs, String> {
    let mut username = None;
    let mut display_name = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let slot = match arg.as_str() {
            "--username" => &mut username,
            "--display-name" => &mut display_name,
            other => return Err(format!("create-admin の不明な引数です: '{other}'")),
        };
        match rest.next() {
            Some(value) => *slot = Some(value.clone()),
            None => return Err(format!("{arg} に値がありません")),
        }
    }
    Ok(CreateAdminArgs {
        username: username.ok_or_else(|| "create-admin には --username が必要です".to_string())?,
        display_name,
    })
}

/// 新しい管理者のパスワードを読む（Hub の `read_new_password` と同じ）。
fn read_new_password() -> Result<String, String> {
    if !pw::stdin_is_terminal() {
        return pw::read_password_line(&mut std::io::stdin().lock())
            .map_err(|err| format!("パスワードを読めませんでした: {err}"));
    }
    let (first, second) = pw::read_password_twice_hidden(
        "パスワード（入力は表示されません）: ",
        "パスワード（確認のためもう一度）: ",
    )
    .map_err(|err| {
        format!(
            "パスワードを非表示で読めませんでした: {err}（Git Bash・MSYS などの疑似端末では使えません。\
             PowerShell・cmd・通常の端末で実行するか、パスワードをファイルに書いて < password.txt で渡してください）"
        )
    })?;
    if !pw::confirmation_matches(&first, &second) {
        return Err("2 回の入力が一致しません".to_string());
    }
    Ok(first)
}

/// `chronogazer-server create-admin`。待ち受けずに終わる。終了コードを返す。
pub(crate) fn run(parsed: CreateAdminArgs) -> i32 {
    let layout = match crate::start::resolve_layout() {
        Ok(layout) => layout,
        Err(code) => return code,
    };
    let password = match read_new_password() {
        Ok(password) => password,
        Err(message) => {
            eprintln!("{SERVER_LABEL}: {message}。DB には触れていません");
            return 1;
        }
    };
    let display_name = parsed
        .display_name
        .clone()
        .unwrap_or_else(|| parsed.username.clone());
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("{SERVER_LABEL}: 非同期ランタイムを作れませんでした: {err}");
            return 1;
        }
    };
    let db_path = layout.db_path();
    let created = runtime.block_on(run_create_admin(
        layout,
        NewAdmin {
            username: &parsed.username,
            password: &password,
            display_name: &display_name,
        },
    ));
    match created {
        Ok(created) => {
            println!(
                "{SERVER_LABEL}: 管理者のアカウント {} を作り、ログイン必須にしました（DB: {}）",
                created.user.username,
                db_path.display()
            );
            println!(
                "{SERVER_LABEL}: この PC からの接続（localDesktop）は、設定 auth.local_grant（既定 off）どおりになります。サービスを起動し直してください"
            );
            0
        }
        Err(reason) => {
            eprintln!(
                "{SERVER_LABEL}: 管理者のアカウントを作れませんでした: {reason}\n\
                 {SERVER_LABEL}: アカウント・認証の設定・監査ログは変更していません"
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_username_is_required_and_the_display_name_is_optional() {
        assert_eq!(
            parse_create_admin_args(&args(&["--username", "owner"])).unwrap(),
            CreateAdminArgs {
                username: "owner".into(),
                display_name: None
            }
        );
        assert_eq!(
            parse_create_admin_args(&args(&["--display-name", "オーナー", "--username", "o"]))
                .unwrap(),
            CreateAdminArgs {
                username: "o".into(),
                display_name: Some("オーナー".into())
            }
        );
        assert!(parse_create_admin_args(&[]).is_err());
        assert!(parse_create_admin_args(&args(&["--username"])).is_err());
        assert!(parse_create_admin_args(&args(&["--username", "o", "--password", "x"])).is_err());
    }
}
