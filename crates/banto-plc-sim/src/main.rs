//! `banto-plc-sim` の入口。引数を読んで [`banto_plc_sim`] を呼ぶだけ
//! （設計: docs/crates/banto-plc-sim-design.md）。
//!
//! ```text
//! banto-plc-sim <設定ファイル> [--listen <addr:port>] [--at <UNIX 秒>] [--freeze]
//! ```
//!
//! 終了コード: 0＝Ctrl+C で止めた、1＝設定・待ち受けの失敗、2＝引数の誤り。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use banto_plc_sim::{load_plan, Clock, Runtime};

const USAGE: &str =
    "使い方: banto-plc-sim <設定ファイル> [--listen <addr:port>] [--at <UNIX 秒>] [--freeze]";

struct Args {
    config: PathBuf,
    listen: Option<SocketAddr>,
    at: Option<f64>,
    freeze: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut config = None;
    let mut listen = None;
    let mut at = None;
    let mut freeze = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                let value = args.next().ok_or("--listen に値が要る")?;
                listen = Some(
                    value
                        .parse::<SocketAddr>()
                        .map_err(|e| format!("--listen {value}: {e}"))?,
                );
            }
            "--at" => {
                let value = args.next().ok_or("--at に値が要る")?;
                let t: f64 = value
                    .parse()
                    .map_err(|_| format!("--at {value}: 数が要る"))?;
                if !t.is_finite() {
                    return Err(format!("--at {value}: 有限の数が要る"));
                }
                at = Some(t);
            }
            "--freeze" => freeze = true,
            other if other.starts_with("--") => return Err(format!("知らない引数 {other}")),
            other => {
                if config.replace(PathBuf::from(other)).is_some() {
                    return Err("設定ファイルは 1 つだけ".to_string());
                }
            }
        }
    }
    Ok(Args {
        config: config.ok_or("設定ファイルが要る")?,
        listen,
        at,
        freeze,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let plan = match load_plan(&args.config) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("設定の誤り: {e}");
            return ExitCode::from(1);
        }
    };
    let clock = match Clock::from_args(&plan, args.at, args.freeze) {
        Ok(clock) => clock,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let runtime = match Runtime::start(plan, args.listen, clock).await {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("待ち受けに失敗: {e}");
            return ExitCode::from(1);
        }
    };
    println!("待ち受け: {} ({})", runtime.addr(), runtime.protocol());
    println!("信号: {} 本", runtime.signal_count());
    if let Clock::Fixed(t) = clock {
        println!("止めた時刻: {t} (UNIX 秒)");
    }
    runtime.serve_until_ctrl_c(clock).await;
    ExitCode::SUCCESS
}
