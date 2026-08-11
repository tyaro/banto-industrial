//! T5-5 実機検証ツール（docs/t5-handoff.md §3 T5-5、docs/tag-server-design.md
//! §9 T5 行、docs/plan.md W5 の残項目「実機検証」に対応）。
//!
//! 実機 PLC（三菱 R08ENCPU、iQ-R シリーズ、SLMP対応、`192.168.11.200:5200`）
//! に対し、以下4点を順に検証する：
//!
//! 1. フェーズ1: 基本接続・読み取り（`banto_plc::slmp::SlmpClient` の SLMP互換性）
//! 2. フェーズ2: 通常書き込み + T8 ビット単位書き込み（RMW）
//! 3. フェーズ3: MELSEC 文字列デバイス（Shift-JIS）の往復
//! 4. フェーズ4: 同時 SLMP セッション数の上限（実機が複数 TCP セッションを
//!    許容するか）
//!
//! ## 安全策
//!
//! - 書き込み対象アドレスは `D3000`〜`D4999` / `M1000`〜`M2000` の範囲に
//!   限定（オーナー指定）。[`addr`] が全アドレス構築の唯一の入口で、
//!   [`assert_safe_address`] がこの範囲外を弾く（このファイル内のハード
//!   コード定数しか通らないので通常は発火しないが、防御的に必ず通す）。
//! - フェーズ2・3（書き込みを伴う）は環境変数 `REAL_PLC_CONFIRM_WRITE=1` が
//!   設定されている時のみ実行する。フェーズ1・4は読み取りのみなので常時
//!   実行可。
//! - 各フェーズは前段が失敗したら即座に停止し、後続フェーズを実行しない。
//! - 接続・応答タイムアウトは数秒に設定し、無応答でハングしない。
//!
//! ## 実行方法
//!
//! ```text
//! cargo run -p banto-broker --example real_plc_verify              # フェーズ1・4のみ（読み取り専用）
//! REAL_PLC_CONFIRM_WRITE=1 cargo run -p banto-broker --example real_plc_verify  # 全フェーズ
//! ```

use std::time::Duration;

use banto_broker::{BackoffConfig, BrokerError, BrokerSupervisor};
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, DataType, PlcClient, PlcValue, ReadRequest,
    ReadResult, SlmpClient, SlmpConfig, SlmpCpu, StringReadRequest, TagValue,
};
use banto_plc_write::{
    BatchWriteRequest, PlcWriteClient, SlmpWriteClient, StringWriteRequest, WriteRequest,
    WriteResult,
};
use banto_tags::PlcConnection;

const HOST: &str = "192.168.11.200";
const PORT: u16 = 5200;

/// D3000-D4999 / M1000-M2000 のみ（オーナー指定の安全範囲）。
fn assert_safe_address(raw: &str) {
    let base = raw.split('.').next().unwrap_or(raw);
    if base.len() < 2 {
        panic!("安全のため、アドレス {raw} を判定できません（デバイス記法が不正）");
    }
    let (device, num_str) = base.split_at(1);
    let num: u32 = num_str
        .parse()
        .unwrap_or_else(|_| panic!("安全のため、アドレス {raw} のデバイス番号を解析できません"));
    let in_range = match device {
        "D" => (3000..=4999).contains(&num),
        "M" => (1000..=2000).contains(&num),
        _ => false,
    };
    if !in_range {
        panic!(
            "安全のため、アドレス {raw} は許可範囲外です（D3000-D4999 / M1000-M2000 のみ許可、オーナー指定）"
        );
    }
}

/// このファイル内の全アドレス構築の唯一の入口。ハードコードされた文字列
/// リテラルしか通らない想定だが、[`assert_safe_address`] を必ず通す。
fn addr(raw: &str) -> Address {
    assert_safe_address(raw);
    Address::parse_slmp(raw).unwrap_or_else(|e| panic!("hardcoded address {raw} should parse: {e}"))
}

fn base_config() -> SlmpConfig {
    SlmpConfig {
        host: HOST.to_string(),
        port: PORT,
        cpu: SlmpCpu::R, // R08ENCPU (iQ-R シリーズ)
        connect_timeout: Duration::from_secs(3),
        response_timeout: Duration::from_secs(3),
        ..Default::default()
    }
}

/// D ワード読み取り結果を10進・16進・2進で表示するための整形。
fn fmt_word_result(r: &ReadResult) -> String {
    match r {
        ReadResult::Value(TagValue::F64(v)) => {
            let w = *v as i64 as u16;
            format!("{v} (0x{w:04X}, 0b{w:016b})")
        }
        other => format!("{other:?}"),
    }
}

/// 1回だけ接続して書き込み、即切断する（フェーズ2・3内で「常に1セッション
/// のみ」を保つためのヘルパー — フェーズ4より前に複数セッションの問題を
/// 混入させないよう、意図的にフェーズ2・3は逐次1セッションに限定する）。
async fn write_once(requests: &[BatchWriteRequest]) -> Result<Vec<WriteResult>, String> {
    let mut client = SlmpWriteClient::new(base_config());
    client
        .connect()
        .await
        .map_err(|e| format!("書込クライアント接続失敗: {e}"))?;
    let result = client
        .write_batch_mixed(requests)
        .await
        .map_err(|e| format!("書込失敗: {e}"));
    client.disconnect().await;
    result
}

/// [`write_once`] の読み取り版（数値/ビットのみ）。
async fn read_once(requests: &[ReadRequest]) -> Result<Vec<ReadResult>, String> {
    let mut client = SlmpClient::new(base_config());
    client
        .connect()
        .await
        .map_err(|e| format!("読取クライアント接続失敗: {e}"))?;
    let result = client
        .read_batch(requests)
        .await
        .map_err(|e| format!("読取失敗: {e}"));
    client.disconnect().await;
    result
}

/// [`write_once`] の読み取り版（文字列混在バッチ）。
async fn read_mixed_once(requests: &[BatchReadRequest]) -> Result<Vec<BatchReadResult>, String> {
    let mut client = SlmpClient::new(base_config());
    client
        .connect()
        .await
        .map_err(|e| format!("読取クライアント接続失敗: {e}"))?;
    let result = client
        .read_batch_mixed(requests)
        .await
        .map_err(|e| format!("読取失敗: {e}"));
    client.disconnect().await;
    result
}

/// フェーズ1: 基本接続・読み取り（常に実行）。ここが失敗したら以降は実行
/// しない。
async fn phase1_read_only() -> Result<(), String> {
    println!("--- フェーズ1: 基本接続・読み取り（SLMP互換性） ---");
    let mut client = SlmpClient::new(base_config());
    println!("接続中: {HOST}:{PORT} (SLMP, R08ENCPU) ...");
    client
        .connect()
        .await
        .map_err(|e| format!("接続失敗: {e}"))?;
    println!("接続成功。");

    let mut requests: Vec<ReadRequest> = Vec::new();
    for n in 3000..3010u32 {
        requests.push(ReadRequest {
            address: addr(&format!("D{n}")),
            data_type: DataType::U16,
        });
    }
    for n in 1000..1016u32 {
        requests.push(ReadRequest {
            address: addr(&format!("M{n}")),
            data_type: DataType::Bit,
        });
    }

    let results = client
        .read_batch(&requests)
        .await
        .map_err(|e| format!("read_batch 失敗: {e}"))?;

    println!("D3000-D3009 (10ワード):");
    for (i, r) in results.iter().take(10).enumerate() {
        println!("  D{} = {}", 3000 + i, fmt_word_result(r));
    }
    println!("M1000-M1015 (16ビット):");
    for (i, r) in results.iter().skip(10).enumerate() {
        println!("  M{} = {:?}", 1000 + i, r);
    }

    client.disconnect().await;
    println!("切断しました。");
    Ok(())
}

/// フェーズ2: 書き込み + T8 ビット単位書き込み（RMW）。
/// `REAL_PLC_CONFIRM_WRITE=1` の時のみ呼ばれる。
async fn phase2_write_and_bit_rmw() -> Result<(), String> {
    println!("--- フェーズ2: 書き込み + Bit RMW ---");

    // 2a: D3000-D3004 にパターン値を書き込み、読み戻して一致を確認。
    let pattern: [u16; 5] = [0x1111, 0x2222, 0x3333, 0x4444, 0x5555];
    let writes: Vec<BatchWriteRequest> = pattern
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            BatchWriteRequest::Numeric(WriteRequest {
                address: addr(&format!("D{}", 3000 + i)),
                data_type: DataType::U16,
                value: TagValue::F64(v as f64),
            })
        })
        .collect();
    let write_results = write_once(&writes).await?;
    println!("[2a] D3000-D3004 書込結果: {write_results:?}");

    let reads: Vec<ReadRequest> = (0..5)
        .map(|i| ReadRequest {
            address: addr(&format!("D{}", 3000 + i)),
            data_type: DataType::U16,
        })
        .collect();
    let read_back = read_once(&reads).await?;
    let mut all_matched = true;
    for (i, (expected, got)) in pattern.iter().zip(read_back.iter()).enumerate() {
        let matched = matches!(got, ReadResult::Value(TagValue::F64(v)) if *v == *expected as f64);
        all_matched &= matched;
        println!(
            "  [2a] D{}: 期待値=0x{:04X} 実測={} 一致={}",
            3000 + i,
            expected,
            fmt_word_result(got),
            matched
        );
    }
    println!("[2a] 全一致: {all_matched}");
    if !all_matched {
        return Err("D3000-D3004 のパターン書込/読戻が一致しませんでした".to_string());
    }

    // 2b: D3010 でビット単位RMW（.0/.1/.2 を個別に立て、都度ワード全体を
    // 読み戻して他ビットが不変であることを確認）。
    let baseline = read_once(&[ReadRequest {
        address: addr("D3010"),
        data_type: DataType::U16,
    }])
    .await?;
    println!("[2b] D3010 初期値: {}", fmt_word_result(&baseline[0]));

    for bit in 0..3u8 {
        let bit_addr = addr(&format!("D3010.{bit}"));
        let w = write_once(&[BatchWriteRequest::BitInWord {
            address: bit_addr,
            value: true,
        }])
        .await?;
        let word = read_once(&[ReadRequest {
            address: addr("D3010"),
            data_type: DataType::U16,
        }])
        .await?;
        println!(
            "  [2b] D3010.{bit}=true 書込結果={:?} 書込後ワード={}",
            w,
            fmt_word_result(&word[0])
        );
        if !matches!(w.first(), Some(WriteResult::Ok)) {
            return Err(format!(
                "D3010.{bit} への RMW 書込が Ok を返しませんでした: {w:?}"
            ));
        }
    }

    // クリア（後片付けの一部）。
    for bit in 0..3u8 {
        let bit_addr = addr(&format!("D3010.{bit}"));
        let w = write_once(&[BatchWriteRequest::BitInWord {
            address: bit_addr,
            value: false,
        }])
        .await?;
        println!("  [2b] D3010.{bit}=false クリア結果={w:?}");
    }
    let final_word = read_once(&[ReadRequest {
        address: addr("D3010"),
        data_type: DataType::U16,
    }])
    .await?;
    println!("[2b] D3010 クリア後: {}", fmt_word_result(&final_word[0]));

    // 2c: M1000 は素のビットデバイス（D側のRMWと対比するため通常の
    // Numeric/Bit書込で試す — BitInWord は使わない）。
    for value in [true, false] {
        let w = write_once(&[BatchWriteRequest::Numeric(WriteRequest {
            address: addr("M1000"),
            data_type: DataType::Bit,
            value: TagValue::Bit(value),
        })])
        .await?;
        let r = read_once(&[ReadRequest {
            address: addr("M1000"),
            data_type: DataType::Bit,
        }])
        .await?;
        println!("[2c] M1000={value} 書込結果={w:?} 読戻={r:?}");
        if !matches!(r.first(), Some(ReadResult::Value(TagValue::Bit(b))) if *b == value) {
            return Err(format!(
                "M1000={value} の書込/読戻が一致しませんでした: {r:?}"
            ));
        }
    }

    // 後片付け: D3000-D3004 を 0 にクリア（D3010・M1000 は上ですでに 0/false
    // に戻している）。
    let cleanup: Vec<BatchWriteRequest> = (0..5)
        .map(|i| {
            BatchWriteRequest::Numeric(WriteRequest {
                address: addr(&format!("D{}", 3000 + i)),
                data_type: DataType::U16,
                value: TagValue::F64(0.0),
            })
        })
        .collect();
    let cleanup_result = write_once(&cleanup).await?;
    println!("[cleanup] D3000-D3004 を 0 にクリア: {cleanup_result:?}");

    Ok(())
}

/// フェーズ3: MELSEC 文字列デバイス（Shift-JIS）の往復検証。
/// `REAL_PLC_CONFIRM_WRITE=1` の時のみ呼ばれる。
async fn phase3_string_roundtrip() -> Result<(), String> {
    println!("--- フェーズ3: 文字列/Shift-JIS 往復検証 ---");
    let test_string = "テスト123";
    let words = 6u16;

    let write_result = write_once(&[BatchWriteRequest::String(StringWriteRequest {
        address: addr("D3020"),
        words,
        value: test_string.to_string(),
    })])
    .await?;
    println!("[3] 文字列書込結果: {write_result:?}");

    let read_result = read_mixed_once(&[BatchReadRequest::String(StringReadRequest {
        address: addr("D3020"),
        words,
    })])
    .await?;
    println!("[3] 文字列読戻結果: {read_result:?}");

    let matched =
        matches!(&read_result[..], [BatchReadResult::Value(PlcValue::Str(s))] if s == test_string);
    println!("[3] 一致: {matched} (期待=\"{test_string}\")");

    // 後片付け: 空文字列を書くと full span が 0x00 パディングされる
    // （StringWriteRequest のドキュメント参照）ので、これでクリアになる。
    let clear_result = write_once(&[BatchWriteRequest::String(StringWriteRequest {
        address: addr("D3020"),
        words,
        value: String::new(),
    })])
    .await?;
    println!("[cleanup] D3020 文字列領域クリア: {clear_result:?}");

    if !matched {
        return Err(format!(
            "文字列往復が一致しませんでした（decode_string_value のバイト順前提を要再検証）: 実測={read_result:?}"
        ));
    }
    Ok(())
}

/// フェーズ4: 同時 SLMP セッション数上限（常に実行、読み取りのみ）。
/// broker 経由で1本、さらに独立した bare SlmpClient を最大2本、同時に
/// 同じ PLC へ接続を試みる。broker 自体の多重化保証ではなく、実機 CPU
/// 側が複数 TCP セッションをそもそも許容するかどうかを見るのが目的。
async fn phase4_session_limit() -> Result<(), String> {
    println!("--- フェーズ4: 同時セッション数上限 ---");

    let connection = PlcConnection {
        id: 1,
        name: "t5-5-verify".to_string(),
        protocol: "slmp".to_string(),
        host: HOST.to_string(),
        port: PORT as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,

        word_order: "low_high".to_string(),
    };

    let supervisor = BrokerSupervisor::spawn(&[connection], BackoffConfig::default())
        .map_err(|e| format!("broker spawn 失敗: {e}"))?;
    let handle = supervisor
        .handle(1)
        .ok_or_else(|| "broker handle が見つかりません".to_string())?;

    println!("[4] broker セッション（セッション#1）接続待機中...");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let probe = handle
            .read(vec![BatchReadRequest::Numeric(ReadRequest {
                address: addr("D3000"),
                data_type: DataType::U16,
            })])
            .await;
        match probe {
            Ok(_) => {
                println!("[4] セッション#1（broker）接続成功。");
                break;
            }
            Err(BrokerError::Disconnected { .. }) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => {
                supervisor.shutdown().await;
                return Err(format!("broker セッション確立に失敗: {e}"));
            }
        }
    }

    // セッション#1（broker）を保持したまま、独立したセッション#2を試す。
    let mut second = SlmpClient::new(base_config());
    println!("[4] セッション#2（独立した SlmpClient）接続試行中...");
    let second_ok = match second.connect().await {
        Ok(()) => {
            println!("[4] セッション#2 接続成功。");
            let r = second
                .read_batch(&[ReadRequest {
                    address: addr("D3000"),
                    data_type: DataType::U16,
                }])
                .await;
            println!("[4] セッション#2 read 結果: {r:?}");
            true
        }
        Err(e) => {
            println!("[4] セッション#2 接続失敗: {e}");
            false
        }
    };

    // セッション#1・#2を保持したまま、さらにセッション#3を試す
    // （#2が失敗した場合は#3は試さない — 上限を超えた地点は既に判明している）。
    let third_ok = if second_ok {
        let mut third = SlmpClient::new(base_config());
        println!("[4] セッション#3（独立した SlmpClient）接続試行中...");
        let ok = match third.connect().await {
            Ok(()) => {
                println!("[4] セッション#3 接続成功。");
                let r = third
                    .read_batch(&[ReadRequest {
                        address: addr("D3000"),
                        data_type: DataType::U16,
                    }])
                    .await;
                println!("[4] セッション#3 read 結果: {r:?}");
                third.disconnect().await;
                true
            }
            Err(e) => {
                println!("[4] セッション#3 接続失敗: {e}");
                false
            }
        };
        ok
    } else {
        println!("[4] セッション#2が失敗したため、セッション#3は試行しません。");
        false
    };

    second.disconnect().await;

    let concurrent_sessions = 1 + u32::from(second_ok) + u32::from(third_ok);
    println!(
        "[4] 実測: セッション#1(broker)=成功, #2={}, #3={} -> 少なくとも{}本の同時セッションを確認",
        second_ok, third_ok, concurrent_sessions
    );

    supervisor.shutdown().await;
    Ok(())
}

#[tokio::main]
async fn main() {
    println!("=== T5-5 実機検証ツール ===");
    println!("対象: 三菱 R08ENCPU (iQ-R, SLMP) @ {HOST}:{PORT}");
    println!("安全に読み書きしてよいアドレス範囲: D3000-D4999 / M1000-M2000（オーナー指定）");
    println!();

    if let Err(e) = phase1_read_only().await {
        eprintln!("[PHASE1] 失敗: {e}");
        eprintln!("フェーズ1（基本接続・読取）が失敗したため、検証を中止します。");
        std::process::exit(1);
    }
    println!("[PHASE1] 成功\n");

    let confirm_write = std::env::var("REAL_PLC_CONFIRM_WRITE")
        .map(|v| v == "1")
        .unwrap_or(false);

    if !confirm_write {
        println!(
            "REAL_PLC_CONFIRM_WRITE=1 が未設定のため、フェーズ2・3（書込を伴う）はスキップします。"
        );
        println!("フェーズ4（同時セッション数上限、読取のみ）は実行します。\n");
        if let Err(e) = phase4_session_limit().await {
            eprintln!("[PHASE4] 失敗: {e}");
            std::process::exit(1);
        }
        println!("[PHASE4] 完了\n");
        println!("=== 検証終了（書込フェーズはスキップしました） ===");
        return;
    }

    if let Err(e) = phase2_write_and_bit_rmw().await {
        eprintln!("[PHASE2] 失敗: {e}");
        eprintln!("フェーズ2が失敗したため、以降のフェーズを中止します。");
        std::process::exit(1);
    }
    println!("[PHASE2] 成功\n");

    if let Err(e) = phase3_string_roundtrip().await {
        eprintln!("[PHASE3] 失敗: {e}");
        eprintln!("フェーズ3が失敗したため、フェーズ4を中止します。");
        std::process::exit(1);
    }
    println!("[PHASE3] 成功\n");

    if let Err(e) = phase4_session_limit().await {
        eprintln!("[PHASE4] 失敗: {e}");
        std::process::exit(1);
    }
    println!("[PHASE4] 完了\n");

    println!("=== 全フェーズ完了 ===");
}
