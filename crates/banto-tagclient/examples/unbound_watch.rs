//! 実 Hub で `Unbound` → `Live` を目で確かめる example（#682 PR-3 の実機確認）。
//!
//! 指定した外部名（まだ Hub に無いタグ）だけで `start` し、状態が変わるたびに
//! 時刻付きの 1 行を出し続け、指定秒数で `shutdown` する。走らせている間に
//! Hub の UI でそのタグを作ると、`unbound` → `rebinding` → `live` と進むのが
//! 見える。購読 0 件の WS を長く保って `config_changed` が届くか
//! （設計書 §7 の 10 分確認）にも使う。
//!
//! ## 実行方法
//!
//! ```text
//! HUB_API_KEY=bh_xxxxxxxx cargo run --example unbound_watch -p banto-tagclient -- 900 plc.gs.new_tag
//! ```
//!
//! - 引数: 1 つ目が観察する秒数、2 つ目以降が外部名（`{connection}.{group}.{tag}`、
//!   1 つ以上）。binding key は外部名と同じにする。
//! - `HUB_URL`（省略時 `http://127.0.0.1:8722`）と `HUB_API_KEY`（必須）は
//!   `real_hub_smoke` と同じく環境変数で受け取る。API キーはソースに埋め込まない。
//! - 1 行の形:
//!   `<UTC 時刻> +<経過秒> state=<状態> last_error=<分類> gen=<世代> rev=<revision> resolved=<件数> unresolved=<件数> values=<件数|none>`。
//!   snapshot が無い状態（`unbound` / `live` 以外）では `gen` 以降を `-` にする。
//! - 状態は `state_watch` で受けるので、短い間に続いた変化はまとめて最新だけが
//!   見える（watch の性質）。

use std::env;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banto_tagclient::{BindingRequest, Endpoint, RestClient, SecretApiKey, TagClientState};

const DEFAULT_HUB_URL: &str = "http://127.0.0.1:8722";

fn utc_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        now.subsec_millis()
    )
}

fn describe(state: &TagClientState) -> String {
    let last_error = state
        .last_error()
        .map_or("-", |kind| kind.as_str())
        .to_owned();
    let tail = match state.snapshot() {
        Some(snapshot) => format!(
            "gen={} rev={} resolved={} unresolved={} values={}",
            snapshot.generation,
            snapshot.revision(),
            snapshot.resolution.resolved.len(),
            snapshot.resolution.unresolved.len(),
            snapshot.values.as_ref().map_or_else(
                || "none".to_owned(),
                |values| values.values.len().to_string()
            ),
        ),
        None => "gen=- rev=- resolved=- unresolved=- values=-".to_owned(),
    };
    format!(
        "state={} last_error={last_error} {tail}",
        state.connection_state()
    )
}

/// `real_hub_smoke` と同じ組み立て方。
fn build_client(hub_url: &str, api_key: &str) -> banto_tagclient::Result<RestClient> {
    let endpoint = Endpoint::new(hub_url)?;
    let secret = SecretApiKey::new(api_key.to_owned())?;
    RestClient::new(endpoint, secret)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = env::args().skip(1);
    let usage = "usage: unbound_watch <seconds> <external_name>...";
    let seconds: u64 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("{usage}");
            std::process::exit(2);
        });
    let names: Vec<String> = args.collect();
    if names.is_empty() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let hub_url = env::var("HUB_URL").unwrap_or_else(|_| DEFAULT_HUB_URL.to_owned());
    let Ok(api_key) = env::var("HUB_API_KEY") else {
        eprintln!("HUB_API_KEY is required");
        std::process::exit(2);
    };
    let client = build_client(&hub_url, &api_key).unwrap_or_else(|error| {
        eprintln!("client: {error}");
        std::process::exit(2);
    });
    let requests = names
        .iter()
        .map(|name| BindingRequest {
            binding_key: name.clone(),
            external_name: name.clone(),
        })
        .collect();
    let handle = client.start(requests).unwrap_or_else(|error| {
        eprintln!("start: {error}");
        std::process::exit(2);
    });
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    let mut watch = handle.state_watch();
    let mut last_line = String::new();
    loop {
        let line = describe(&watch.borrow_and_update());
        if line != last_line {
            println!(
                "{} +{:.3}s {line}",
                utc_now(),
                started.elapsed().as_secs_f64()
            );
            last_line = line;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            changed = watch.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    let result = handle.shutdown().await;
    println!(
        "{} +{:.3}s shutdown: {}",
        utc_now(),
        started.elapsed().as_secs_f64(),
        match &result {
            Ok(()) => "ok".to_owned(),
            Err(error) => format!("error {}", error.kind().as_str()),
        }
    );
}
