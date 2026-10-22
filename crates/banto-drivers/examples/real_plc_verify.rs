//! T5-5 実機検証ツール（docs/t5-handoff.md §3 T5-5、docs/tag-server-design.md
//! §9 T5 行、docs/plan.md W5 の残項目「実機検証」に対応）。
//!
//! 既定の対象は三菱 R08ENCPU（iQ-R、`192.168.11.200:5200`、CPU 種別 R）。
//! #605 で接続先の IP・ポート・CPU 種別（Q / R）を引数で変えられる。
//! 乾燥炉の MELSEC-Q（QJ71E71-100）を見るときは、その PLC 用の許可アドレスを
//! `--safe-d` と `--safe-m` で渡す。元の R 機用 D3000–D4999 / M1000–M2000 は
//! 別の機械の安全範囲としては使わない。
//!
//! 1. フェーズ1: 基本接続・読み取り
//! 2. フェーズ2: 通常書き込み + ビット単位書き込み（RMW）
//! 3. フェーズ3: MELSEC 文字列デバイス（Shift-JIS）の往復
//! 4. フェーズ4: 同時 SLMP セッション数の上限
//!
//! ## 安全策
//!
//! - 書き込みを伴うフェーズ2・3は `REAL_PLC_CONFIRM_WRITE=1` のときだけ。
//! - アドレスは安全範囲の中だけ。範囲の入口は [`Ctx::addr`]。
//! - 試験の後は 0 や false で埋めず、書き込む前に読んだ元の値を戻す
//!   （`[restore]`）。試験中の true/false 書き込み（`[trial]`）とは別。
//!   SLMP は PLC の拒否を呼び出しの `Err` ではなく個別の `WriteResult::Bad`
//!   で返す。復元は全件の成功と件数を見てから読み戻し、元の値と違うときは
//!   アドレスと元の値を出して失敗する（成功の表示には進まない）。
//! - 各フェーズは前段が失敗したら止まる。タイムアウトは数秒。
//!
//! ## 実行方法
//!
//! ```text
//! cargo run -p banto-drivers --example real_plc_verify
//! REAL_PLC_CONFIRM_WRITE=1 cargo run -p banto-drivers --example real_plc_verify
//! cargo run -p banto-drivers --example real_plc_verify -- --host 192.168.1.50 --port 5007 --cpu q --safe-d 1000-1099 --safe-m 200-299
//! ```
//!
//! `--cpu q` などで既定の R 機から外れたときは、書き込みだけでなく読み取りも
//! `--safe-d` / `--safe-m` が必要（存在しないデバイスを叩かないため）。
//! D は 26 語以上、M は 16 点以上。乾燥炉 PLC の許可アドレス・書いてよい値・
//! 戻せなくなったときの復旧手順を、実行前に確認する。

use std::time::Duration;

use banto_broker::{BackoffConfig, BrokerError, BrokerSupervisor};
use banto_drivers::builtin;
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, DataType, PlcClient, PlcValue, ReadRequest,
    ReadResult, SlmpClient, SlmpConfig, SlmpCpu, StringReadRequest, TagValue,
};
use banto_plc_write::{
    BatchWriteRequest, PlcWriteClient, SlmpWriteClient, StringWriteRequest, WriteRequest,
    WriteResult,
};
use banto_tags::PlcConnection;

/// オーナーが R08ENCPU に指定した接続先。これ以外へ D3000 帯を流用しない。
const ORIGINAL_HOST: &str = "192.168.11.200";
const ORIGINAL_PORT: u16 = 5200;

/// フェーズが安全範囲の先頭から使う幅。D は 0..=25（26 語）、M は 0..=15。
const D_SPAN: u32 = 26;
const M_SPAN: u32 = 16;
const D_BIT_OFFSET: u32 = 10;
const D_STRING_OFFSET: u32 = 20;
const D_STRING_WORDS: u16 = 6;

/// 接続先。既定は R08ENCPU。
struct Target {
    host: String,
    port: u16,
    cpu: SlmpCpu,
}

/// 読んで書いてよいデバイス番号の閉区間。
#[derive(Debug)]
struct SafeRange {
    d_min: u32,
    d_max: u32,
    m_min: u32,
    m_max: u32,
}

struct Cli {
    host: String,
    port: u16,
    cpu: SlmpCpu,
    safe_d: Option<(u32, u32)>,
    safe_m: Option<(u32, u32)>,
    help: bool,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            host: ORIGINAL_HOST.to_string(),
            port: ORIGINAL_PORT,
            cpu: SlmpCpu::R,
            safe_d: None,
            safe_m: None,
            help: false,
        }
    }
}

/// この実行の接続先と安全範囲。アドレス生成の唯一の入口を持つ。
struct Ctx {
    target: Target,
    range: SafeRange,
}

impl Ctx {
    fn config(&self) -> SlmpConfig {
        SlmpConfig {
            host: self.target.host.clone(),
            port: self.target.port,
            cpu: self.target.cpu,
            connect_timeout: Duration::from_secs(3),
            response_timeout: Duration::from_secs(3),
            ..Default::default()
        }
    }

    fn d(&self, offset: u32) -> String {
        format!("D{}", self.range.d_min + offset)
    }

    fn m(&self, offset: u32) -> String {
        format!("M{}", self.range.m_min + offset)
    }

    /// 安全範囲の外は panic する。試験アドレスはすべてここを通る。
    fn addr(&self, raw: &str) -> Address {
        assert_in_range(raw, &self.range);
        Address::parse_slmp(raw).unwrap_or_else(|e| panic!("address {raw} should parse: {e}"))
    }
}

fn assert_in_range(raw: &str, range: &SafeRange) {
    let base = raw.split('.').next().unwrap_or(raw);
    if base.len() < 2 {
        panic!("安全のため、アドレス {raw} を判定できません");
    }
    let (device, num_str) = base.split_at(1);
    let num: u32 = num_str
        .parse()
        .unwrap_or_else(|_| panic!("安全のため、アドレス {raw} のデバイス番号を解析できません"));
    let in_range = match device {
        "D" => (range.d_min..=range.d_max).contains(&num),
        "M" => (range.m_min..=range.m_max).contains(&num),
        _ => false,
    };
    if !in_range {
        panic!(
            "安全のため、アドレス {raw} は許可範囲外です（D{}-D{} / M{}-M{} のみ）",
            range.d_min, range.d_max, range.m_min, range.m_max
        );
    }
}

fn original_range() -> SafeRange {
    SafeRange {
        d_min: 3000,
        d_max: 4999,
        m_min: 1000,
        m_max: 2000,
    }
}

fn is_original_r_machine(target: &Target) -> bool {
    target.host == ORIGINAL_HOST && target.port == ORIGINAL_PORT && target.cpu == SlmpCpu::R
}

/// 既定の R 機だけが D3000 帯を使える。それ以外は明示した範囲が必要で、
/// 幅がフェーズのオフセットに足りなければ拒否する。
fn resolve_range(
    target: &Target,
    safe_d: Option<(u32, u32)>,
    safe_m: Option<(u32, u32)>,
) -> Result<SafeRange, String> {
    match (safe_d, safe_m) {
        (None, None) if is_original_r_machine(target) => Ok(original_range()),
        (None, None) => Err(
            "この接続先は R08ENCPU（192.168.11.200:5200、CPU R）ではありません。\
             D3000–D4999 / M1000–M2000 を安全範囲として使いません。\
             乾燥炉 PLC の許可アドレス・書いてよい値・戻せなくなったときの復旧手順を確認し、\
             --safe-d MIN-MAX と --safe-m MIN-MAX を指定してください\
             （D は 26 語以上、M は 16 点以上）。"
                .to_string(),
        ),
        (Some(_), None) | (None, Some(_)) => {
            Err("--safe-d と --safe-m は両方指定してください".to_string())
        }
        (Some((d_min, d_max)), Some((m_min, m_max))) => {
            if d_min > d_max || m_min > m_max {
                return Err("安全範囲は MIN-MAX（MIN <= MAX）で指定してください".to_string());
            }
            let d_len = d_max - d_min + 1;
            let m_len = m_max - m_min + 1;
            if d_len < D_SPAN || m_len < M_SPAN {
                return Err(format!(
                    "安全範囲が狭すぎます（D は {D_SPAN} 語以上、M は {M_SPAN} 点以上。指定は D {d_len} 語、M {m_len} 点）"
                ));
            }
            Ok(SafeRange {
                d_min,
                d_max,
                m_min,
                m_max,
            })
        }
    }
}

fn parse_cpu(raw: &str) -> Result<SlmpCpu, String> {
    match raw.to_ascii_lowercase().as_str() {
        "q" => Ok(SlmpCpu::Q),
        "r" => Ok(SlmpCpu::R),
        other => Err(format!("--cpu は q または r です（受け取った値: {other}）")),
    }
}

fn parse_range(raw: &str) -> Result<(u32, u32), String> {
    let (lo, hi) = raw
        .split_once('-')
        .ok_or_else(|| format!("範囲は MIN-MAX です（受け取った値: {raw}）"))?;
    let lo: u32 = lo
        .parse()
        .map_err(|_| format!("範囲の開始が数値ではありません: {raw}"))?;
    let hi: u32 = hi
        .parse()
        .map_err(|_| format!("範囲の終了が数値ではありません: {raw}"))?;
    Ok((lo, hi))
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let need = |flag: &str| -> Result<String, String> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} には値が必要です"))
        };
        match arg.as_str() {
            "--host" => {
                cli.host = need("--host")?;
                i += 2;
            }
            "--port" => {
                let raw = need("--port")?;
                cli.port = raw
                    .parse()
                    .map_err(|_| format!("--port が数値ではありません: {raw}"))?;
                i += 2;
            }
            "--cpu" => {
                cli.cpu = parse_cpu(&need("--cpu")?)?;
                i += 2;
            }
            "--safe-d" => {
                cli.safe_d = Some(parse_range(&need("--safe-d")?)?);
                i += 2;
            }
            "--safe-m" => {
                cli.safe_m = Some(parse_range(&need("--safe-m")?)?);
                i += 2;
            }
            "--help" | "-h" => {
                cli.help = true;
                i += 1;
            }
            other => return Err(format!("不明な引数です: {other}")),
        }
    }
    Ok(cli)
}

fn cpu_name(cpu: SlmpCpu) -> &'static str {
    match cpu {
        SlmpCpu::Q => "Q",
        SlmpCpu::R => "R",
        SlmpCpu::L => "L",
    }
}

fn fmt_word_result(r: &ReadResult) -> String {
    match r {
        ReadResult::Value(TagValue::F64(v)) => {
            let w = *v as i64 as u16;
            format!("{v} (0x{w:04X}, 0b{w:016b})")
        }
        other => format!("{other:?}"),
    }
}

fn word_number(r: &ReadResult) -> Result<f64, String> {
    match r {
        ReadResult::Value(TagValue::F64(v)) => Ok(*v),
        other => Err(format!("元のワードを読めませんでした: {other:?}")),
    }
}

fn bit_number(r: &ReadResult) -> Result<bool, String> {
    match r {
        ReadResult::Value(TagValue::Bit(v)) => Ok(*v),
        other => Err(format!("元のビットを読めませんでした: {other:?}")),
    }
}

async fn write_once(ctx: &Ctx, requests: &[BatchWriteRequest]) -> Result<Vec<WriteResult>, String> {
    let mut client = SlmpWriteClient::new(ctx.config());
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

async fn read_once(ctx: &Ctx, requests: &[ReadRequest]) -> Result<Vec<ReadResult>, String> {
    let mut client = SlmpClient::new(ctx.config());
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

async fn read_mixed_once(
    ctx: &Ctx,
    requests: &[BatchReadRequest],
) -> Result<Vec<BatchReadResult>, String> {
    let mut client = SlmpClient::new(ctx.config());
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

fn u16_read(ctx: &Ctx, raw: &str) -> ReadRequest {
    ReadRequest {
        address: ctx.addr(raw),
        data_type: DataType::U16,
    }
}

async fn phase1_read_only(ctx: &Ctx) -> Result<(), String> {
    println!("--- フェーズ1: 基本接続・読み取り（SLMP互換性） ---");
    let mut client = SlmpClient::new(ctx.config());
    println!(
        "接続中: {}:{} (SLMP, CPU {}) ...",
        ctx.target.host,
        ctx.target.port,
        cpu_name(ctx.target.cpu)
    );
    client
        .connect()
        .await
        .map_err(|e| format!("接続失敗: {e}"))?;
    println!("接続成功。");

    let mut requests: Vec<ReadRequest> = Vec::new();
    for offset in 0..10u32 {
        let raw = ctx.d(offset);
        requests.push(u16_read(ctx, &raw));
    }
    for offset in 0..16u32 {
        let raw = ctx.m(offset);
        requests.push(ReadRequest {
            address: ctx.addr(&raw),
            data_type: DataType::Bit,
        });
    }

    let results = client
        .read_batch(&requests)
        .await
        .map_err(|e| format!("read_batch 失敗: {e}"))?;

    println!("D 先頭 10 語（{} から）:", ctx.d(0));
    for (i, r) in results.iter().take(10).enumerate() {
        println!("  {} = {}", ctx.d(i as u32), fmt_word_result(r));
    }
    println!("M 先頭 16 点（{} から）:", ctx.m(0));
    for (i, r) in results.iter().skip(10).enumerate() {
        println!("  {} = {:?}", ctx.m(i as u32), r);
    }

    client.disconnect().await;
    println!("切断しました。");
    Ok(())
}

/// 復元の 1 点。失敗したときに、アドレスと手で戻す元の値を出せるようにする。
struct RestoreItem {
    address: String,
    original: String,
}

fn format_word(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && (0.0..=f64::from(u16::MAX)).contains(&value) {
        let word = value as u16;
        format!("{value} (0x{word:04X})")
    } else {
        value.to_string()
    }
}

fn format_bit(value: bool) -> String {
    value.to_string()
}

fn restore_needed(items: &[RestoreItem]) -> String {
    let pairs = items
        .iter()
        .map(|item| format!("{}={}", item.address, item.original))
        .collect::<Vec<_>>()
        .join("、");
    format!("手で戻すときの元の値: {pairs}")
}

/// 復元書き込みが PLC に届いたかを検査する。
///
/// `write_once` の外側が `Ok` でも、SLMP のデバイス拒否は
/// [`WriteResult::Bad`] になる（`banto-plc-write` の `execute_slmp_writes`）。
/// 件数不足も、個別の `Bad` も復元失敗にする。成功の表示はこの関数が
/// `Ok` を返したあとだけ。
fn require_restore_accepted(
    kind: &str,
    items: &[RestoreItem],
    results: &[WriteResult],
) -> Result<(), String> {
    if results.len() != items.len() {
        return Err(format!(
            "{kind}の復元: 書き込み結果が {} 件、対象が {} 件で一致しません。{}",
            results.len(),
            items.len(),
            restore_needed(items),
        ));
    }
    let rejected: Vec<String> = results
        .iter()
        .zip(items)
        .filter(|(result, _)| !result.is_ok())
        .map(|(result, item)| {
            format!(
                "{}（元の値 {}、結果 {result:?}）",
                item.address, item.original
            )
        })
        .collect();
    if !rejected.is_empty() {
        return Err(format!(
            "{kind}の復元: PLC が書き込みを拒否しました（呼び出しは成功、個別結果は Bad）。{}",
            rejected.join("、")
        ));
    }
    Ok(())
}

/// 読み戻しが元の値と一致するかを検査する。違う点はアドレスと元の値を出す。
fn require_restore_readback(
    kind: &str,
    items: &[RestoreItem],
    actuals: &[String],
) -> Result<(), String> {
    if actuals.len() != items.len() {
        return Err(format!(
            "{kind}の復元: 読み戻しが {} 件、対象が {} 件で一致しません。{}",
            actuals.len(),
            items.len(),
            restore_needed(items),
        ));
    }
    let mismatched: Vec<String> = items
        .iter()
        .zip(actuals)
        .filter(|(item, actual)| item.original != **actual)
        .map(|(item, actual)| {
            format!(
                "{}（元の値 {}、読み戻し {actual}）",
                item.address, item.original
            )
        })
        .collect();
    if !mismatched.is_empty() {
        return Err(format!(
            "{kind}の復元: 読み戻しが元の値と一致しません。{}",
            mismatched.join("、")
        ));
    }
    Ok(())
}

async fn restore_words(ctx: &Ctx, offsets: &[u32], values: &[f64]) -> Result<(), String> {
    if offsets.len() != values.len() {
        return Err(format!(
            "ワードの復元: 対象 {} 件と元の値 {} 件が一致しません",
            offsets.len(),
            values.len()
        ));
    }
    let items: Vec<RestoreItem> = offsets
        .iter()
        .zip(values)
        .map(|(&offset, &value)| RestoreItem {
            address: ctx.d(offset),
            original: format_word(value),
        })
        .collect();
    let writes: Vec<BatchWriteRequest> = offsets
        .iter()
        .zip(values)
        .map(|(&offset, &value)| {
            let raw = ctx.d(offset);
            BatchWriteRequest::Numeric(WriteRequest {
                address: ctx.addr(&raw),
                data_type: DataType::U16,
                value: TagValue::F64(value),
            })
        })
        .collect();
    let result = write_once(ctx, &writes)
        .await
        .map_err(|err| format!("{err}。{}", restore_needed(&items)))?;
    require_restore_accepted("ワード", &items, &result)?;
    let reads: Vec<ReadRequest> = items
        .iter()
        .map(|item| u16_read(ctx, &item.address))
        .collect();
    let read_back = read_once(ctx, &reads)
        .await
        .map_err(|err| format!("{err}。{}", restore_needed(&items)))?;
    let actuals: Vec<String> = read_back
        .iter()
        .map(|got| match word_number(got) {
            Ok(value) => format_word(value),
            Err(err) => format!("読めない: {err}"),
        })
        .collect();
    require_restore_readback("ワード", &items, &actuals)?;
    println!(
        "[restore] ワードを元の値へ戻し、読み戻しで一致を確認しました: {}",
        restore_needed(&items)
    );
    Ok(())
}

/// 1 点のビットを元の値へ戻し、受理と読み戻しの両方を確認する。
async fn restore_bit(ctx: &Ctx, raw: &str, saved: bool) -> Result<(), String> {
    let items = vec![RestoreItem {
        address: raw.to_string(),
        original: format_bit(saved),
    }];
    let result = write_once(
        ctx,
        &[BatchWriteRequest::Numeric(WriteRequest {
            address: ctx.addr(raw),
            data_type: DataType::Bit,
            value: TagValue::Bit(saved),
        })],
    )
    .await
    .map_err(|err| format!("{err}。{}", restore_needed(&items)))?;
    require_restore_accepted("ビット", &items, &result)?;
    let read_back = read_once(
        ctx,
        &[ReadRequest {
            address: ctx.addr(raw),
            data_type: DataType::Bit,
        }],
    )
    .await
    .map_err(|err| format!("{err}。{}", restore_needed(&items)))?;
    let actual = match read_back.first().map(bit_number) {
        Some(Ok(value)) => format_bit(value),
        Some(Err(err)) => format!("読めない: {err}"),
        None => "結果なし".to_string(),
    };
    require_restore_readback("ビット", &items, &[actual])?;
    println!("[restore] {raw} を元の値 {saved} へ戻し、読み戻しで一致を確認しました");
    Ok(())
}

async fn phase2_write_and_bit_rmw(ctx: &Ctx) -> Result<(), String> {
    println!("--- フェーズ2: 書き込み + Bit RMW ---");
    let pattern: [u16; 5] = [0x1111, 0x2222, 0x3333, 0x4444, 0x5555];
    let pattern_offsets: Vec<u32> = (0..5).collect();
    let pattern_reads: Vec<ReadRequest> = pattern_offsets
        .iter()
        .map(|offset| {
            let raw = ctx.d(*offset);
            u16_read(ctx, &raw)
        })
        .collect();
    let saved_pattern = read_once(ctx, &pattern_reads).await?;
    let saved_pattern_values = saved_pattern
        .iter()
        .map(word_number)
        .collect::<Result<Vec<_>, _>>()?;
    let bit_raw = ctx.d(D_BIT_OFFSET);
    let saved_bit_word = word_number(&read_once(ctx, &[u16_read(ctx, &bit_raw)]).await?[0])?;
    let m_raw = ctx.m(0);
    let saved_m = bit_number(
        &read_once(
            ctx,
            &[ReadRequest {
                address: ctx.addr(&m_raw),
                data_type: DataType::Bit,
            }],
        )
        .await?[0],
    )?;
    println!(
        "[snapshot] 試験前の値を記録しました（復元はこの値です。0/false へのクリアではありません）"
    );

    let trial = phase2_trial(ctx, &pattern).await;
    let restored = restore_phase2(
        ctx,
        &pattern_offsets,
        &saved_pattern_values,
        saved_bit_word,
        saved_m,
    )
    .await;
    match (trial, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), Ok(())) => Err(format!("{err}（元の値への復元は成功しました）")),
        (Ok(()), Err(err)) => Err(format!(
            "試験は成功しましたが、元の値へ戻せませんでした: {err}"
        )),
        (Err(err), Err(restore_err)) => {
            Err(format!("{err}（元の値への復元も失敗: {restore_err}）"))
        }
    }
}

async fn phase2_trial(ctx: &Ctx, pattern: &[u16; 5]) -> Result<(), String> {
    let writes: Vec<BatchWriteRequest> = pattern
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let raw = ctx.d(i as u32);
            BatchWriteRequest::Numeric(WriteRequest {
                address: ctx.addr(&raw),
                data_type: DataType::U16,
                value: TagValue::F64(v as f64),
            })
        })
        .collect();
    let write_results = write_once(ctx, &writes).await?;
    println!("[trial] パターン書込結果: {write_results:?}");

    let reads: Vec<ReadRequest> = (0..5)
        .map(|i| {
            let raw = ctx.d(i);
            u16_read(ctx, &raw)
        })
        .collect();
    let read_back = read_once(ctx, &reads).await?;
    let mut all_matched = true;
    for (i, (expected, got)) in pattern.iter().zip(read_back.iter()).enumerate() {
        let matched = matches!(got, ReadResult::Value(TagValue::F64(v)) if *v == *expected as f64);
        all_matched &= matched;
        println!(
            "  [trial] {}: 期待値=0x{:04X} 実測={} 一致={}",
            ctx.d(i as u32),
            expected,
            fmt_word_result(got),
            matched
        );
    }
    if !all_matched {
        return Err("パターン書込/読戻が一致しませんでした".to_string());
    }

    let bit_word = ctx.d(D_BIT_OFFSET);
    for bit in 0..3u8 {
        let bit_text = format!("{bit_word}.{bit}");
        let w = write_once(
            ctx,
            &[BatchWriteRequest::BitInWord {
                address: ctx.addr(&bit_text),
                value: true,
            }],
        )
        .await?;
        println!("  [trial] {bit_text}=true 書込結果={w:?}");
        if !matches!(w.first(), Some(WriteResult::Ok)) {
            return Err(format!(
                "{bit_text} への RMW 書込が Ok を返しませんでした: {w:?}"
            ));
        }
    }
    // 試験としてビットを戻せることを確認する。これは復元ではない。
    for bit in 0..3u8 {
        let bit_text = format!("{bit_word}.{bit}");
        let w = write_once(
            ctx,
            &[BatchWriteRequest::BitInWord {
                address: ctx.addr(&bit_text),
                value: false,
            }],
        )
        .await?;
        println!("  [trial] {bit_text}=false 試験結果={w:?}");
    }

    let m_text = ctx.m(0);
    for value in [true, false] {
        let w = write_once(
            ctx,
            &[BatchWriteRequest::Numeric(WriteRequest {
                address: ctx.addr(&m_text),
                data_type: DataType::Bit,
                value: TagValue::Bit(value),
            })],
        )
        .await?;
        let r = read_once(
            ctx,
            &[ReadRequest {
                address: ctx.addr(&m_text),
                data_type: DataType::Bit,
            }],
        )
        .await?;
        println!("[trial] {m_text}={value} 書込結果={w:?} 読戻={r:?}");
        if !matches!(r.first(), Some(ReadResult::Value(TagValue::Bit(b))) if *b == value) {
            return Err(format!(
                "{m_text}={value} の書込/読戻が一致しませんでした: {r:?}"
            ));
        }
    }
    Ok(())
}

async fn restore_phase2(
    ctx: &Ctx,
    pattern_offsets: &[u32],
    saved_pattern: &[f64],
    saved_bit_word: f64,
    saved_m: bool,
) -> Result<(), String> {
    // 1 組が拒否・不一致でも、残りの組は必ず試みる（`?` で打ち切ると、通信が生きていて
    // 戻せたはずの領域が試験値のまま残る）。失敗は全部集めて、元の値つきで報告する。
    let m_raw = ctx.m(0);
    run_restore_steps(vec![
        Box::pin(restore_words(ctx, pattern_offsets, saved_pattern)),
        Box::pin(restore_words(ctx, &[D_BIT_OFFSET], &[saved_bit_word])),
        Box::pin(restore_bit(ctx, &m_raw, saved_m)),
    ])
    .await
}

type RestoreStep<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + 'a>>;

/// 復元の各組を先頭から順にすべて実行し、失敗をまとめて返す。
/// どの組が先に失敗しても、あとの組は飛ばさない。
async fn run_restore_steps(steps: Vec<RestoreStep<'_>>) -> Result<(), String> {
    let total = steps.len();
    let mut failures = Vec::new();
    for step in steps {
        if let Err(err) = step.await {
            failures.push(err);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "復元できなかった組が {} / {total} 件あります。{}",
            failures.len(),
            failures.join(" / ")
        ))
    }
}

async fn phase3_string_roundtrip(ctx: &Ctx) -> Result<(), String> {
    println!("--- フェーズ3: 文字列/Shift-JIS 往復検証 ---");
    let test_string = "テスト123";
    let words = D_STRING_WORDS;
    let offsets: Vec<u32> = (0..words as u32).map(|i| D_STRING_OFFSET + i).collect();
    let reads: Vec<ReadRequest> = offsets
        .iter()
        .map(|offset| {
            let raw = ctx.d(*offset);
            u16_read(ctx, &raw)
        })
        .collect();
    let saved = read_once(ctx, &reads).await?;
    let saved_values = saved
        .iter()
        .map(word_number)
        .collect::<Result<Vec<_>, _>>()?;
    println!("[snapshot] 文字列領域の元のワードを記録しました");

    let raw = ctx.d(D_STRING_OFFSET);
    let trial = async {
        let write_result = write_once(
            ctx,
            &[BatchWriteRequest::String(StringWriteRequest {
                address: ctx.addr(&raw),
                words,
                value: test_string.to_string(),
                encoding: banto_plc_write::StringEncoding::ShiftJis,
            })],
        )
        .await?;
        println!("[trial] 文字列書込結果: {write_result:?}");
        let read_result = read_mixed_once(
            ctx,
            &[BatchReadRequest::String(StringReadRequest {
                address: ctx.addr(&raw),
                words,
                encoding: banto_plc::StringEncoding::ShiftJis,
            })],
        )
        .await?;
        println!("[trial] 文字列読戻結果: {read_result:?}");
        let matched = matches!(
            &read_result[..],
            [BatchReadResult::Value(PlcValue::Str(s))] if s == test_string
        );
        if !matched {
            return Err(format!(
                "文字列往復が一致しませんでした: 実測={read_result:?}"
            ));
        }
        Ok(())
    }
    .await;
    let restored = restore_words(ctx, &offsets, &saved_values).await;
    match (trial, restored) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(err), Ok(())) => Err(format!("{err}（元のワードへの復元は成功しました）")),
        (Ok(()), Err(err)) => Err(format!(
            "試験は成功しましたが、元のワードへ戻せませんでした: {err}"
        )),
        (Err(err), Err(restore_err)) => {
            Err(format!("{err}（元のワードへの復元も失敗: {restore_err}）"))
        }
    }
}

async fn phase4_session_limit(ctx: &Ctx) -> Result<(), String> {
    println!("--- フェーズ4: 同時セッション数上限 ---");
    let probe = ctx.d(0);
    // R2b-3a-2: the broker dials through the registry with the connection's
    // `settings` (the flat columns are only a projection of them).
    let serde_json::Value::Object(settings) = serde_json::json!({
        "host": ctx.target.host.clone(),
        "port": ctx.target.port,
        "word_order": "low_high",
        "cpu_type": cpu_name(ctx.target.cpu),
    }) else {
        unreachable!("a JSON object literal")
    };
    let connection = PlcConnection::from_settings(
        1,
        "t5-5-verify".to_string(),
        "slmp".to_string(),
        true,
        false,
        settings,
        1,
    );

    let supervisor = BrokerSupervisor::spawn(
        &[connection],
        BackoffConfig::default(),
        std::sync::Arc::new(builtin().registry),
    )
    .map_err(|e| format!("broker spawn 失敗: {e}"))?;
    let handle = supervisor
        .handle(1)
        .ok_or_else(|| "broker handle が見つかりません".to_string())?;
    let probe_kind = banto_driver_api::TagKind::numeric(DataType::U16);
    let probe_spec = banto_driver_api::TagSpec::new(
        banto_driver_api::DriverBase::parse_address(
            &banto_driver_slmp::SlmpDriver,
            &ctx.addr(&probe).to_string(),
            &probe_kind,
        )
        .map_err(|e| format!("アドレス {probe}: {e}"))?,
        probe_kind,
    );

    println!("[4] broker セッション（セッション#1）接続待機中...");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let probe_result = handle.read(vec![probe_spec.clone()]).await;
        match probe_result {
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

    let mut second = SlmpClient::new(ctx.config());
    println!("[4] セッション#2（独立した SlmpClient）接続試行中...");
    let second_ok = match second.connect().await {
        Ok(()) => {
            println!("[4] セッション#2 接続成功。");
            let r = second
                .read_batch(&[ReadRequest {
                    address: ctx.addr(&probe),
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

    let third_ok = if second_ok {
        let mut third = SlmpClient::new(ctx.config());
        println!("[4] セッション#3（独立した SlmpClient）接続試行中...");
        match third.connect().await {
            Ok(()) => {
                println!("[4] セッション#3 接続成功。");
                let r = third
                    .read_batch(&[ReadRequest {
                        address: ctx.addr(&probe),
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
        }
    } else {
        println!("[4] セッション#2が失敗したため、セッション#3は試行しません。");
        false
    };

    second.disconnect().await;
    let concurrent_sessions = 1 + u32::from(second_ok) + u32::from(third_ok);
    println!(
        "[4] 実測: 少なくとも{concurrent_sessions}本の同時セッションを確認（#2={second_ok}, #3={third_ok}）"
    );
    supervisor.shutdown().await;
    Ok(())
}

fn usage() {
    println!(
        "\
使い方:
  real_plc_verify [--host IP] [--port N] [--cpu q|r] [--safe-d MIN-MAX] [--safe-m MIN-MAX]

既定: {ORIGINAL_HOST}:{ORIGINAL_PORT} CPU R（R08ENCPU）。この組み合わせだけ
D3000–D4999 / M1000–M2000 を安全範囲にします。
別の PLC（乾燥炉の Q シリーズを含む）では --safe-d と --safe-m が必要です。
書き込みフェーズは REAL_PLC_CONFIRM_WRITE=1 のときだけ実行します。
試験の後は 0 クリアではなく、読む前の値を戻します。"
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match parse_args(&args) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("{err}");
            usage();
            std::process::exit(2);
        }
    };
    if cli.help {
        usage();
        return;
    }
    let target = Target {
        host: cli.host,
        port: cli.port,
        cpu: cli.cpu,
    };
    let range = match resolve_range(&target, cli.safe_d, cli.safe_m) {
        Ok(range) => range,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };
    let ctx = Ctx { target, range };

    println!("=== T5-5 実機検証ツール ===");
    println!(
        "対象: {}:{} CPU {}",
        ctx.target.host,
        ctx.target.port,
        cpu_name(ctx.target.cpu)
    );
    println!(
        "安全範囲: D{}-D{} / M{}-M{}",
        ctx.range.d_min, ctx.range.d_max, ctx.range.m_min, ctx.range.m_max
    );
    if is_original_r_machine(&ctx.target) {
        println!("（R08ENCPU 向けにオーナーが指定した範囲です）");
    } else {
        println!("（この機械用に引数で指定した範囲です。R 機の D3000 帯は使っていません）");
    }
    println!();

    if let Err(e) = phase1_read_only(&ctx).await {
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
        if let Err(e) = phase4_session_limit(&ctx).await {
            eprintln!("[PHASE4] 失敗: {e}");
            std::process::exit(1);
        }
        println!("[PHASE4] 完了\n");
        println!("=== 検証終了（書込フェーズはスキップしました） ===");
        return;
    }

    if let Err(e) = phase2_write_and_bit_rmw(&ctx).await {
        eprintln!("[PHASE2] 失敗: {e}");
        eprintln!("フェーズ2が失敗したため、以降のフェーズを中止します。");
        std::process::exit(1);
    }
    println!("[PHASE2] 成功\n");

    if let Err(e) = phase3_string_roundtrip(&ctx).await {
        eprintln!("[PHASE3] 失敗: {e}");
        eprintln!("フェーズ3が失敗したため、フェーズ4を中止します。");
        std::process::exit(1);
    }
    println!("[PHASE3] 成功\n");

    if let Err(e) = phase4_session_limit(&ctx).await {
        eprintln!("[PHASE4] 失敗: {e}");
        std::process::exit(1);
    }
    println!("[PHASE4] 完了\n");

    println!("=== 全フェーズ完了 ===");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(host: &str, port: u16, cpu: SlmpCpu) -> Target {
        Target {
            host: host.to_string(),
            port,
            cpu,
        }
    }

    #[test]
    fn default_args_keep_the_original_r_machine() {
        let cli = parse_args(&[]).unwrap();
        assert_eq!(cli.host, ORIGINAL_HOST);
        assert_eq!(cli.port, ORIGINAL_PORT);
        assert_eq!(cli.cpu, SlmpCpu::R);
        assert!(cli.safe_d.is_none() && cli.safe_m.is_none());
        let range = resolve_range(
            &target(&cli.host, cli.port, cli.cpu),
            cli.safe_d,
            cli.safe_m,
        )
        .unwrap();
        assert_eq!(range.d_min, 3000);
        assert_eq!(range.m_min, 1000);
    }

    #[test]
    fn another_machine_refuses_the_r_machine_range() {
        let err = resolve_range(&target("192.168.1.50", 5007, SlmpCpu::Q), None, None).unwrap_err();
        assert!(err.contains("D3000"), "{err}");
        assert!(err.contains("乾燥炉"), "{err}");
    }

    #[test]
    fn same_host_with_q_cpu_still_requires_an_explicit_range() {
        let err = resolve_range(
            &target(ORIGINAL_HOST, ORIGINAL_PORT, SlmpCpu::Q),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("--safe-d"), "{err}");
    }

    #[test]
    fn explicit_range_is_used_and_must_be_wide_enough() {
        let range = resolve_range(
            &target("10.0.0.8", 5007, SlmpCpu::Q),
            Some((1000, 1099)),
            Some((200, 299)),
        )
        .unwrap();
        assert_eq!((range.d_min, range.m_min), (1000, 200));
        let err = resolve_range(
            &target("10.0.0.8", 5007, SlmpCpu::Q),
            Some((1000, 1010)),
            Some((200, 299)),
        )
        .unwrap_err();
        assert!(err.contains("狭すぎ"), "{err}");
    }

    #[test]
    fn cpu_flag_parses_q_and_r() {
        let cli = parse_args(&[
            "--host".into(),
            "10.0.0.8".into(),
            "--port".into(),
            "5007".into(),
            "--cpu".into(),
            "q".into(),
        ])
        .unwrap();
        assert_eq!(cli.cpu, SlmpCpu::Q);
        assert_eq!(cli.port, 5007);
        assert!(parse_args(&["--cpu".into(), "L".into()]).is_err());
    }

    fn word_item(address: &str, value: u16) -> RestoreItem {
        RestoreItem {
            address: address.to_string(),
            original: format_word(f64::from(value)),
        }
    }

    /// `write_once` が `Ok(vec![Bad])` を返したときと同じ。外側の成功だけで
    /// 復元成功にしない。失敗の文言にアドレスと元の値がある。
    #[test]
    fn restore_treats_an_inner_bad_as_failure() {
        let items = vec![word_item("D3000", 0x1111), word_item("D3001", 0x2222)];
        let results = vec![
            WriteResult::Ok,
            WriteResult::Bad(banto_plc_write::PlcWriteError::SlmpEndCode {
                code: 0xC059,
                message: "wrong command".to_string(),
            }),
        ];
        let err = require_restore_accepted("ワード", &items, &results).unwrap_err();
        assert!(err.contains("D3001"), "{err}");
        assert!(err.contains("0x2222"), "{err}");
        assert!(err.contains("Bad"), "{err}");
        assert!(!err.contains("戻し、読み戻しで一致"), "{err}");
    }

    #[test]
    fn restore_rejects_a_short_result_list() {
        let items = vec![word_item("D3000", 1)];
        let err = require_restore_accepted("ワード", &items, &[]).unwrap_err();
        assert!(err.contains("D3000"), "{err}");
        assert!(err.contains("0x0001"), "{err}");
    }

    #[test]
    fn restore_readback_mismatch_names_the_address_and_the_original() {
        let items = vec![RestoreItem {
            address: "M1000".to_string(),
            original: format_bit(false),
        }];
        let err = require_restore_readback("ビット", &items, &["true".to_string()]).unwrap_err();
        assert!(err.contains("M1000"), "{err}");
        assert!(err.contains("false"), "{err}");
        assert!(err.contains("true"), "{err}");
    }

    #[test]
    fn restore_accepts_matching_results_and_readback() {
        let items = vec![word_item("D3000", 0x1111)];
        require_restore_accepted("ワード", &items, &[WriteResult::Ok]).unwrap();
        require_restore_readback("ワード", &items, &[format_word(f64::from(0x1111u16))]).unwrap();
    }

    /// 最初の組だけが失敗しても、残り 2 組の復元は呼ばれ、報告に失敗した組の
    /// アドレスと元の値が残る。終わりの 2 組が失敗したときも同じ。
    #[tokio::test]
    async fn restore_steps_run_all_even_when_the_first_fails() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let step = |ok: Result<(), String>| -> RestoreStep<'_> {
            let calls = &calls;
            Box::pin(async move {
                calls.set(calls.get() + 1);
                ok
            })
        };
        let err = run_restore_steps(vec![
            step(Err(
                "ワードの復元: D3000（元の値 4369 (0x1111)、結果 Bad）".to_string()
            )),
            step(Ok(())),
            step(Ok(())),
        ])
        .await
        .unwrap_err();
        assert_eq!(calls.get(), 3, "後続の復元が飛ばされた");
        assert!(err.contains("D3000"), "{err}");
        assert!(err.contains("0x1111"), "{err}");
        assert!(err.contains("1 / 3"), "{err}");

        calls.set(0);
        let err = run_restore_steps(vec![
            step(Err("D3000 失敗（元の値 1）".to_string())),
            step(Ok(())),
            step(Err("M1000 失敗（元の値 true）".to_string())),
        ])
        .await
        .unwrap_err();
        assert_eq!(calls.get(), 3);
        assert!(err.contains("D3000") && err.contains("M1000"), "{err}");
        assert!(err.contains("2 / 3"), "{err}");

        calls.set(0);
        run_restore_steps(vec![step(Ok(())), step(Ok(())), step(Ok(()))])
            .await
            .unwrap();
        assert_eq!(calls.get(), 3);
    }
}
