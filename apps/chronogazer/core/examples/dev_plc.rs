//! 開発用 PLC（ChronoGazer R1-C の C-4、2026-09-23）。
//!
//! `banto_collect::simulation` のランプ波シミュレータ（`banto-plc` の Modbus
//! TCP / SLMP シミュレータ = 実 TCP・実バイト列）を**別プロセス**として固定
//! ポートで起動し、止められるまで値を流し続ける。ChronoGazer 側からは
//! **普通の Modbus TCP / SLMP 接続**（ホスト `127.0.0.1`・ポート = ここで
//! 指定した番号）として登録する。
//!
//! ## なぜ製品の中ではなく別プロセスなのか
//!
//! ChronoGazer の REST / Tauri は R1-B の決定で接続の `simulation` を常に
//! `false` にしている（`chronogazer_core::rest::PlcConnectionPayload` の
//! `From` 実装）。**製品にシミュレータを持ち込まない**ためで、この決定は
//! 変えない。代わりにシミュレータを「そこに居る PLC」として外に立てれば、
//! 収集は**実際のワイヤ経路**（接続・読み取り・デコード）をそのまま通る -
//! R1-C の完了条件「設定 → 収集開始 → データファイル生成 → イベント記録まで
//! 一巡」を、製品のコードパスを一切変えずに確かめられる。
//!
//! 使うのは E2E（`e2e/playwright.config.ts` の `webServer` がビルド済みの
//! `target/debug/examples/dev_plc` を起動する）と、手で画面を触る開発者
//! （`pnpm dev:plc`）。**製品コード（`src/`）はこの example を知らない。**
//!
//! ## 実行方法
//!
//! ```text
//! pnpm dev:plc                                   # Modbus TCP, 127.0.0.1:15020
//! pnpm dev:plc --protocol slmp                   # SLMP,       127.0.0.1:15000（`--` は挟まない）
//! cargo run -p chronogazer-core --example dev_plc -- --protocol modbus --port 8803
//! ```
//!
//! | 引数 / 環境変数 | 既定 | 意味 |
//! | --- | --- | --- |
//! | `--protocol modbus\|slmp`（`modbus-tcp` も可） | `modbus` | 話すプロトコル |
//! | `--port <N>` / `DEV_PLC_PORT` | Modbus `15020` / SLMP `15000` | 待ち受けポート（引数が優先） |
//!
//! 待ち受けは常に `127.0.0.1`（ループバック）だけ - 開発用の偽 PLC を LAN に
//! 晒さない。
//!
//! ## 動く番地
//!
//! `banto_collect::simulation` の「値生成」節どおり、100ms ごとに先頭
//! [`banto_collect::simulation::RAMP_ADDRESS_COUNT`] 個の番地を書き換える:
//!
//! * Modbus: 保持レジスタ `40001..` と入力レジスタ `30001..` にランプ波、
//!   コイル `00001..` と入力ステータス `10001..` にトグル。
//! * SLMP: `D0..` にランプ波、`M0..` にトグル。
//!
//! それ以外の番地は読めるが常に 0 / false。
//!
//! ## 止め方と失敗
//!
//! * Ctrl+C でランプ波タスクとシミュレータを止めて終了する（終了コード 0）。
//!   プロセスごと殺された場合（Playwright の `webServer` の後始末など）も、
//!   ソケットは OS が閉じるので残るものは無い。
//! * **ポートが使用中なら panic せず**、その旨を 1 行出して終了コード 1 で
//!   終わる（`banto_collect::simulation::start_on` が bind の失敗を
//!   `io::Error` で返す）。引数の誤りは使い方を出して終了コード 2。

use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;

use banto_collect::simulation::{self, RAMP_ADDRESS_COUNT};
use banto_collect::Protocol;

const DEFAULT_MODBUS_PORT: u16 = 15020;
const DEFAULT_SLMP_PORT: u16 = 15000;
const PORT_ENV: &str = "DEV_PLC_PORT";

const USAGE: &str = "使い方: dev_plc [--protocol modbus|slmp] [--port <N>]
  --protocol  modbus（modbus-tcp も可）または slmp。既定 modbus
  --port      待ち受けポート（1-65535）。未指定なら環境変数 DEV_PLC_PORT、
              それも無ければ Modbus 15020 / SLMP 15000";

#[derive(Debug)]
struct Args {
    protocol: Protocol,
    port: u16,
}

fn parse_protocol(value: &str) -> Result<Protocol, String> {
    match value {
        "modbus" | "modbus-tcp" => Ok(Protocol::ModbusTcp),
        "slmp" => Ok(Protocol::Slmp),
        other => Err(format!(
            "--protocol に {other:?} は指定できません（modbus / slmp）"
        )),
    }
}

fn parse_port(value: &str, source: &str) -> Result<u16, String> {
    match value.parse::<u16>() {
        Ok(0) | Err(_) => Err(format!(
            "{source} に {value:?} は指定できません（1-65535 の整数）"
        )),
        Ok(port) => Ok(port),
    }
}

/// 引数と環境変数を解釈する（純関数に近い形 - 環境変数の値は呼び出し側が渡す）。
fn parse_args(
    mut argv: impl Iterator<Item = String>,
    env_port: Option<String>,
) -> Result<Args, String> {
    let mut protocol = Protocol::ModbusTcp;
    let mut port: Option<u16> = None;
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--protocol" => {
                let value = argv
                    .next()
                    .ok_or_else(|| "--protocol の値がありません".to_string())?;
                protocol = parse_protocol(&value)?;
            }
            "--port" => {
                let value = argv
                    .next()
                    .ok_or_else(|| "--port の値がありません".to_string())?;
                port = Some(parse_port(&value, "--port")?);
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("知らない引数です: {other:?}")),
        }
    }
    let port = match (port, env_port) {
        (Some(port), _) => port,
        (None, Some(value)) => parse_port(&value, PORT_ENV)?,
        (None, None) => match protocol {
            Protocol::ModbusTcp => DEFAULT_MODBUS_PORT,
            Protocol::Slmp => DEFAULT_SLMP_PORT,
        },
    };
    Ok(Args { protocol, port })
}

/// 起動時の 1 行: どこで待っているか・どの番地が動くか。
fn banner(protocol: Protocol, addr: SocketAddr) -> String {
    let last = RAMP_ADDRESS_COUNT;
    match protocol {
        Protocol::ModbusTcp => format!(
            "dev_plc: Modbus TCP で {addr} を待ち受けています（ユニットID 任意。100ms ごとにランプ波: 保持レジスタ 40001-{} / 入力レジスタ 30001-{}、トグル: コイル 00001-{:05} / 入力ステータス 10001-{}）。Ctrl+C で終了",
            40000 + u32::from(last),
            30000 + u32::from(last),
            last,
            10000 + u32::from(last),
        ),
        Protocol::Slmp => format!(
            "dev_plc: SLMP で {addr} を待ち受けています（100ms ごとにランプ波: D0-D{}、トグル: M0-M{}）。Ctrl+C で終了",
            last - 1,
            last - 1,
        ),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1), std::env::var(PORT_ENV).ok()) {
        Ok(args) => args,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("dev_plc: {message}");
            }
            eprintln!("{USAGE}");
            return if message.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
        }
    };

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port));
    let handle = match simulation::start_on(args.protocol, addr).await {
        Ok(handle) => handle,
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!(
                "dev_plc: {addr} は既に使用中です（別の dev_plc が動いていないか確認するか、--port で別のポートを指定してください）: {err}"
            );
            return ExitCode::FAILURE;
        }
        Err(err) => {
            eprintln!("dev_plc: {addr} で待ち受けを開始できませんでした: {err}");
            return ExitCode::FAILURE;
        }
    };
    println!("{}", banner(args.protocol, handle.addr()));

    let result = tokio::signal::ctrl_c().await;
    handle.stop().await;
    match result {
        Ok(()) => {
            println!("dev_plc: 終了しました");
            ExitCode::SUCCESS
        }
        Err(err) => {
            // Ctrl+C を待てない環境（シグナルを登録できない）。シミュレータは
            // 上で止めた。
            eprintln!("dev_plc: 終了シグナルを待てませんでした: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> std::vec::IntoIter<String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn defaults_to_modbus_on_its_default_port() {
        let parsed = parse_args(args(&[]), None).unwrap();
        assert_eq!(parsed.protocol, Protocol::ModbusTcp);
        assert_eq!(parsed.port, DEFAULT_MODBUS_PORT);
    }

    #[test]
    fn slmp_has_its_own_default_port() {
        let parsed = parse_args(args(&["--protocol", "slmp"]), None).unwrap();
        assert_eq!(parsed.protocol, Protocol::Slmp);
        assert_eq!(parsed.port, DEFAULT_SLMP_PORT);
    }

    #[test]
    fn port_argument_wins_over_the_environment() {
        let parsed = parse_args(args(&["--port", "8803"]), Some("9000".into())).unwrap();
        assert_eq!(parsed.port, 8803);
        let parsed = parse_args(args(&[]), Some("9000".into())).unwrap();
        assert_eq!(parsed.port, 9000);
    }

    #[test]
    fn rejects_bad_values_instead_of_falling_back() {
        assert!(parse_args(args(&["--protocol", "opcua"]), None).is_err());
        assert!(parse_args(args(&["--port", "0"]), None).is_err());
        assert!(parse_args(args(&["--port", "70000"]), None).is_err());
        assert!(parse_args(args(&["--port"]), None).is_err());
        assert!(parse_args(args(&[]), Some("abc".into())).is_err());
        assert!(parse_args(args(&["--verbose"]), None).is_err());
    }
}
