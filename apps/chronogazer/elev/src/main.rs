//! `chronogazer-elev.exe`（#392 B3-3b）: ChronoGazer の UAC 昇格ヘルパー。
//!
//! `chronogazer-elev.manifest`（`requireAdministrator`。`build.rs` がこの bin にだけ
//! 埋め込む）により、実行するだけで OS が UAC の昇格を求める（自分で昇格するコードは
//! 持たない）。中身は lib（`chronogazer_elev`）にあり、ここは引数を受け取って
//! `chronogazer_elev::run` に渡し、行を出して終了コードで終わるだけ。
//!
//! ```text
//! chronogazer-elev.exe service-install [--install-user-sid S-1-5-21-…]
//! chronogazer-elev.exe service-uninstall | setup-operators | grant-service-acl |
//!                      grant-data-acl | autostart-enable | autostart-disable
//! ```
//!
//! 終了コード: 0 = 完了、1 = 失敗、2 = 前提が無い、64 = 使い方の誤り（Windows 以外は
//! すべて 64）。

use banto_service_control::print_console_lines;

#[cfg(windows)]
fn main() {
    use chronogazer_elev::{parse_args, run, usage_lines, win::WindowsOps, EXIT_USAGE};

    // UTF-8 にできない引数は、どのアクションにも当たらないので使い方の誤り。
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(_) => {
                print_console_lines(&usage_lines("UTF-8 にできない引数があります"));
                std::process::exit(EXIT_USAGE);
            }
        }
    }
    let action = match parse_args(&args) {
        Ok(action) => action,
        Err(reason) => {
            print_console_lines(&usage_lines(&reason));
            std::process::exit(EXIT_USAGE);
        }
    };
    let code = run(&action, &WindowsOps, &mut |line| {
        print_console_lines(std::slice::from_ref(&line))
    });
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    print_console_lines(&chronogazer_elev::not_windows_lines());
    std::process::exit(chronogazer_elev::EXIT_USAGE);
}
