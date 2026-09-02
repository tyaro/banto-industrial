//! 実稼働中の banto-hub に対する `banto-tagclient` 実機検証ツール
//! （2026-09-01 オーナー指示）。
//!
//! catalog 取得・値読み取り・購読（WebSocket）・単一タグ書き込みという
//! 読み書きの主要経路を実 Hub 相手に一通り確認したうえで、特に
//! **403（`ErrorKind::WriteForbidden`）と 503（`ErrorKind::WriteUnavailable`）
//! を SDK 利用者が判別できること** を主眼に置く。`write.rs` の
//! `classify_write_status` が設計として区別している通り、403 は
//! 「そのタグには書けない」という設定・権限の問題（リトライしても直らない）、
//! 503 は「今は書けない」という一時的なサーバー状態（現場の運用判断で
//! リトライしてよい）であり、SDK 側のエラー種別がこの2つを取り違えると
//! 現場の対処（設定を直すべきか、しばらく待って再送すべきか）を誤らせる。
//!
//! ## 実行方法
//!
//! ```text
//! HUB_API_KEY=bh_xxxxxxxx cargo run --example real_hub_smoke -p banto-tagclient
//! ```
//!
//! - `HUB_URL`（省略時 `http://127.0.0.1:8722`）と `HUB_API_KEY`（必須）を
//!   環境変数で受け取る。API キーはソースに埋め込まない。
//! - `HUB_ADMIN_TOKEN`（任意）を設定すると、項目6が叩く
//!   `/api/write-control/{enable,disable}` に管理セッションの
//!   `Authorization: Bearer` ヘッダを付ける。試運転モードの Hub では
//!   この認証は不要だが、**ロックダウン済みの Hub（＝本番の実態）では
//!   このヘッダが無いと 401/403 になり項目6を実行できない**ため、その
//!   場合はこの環境変数に管理者の Bearer トークンを設定すること。
//!   トークンはソースに埋め込まない。
//! - 6項目それぞれが独立して合否を出す。前段（catalog 取得）が失敗した
//!   場合、以降の項目は「**実際に試して失敗した**」のではなく
//!   「**前段が失敗して検証できなかった＝スキップ**」として区別して
//!   記録したうえで最後まで実行し切る（途中で panic して残りの項目の
//!   情報を失わないため）。スキップも「検証できていない」という点では
//!   失敗と同様に扱い、終了コードは非ゼロのままにする。
//! - 項目6（503確認）は `POST /api/write-control/disable` で受付を
//!   意図的に無効化するため、検証後は必ず `POST /api/write-control/enable`
//!   で元に戻す。これを戻し忘れると、稼働中の Hub の以降の書き込みが
//!   全て 503 になってしまう。

use std::env;
use std::time::Duration;

use banto_tagclient::{
    BindingRequest, CatalogSnapshot, CatalogTag, Endpoint, ErrorKind, RequestedValue, RestClient,
    SecretApiKey, TagClientConnectionState, TagClientState,
};
use tokio::sync::watch;
use tokio::time::{sleep, timeout, Instant};

const DEFAULT_HUB_URL: &str = "http://127.0.0.1:8722";
const EXT_PLC_D3000: &str = "plc.gs.d3000";
const EXT_MB_HR1: &str = "mb.g1.hr1";
const EXT_MB_DI_RO: &str = "mb.g1.di_ro";
const LIVE_TIMEOUT: Duration = Duration::from_secs(5);
const UPDATE_WINDOW: Duration = Duration::from_secs(5);

/// 1項目の検証結果の状態。
///
/// 「実際に試して失敗した」（`Fail`）と「前段が失敗して検証できなかった」
/// （`Skipped`）は読み手にとって全く違う情報なので区別する。ただし
/// どちらも「検証できていない」という点では成功ではないため、終了コードの
/// 判定（[`StepStatus::is_pass`]）では両方とも非成功として扱う。
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepStatus {
    /// 実際に試して期待どおりだった。
    Pass,
    /// 実際に試したが期待と異なった、またはエラーになった。
    Fail,
    /// 前段の失敗により、この項目自体は試せなかった。
    Skipped,
}

impl StepStatus {
    /// サマリ表示用のマーク。
    fn mark(self) -> &'static str {
        match self {
            StepStatus::Pass => "OK",
            StepStatus::Fail => "NG",
            StepStatus::Skipped => "SKIP",
        }
    }

    /// 終了コード判定用。スキップは成功として扱わない
    /// （「検証できていない」のであって「成功した」わけではないため）。
    fn is_pass(self) -> bool {
        matches!(self, StepStatus::Pass)
    }
}

/// 1項目の検証結果（最後にまとめて表示するためのサマリ行）。
struct StepResult {
    name: &'static str,
    status: StepStatus,
    detail: String,
}

impl StepResult {
    fn new(name: &'static str, status: StepStatus, detail: impl Into<String>) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
        }
    }

    /// 実際に試した結果が bool で得られる場合の簡便コンストラクタ。
    fn from_bool(name: &'static str, ok: bool, detail: impl Into<String>) -> Self {
        Self::new(
            name,
            if ok {
                StepStatus::Pass
            } else {
                StepStatus::Fail
            },
            detail,
        )
    }

    /// 前段の失敗によりこの項目を試せなかった場合のコンストラクタ。
    fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, StepStatus::Skipped, detail)
    }
}

/// `Endpoint`・`SecretApiKey` から新しい `RestClient` を組み立てる。
///
/// `RestClient::start` が `self` を消費するため、購読用と読み書き用で
/// クライアントのインスタンスを分ける必要があり、この関数を都度呼び直す。
fn build_client(hub_url: &str, api_key: &str) -> banto_tagclient::Result<RestClient> {
    let endpoint = Endpoint::new(hub_url)?;
    let secret = SecretApiKey::new(api_key.to_owned())?;
    RestClient::new(endpoint, secret)
}

fn find_tag<'a>(catalog: &'a CatalogSnapshot, external_name: &str) -> Option<&'a CatalogTag> {
    catalog
        .tags
        .iter()
        .find(|tag| tag.external_name == external_name)
}

/// 1. catalog 取得。
async fn step1_catalog(hub_url: &str, api_key: &str) -> (StepResult, Option<CatalogSnapshot>) {
    println!("--- [1/6] catalog 取得 (fetch_catalog) ---");
    let client = match build_client(hub_url, api_key) {
        Ok(client) => client,
        Err(error) => {
            println!("  クライアント構築失敗: kind={}", error.kind().as_str());
            let result = StepResult::from_bool(
                "catalog取得",
                false,
                format!("client build error: {}", error.kind().as_str()),
            );
            return (result, None);
        }
    };
    match client.fetch_catalog().await {
        Ok(catalog) => {
            println!(
                "  成功: revision={} run_id={:?} tags={}",
                catalog.revision,
                catalog.run_id,
                catalog.tags.len()
            );
            for tag in catalog.tags.iter().take(8) {
                println!(
                    "   - {} id={:?} data_type={} writable={}",
                    tag.external_name,
                    tag.ids.as_array(),
                    tag.data_type,
                    tag.writable
                );
            }
            let detail = format!("tags={}", catalog.tags.len());
            (
                StepResult::from_bool("catalog取得", true, detail),
                Some(catalog),
            )
        }
        Err(error) => {
            println!("  失敗: kind={}", error.kind().as_str());
            let detail = error.kind().as_str().to_owned();
            (StepResult::from_bool("catalog取得", false, detail), None)
        }
    }
}

/// 2. 読み取り (`fetch_values`)。
async fn step2_read(hub_url: &str, api_key: &str) -> StepResult {
    println!("--- [2/6] 読み取り (fetch_values) ---");
    let client = match build_client(hub_url, api_key) {
        Ok(client) => client,
        Err(error) => {
            println!("  クライアント構築失敗: kind={}", error.kind().as_str());
            return StepResult::from_bool("読み取り", false, error.kind().as_str().to_owned());
        }
    };
    match client.fetch_values(&[EXT_PLC_D3000, EXT_MB_HR1]).await {
        Ok(snapshot) => {
            for value in &snapshot.values {
                println!(
                    "   - {} = {:?} (quality={}, t={})",
                    value.tag,
                    value.v,
                    value.q.as_str(),
                    value.t
                );
            }
            let detail = format!("{}件取得", snapshot.values.len());
            StepResult::from_bool("読み取り", !snapshot.values.is_empty(), detail)
        }
        Err(error) => {
            println!("  失敗: kind={}", error.kind().as_str());
            StepResult::from_bool("読み取り", false, error.kind().as_str().to_owned())
        }
    }
}

/// `target` の接続状態になるまで待つ（呼び出し側で全体タイムアウトをかける）。
async fn wait_for_state(
    watch: &mut watch::Receiver<TagClientState>,
    target: TagClientConnectionState,
) {
    loop {
        if watch.borrow().connection_state() == target {
            return;
        }
        if watch.changed().await.is_err() {
            return;
        }
    }
}

/// `window` 以内に現在のスナップショットの `t`（タイムスタンプ）が
/// 変化する（＝値が更新配信される）のを待つ。
async fn wait_for_value_update(
    watch: &mut watch::Receiver<TagClientState>,
    window: Duration,
) -> bool {
    let initial_t = watch.borrow().current().map(|snapshot| snapshot.t);
    let deadline = Instant::now() + window;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        match timeout(deadline - now, watch.changed()).await {
            Ok(Ok(())) => {
                let current_t = watch.borrow().current().map(|snapshot| snapshot.t);
                if current_t.is_some() && current_t != initial_t {
                    return true;
                }
            }
            _ => return false,
        }
    }
}

/// 3. 購読 (`start` → `TagClientHandle`)。数秒間 Live のまま値が更新されることを確認し、確認後は必ず `shutdown()` する。
async fn step3_subscribe(
    hub_url: &str,
    api_key: &str,
    catalog: Option<&CatalogSnapshot>,
) -> StepResult {
    println!("--- [3/6] 購読 (start / WebSocket) ---");
    let Some(catalog) = catalog else {
        println!("  スキップ: catalog取得に失敗したため stable_id を解決できません。");
        return StepResult::skipped("購読", "catalog未取得のためスキップ");
    };
    let Some(plc_tag) = find_tag(catalog, EXT_PLC_D3000) else {
        println!("  失敗: {EXT_PLC_D3000} が catalog に見つかりません。");
        return StepResult::from_bool("購読", false, "plc.gs.d3000がcatalogに存在しない");
    };
    let Some(mb_tag) = find_tag(catalog, EXT_MB_HR1) else {
        println!("  失敗: {EXT_MB_HR1} が catalog に見つかりません。");
        return StepResult::from_bool("購読", false, "mb.g1.hr1がcatalogに存在しない");
    };
    let requests = vec![
        BindingRequest {
            binding_key: "plc_d3000".to_owned(),
            stable_id: plc_tag.ids,
        },
        BindingRequest {
            binding_key: "mb_hr1".to_owned(),
            stable_id: mb_tag.ids,
        },
    ];
    let client = match build_client(hub_url, api_key) {
        Ok(client) => client,
        Err(error) => {
            println!("  クライアント構築失敗: kind={}", error.kind().as_str());
            return StepResult::from_bool("購読", false, error.kind().as_str().to_owned());
        }
    };
    let handle = match client.start(requests) {
        Ok(handle) => handle,
        Err(error) => {
            println!("  start失敗: kind={}", error.kind().as_str());
            return StepResult::from_bool("購読", false, error.kind().as_str().to_owned());
        }
    };

    let mut watch = handle.state_watch();
    let reached_live = timeout(
        LIVE_TIMEOUT,
        wait_for_state(&mut watch, TagClientConnectionState::Live),
    )
    .await
    .is_ok()
        && watch.borrow().connection_state() == TagClientConnectionState::Live;
    if !reached_live {
        let current_state = handle.state().connection_state();
        println!(
            "  失敗: {}秒以内にLiveへ到達しませんでした（現在={current_state}）",
            LIVE_TIMEOUT.as_secs()
        );
        let shutdown_result = handle.shutdown().await;
        println!("  shutdown: {}", describe_unit_result(&shutdown_result));
        return StepResult::from_bool("購読", false, "Live到達タイムアウト");
    }
    println!("  Live状態に到達しました。");

    // banto-hub の既定の購読は on-change 配信（tag-server-design.md /
    // subscribe_core.rs `Mode::OnChange`）で、値そのものが変化しない限り
    // 何もフレームが飛んでこない。ただ待つだけでは環境依存（誰も値を
    // 変えなければ何も届かない）になるため、ここで能動的に別クライアント
    // から plc.gs.d3000 へ書き込み、変化を発生させたうえで購読側にそれが
    // 伝搬するのを確認する。
    let trigger_value = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
        % 10_000) as f64;
    let trigger_result = match build_client(hub_url, api_key) {
        Ok(trigger_client) => {
            trigger_client
                .write_tag(plc_tag.ids, RequestedValue::Num(trigger_value))
                .await
        }
        Err(error) => Err(error),
    };
    match &trigger_result {
        Ok(()) => {
            println!("  値変化のトリガとして {EXT_PLC_D3000} <- {trigger_value} を書き込みました。")
        }
        Err(error) => println!(
            "  値変化のトリガ書き込みに失敗しました: kind={}（更新確認の信頼度が下がります）",
            error.kind().as_str()
        ),
    }

    let updated = wait_for_value_update(&mut watch, UPDATE_WINDOW).await;
    println!(
        "  {}秒以内の値更新確認: {}",
        UPDATE_WINDOW.as_secs(),
        if updated {
            "成功（更新を検出）"
        } else {
            "失敗（更新を検出できず）"
        }
    );

    let shutdown_result = handle.shutdown().await;
    println!("  shutdown: {}", describe_unit_result(&shutdown_result));

    let ok = updated && shutdown_result.is_ok();
    let detail = format!(
        "live到達=true, 値更新={updated}, shutdown成功={}",
        shutdown_result.is_ok()
    );
    StepResult::from_bool("購読", ok, detail)
}

fn describe_unit_result(result: &banto_tagclient::Result<()>) -> String {
    match result {
        Ok(()) => "成功".to_owned(),
        Err(error) => format!("失敗 kind={}", error.kind().as_str()),
    }
}

/// 4. 書き込み → 読み戻して一致を確認。
async fn step4_write(
    hub_url: &str,
    api_key: &str,
    catalog: Option<&CatalogSnapshot>,
) -> StepResult {
    println!("--- [4/6] 書き込み (write_tag) ---");
    let Some(catalog) = catalog else {
        println!("  スキップ: catalog取得に失敗したため stable_id を解決できません。");
        return StepResult::skipped("書き込み", "catalog未取得のためスキップ");
    };
    let Some(tag) = find_tag(catalog, EXT_PLC_D3000) else {
        println!("  失敗: {EXT_PLC_D3000} が catalog に見つかりません。");
        return StepResult::from_bool("書き込み", false, "plc.gs.d3000がcatalogに存在しない");
    };
    let client = match build_client(hub_url, api_key) {
        Ok(client) => client,
        Err(error) => {
            println!("  クライアント構築失敗: kind={}", error.kind().as_str());
            return StepResult::from_bool("書き込み", false, error.kind().as_str().to_owned());
        }
    };
    let test_value = 4242.0;
    match client
        .write_tag(tag.ids, RequestedValue::Num(test_value))
        .await
    {
        Ok(()) => {
            println!("  書込成功: {EXT_PLC_D3000} <- {test_value}");
            // banto-hub は tag の period_ms（実測1000ms）でPLC/Modbusを
            // ポーリングしてから current 値を更新するため、書込直後の
            // 読み取りはまだ旧値を返すことがある。ポーリング周期を跨ぐまで
            // 短い間隔でリトライする。
            const ATTEMPTS: u32 = 6;
            let mut got = None;
            let mut last_error = None;
            for attempt in 0..ATTEMPTS {
                match client.fetch_values(&[EXT_PLC_D3000]).await {
                    Ok(snapshot) => {
                        got = snapshot.values.first().and_then(|value| value.v);
                        if got == Some(test_value) {
                            break;
                        }
                    }
                    Err(error) => last_error = Some(error),
                }
                if attempt + 1 < ATTEMPTS {
                    sleep(Duration::from_millis(500)).await;
                }
            }
            let matched = got == Some(test_value);
            println!("  読戻し: {got:?} 一致={matched}");
            if !matched {
                if let Some(error) = last_error {
                    println!(
                        "  (読戻しの途中で一時的なエラーもありました: kind={})",
                        error.kind().as_str()
                    );
                }
            }
            StepResult::from_bool("書き込み", matched, format!("読戻={got:?}"))
        }
        Err(error) => {
            println!("  書込失敗: kind={}", error.kind().as_str());
            StepResult::from_bool("書き込み", false, error.kind().as_str().to_owned())
        }
    }
}

/// 5. writable=false タグへの書き込みが `ErrorKind::WriteForbidden`（403）になることを確認する。
async fn step5_forbidden(
    hub_url: &str,
    api_key: &str,
    catalog: Option<&CatalogSnapshot>,
) -> StepResult {
    println!("--- [5/6] 403 (writable=false) 確認 ---");
    let Some(catalog) = catalog else {
        println!("  スキップ: catalog取得に失敗したため stable_id を解決できません。");
        return StepResult::skipped("403確認", "catalog未取得のためスキップ");
    };
    let Some(tag) = find_tag(catalog, EXT_MB_DI_RO) else {
        println!("  失敗: {EXT_MB_DI_RO} が catalog に見つかりません。");
        return StepResult::from_bool("403確認", false, "mb.g1.di_roがcatalogに存在しない");
    };
    if tag.writable {
        println!("  警告: {EXT_MB_DI_RO} は writable=true でした（想定は false）。");
    }
    let client = match build_client(hub_url, api_key) {
        Ok(client) => client,
        Err(error) => {
            println!("  クライアント構築失敗: kind={}", error.kind().as_str());
            return StepResult::from_bool("403確認", false, error.kind().as_str().to_owned());
        }
    };
    match client.write_tag(tag.ids, RequestedValue::Bool(true)).await {
        Ok(()) => {
            println!("  想定外: writable=false のタグへの書込が成功してしまいました。");
            StepResult::from_bool("403確認", false, "書込が成功してしまった（本来403のはず）")
        }
        Err(error) => {
            let matched = error.kind() == ErrorKind::WriteForbidden;
            println!(
                "  結果: kind={} (期待=write_forbidden) 一致={matched}",
                error.kind().as_str()
            );
            let detail = format!("kind={}", error.kind().as_str());
            StepResult::from_bool("403確認", matched, detail)
        }
    }
}

/// `POST /api/write-control/{enable|disable}` を叩く。
///
/// 試運転モードの Hub では管理セッションの Bearer 認証は不要で、CSRF
/// ガード用の `X-Banto-Client` ヘッダのみで通る。ただし**ロックダウン済み
/// の Hub（＝本番の実態）ではこれだけでは 401/403 になる**ため、
/// `admin_token`（環境変数 `HUB_ADMIN_TOKEN` 由来。未設定なら `None`）が
/// 与えられていれば `Authorization: Bearer` ヘッダを付ける。
async fn set_write_control(
    http: &reqwest::Client,
    hub_url: &str,
    enable: bool,
    admin_token: Option<&str>,
) -> Result<(), String> {
    let action = if enable { "enable" } else { "disable" };
    let base = hub_url.trim_end_matches('/');
    let url = format!("{base}/api/write-control/{action}");
    let mut request = http.post(url).header("X-Banto-Client", "banto");
    if let Some(token) = admin_token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("送信失敗: {error}"))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        let hint = if admin_token.is_some() {
            "HUB_ADMIN_TOKEN の値が無効か、権限が不足している可能性があります。"
        } else {
            "ロックダウン済みの Hub では管理セッションの認証が必要です。環境変数 HUB_ADMIN_TOKEN に管理者の Bearer トークンを設定してください（試運転モードでは不要）。"
        };
        Err(format!(
            "HTTPステータス={status}（認証/権限エラー）。{hint}"
        ))
    } else {
        Err(format!("HTTPステータス={status}"))
    }
}

/// 6. 書き込み受付を無効化した状態での書き込みが `ErrorKind::WriteUnavailable`（503）になることを確認する。
///
/// **後始末**: この関数に入って `disable` を試みた後は、テストの成否に
/// 関わらず必ず `enable` へ戻す（戻し忘れは以降の全書き込みを503にする）。
async fn step6_unavailable(
    hub_url: &str,
    api_key: &str,
    catalog: Option<&CatalogSnapshot>,
    admin_token: Option<&str>,
) -> StepResult {
    println!("--- [6/6] 503 (write-control 無効化) 確認 ---");
    let Some(catalog) = catalog else {
        println!("  スキップ: catalog取得に失敗したため stable_id を解決できません。");
        return StepResult::skipped("503確認", "catalog未取得のためスキップ");
    };
    let Some(tag) = find_tag(catalog, EXT_PLC_D3000) else {
        println!("  失敗: {EXT_PLC_D3000} が catalog に見つかりません。");
        return StepResult::from_bool("503確認", false, "plc.gs.d3000がcatalogに存在しない");
    };
    let http = match reqwest::ClientBuilder::new().no_proxy().build() {
        Ok(http) => http,
        Err(error) => {
            println!("  reqwestクライアント構築失敗: {error}");
            let detail = format!("reqwestクライアント構築失敗: {error}");
            return StepResult::from_bool("503確認", false, detail);
        }
    };

    println!("  write-control を無効化します (POST /api/write-control/disable) ...");
    let disable_result = set_write_control(&http, hub_url, false, admin_token).await;

    match disable_result {
        Ok(()) => {
            println!("  無効化: 成功");
            let write_client = build_client(hub_url, api_key);
            let (write_ok, write_detail) = match write_client {
                Ok(client) => match client.write_tag(tag.ids, RequestedValue::Num(1.0)).await {
                    Ok(()) => {
                        println!("  想定外: write-control無効時に書込が成功してしまいました。");
                        (false, "書込が成功してしまった（本来503のはず）".to_owned())
                    }
                    Err(error) => {
                        let matched = error.kind() == ErrorKind::WriteUnavailable;
                        println!(
                            "  結果: kind={} (期待=write_unavailable) 一致={matched}",
                            error.kind().as_str()
                        );
                        (matched, format!("kind={}", error.kind().as_str()))
                    }
                },
                Err(error) => {
                    println!("  クライアント構築失敗: kind={}", error.kind().as_str());
                    (
                        false,
                        format!("client build error: {}", error.kind().as_str()),
                    )
                }
            };

            // disable に成功した＝実際に write-control を無効化してしまった
            // 場合のみ、Hub の状態を書き換えた責任として必ず enable へ戻す
            // 復旧処理を行う。disable 自体が失敗した場合は何も変更していない
            // ため、この節には入らない（下の Err 枝を参照）。
            println!("  write-control を有効化に戻します (POST /api/write-control/enable) ...");
            let enable_result = set_write_control(&http, hub_url, true, admin_token).await;
            match &enable_result {
                Ok(()) => println!("  有効化: 成功"),
                Err(message) => {
                    println!(
                        "  ★★★ 有効化に失敗しました: {message} ★★★ 手動で /api/write-control/enable を呼んで復旧してください。"
                    );
                }
            }

            // 復旧確認: 実際に書き込めることを確かめる（あくまで付随情報）。
            let restored = match build_client(hub_url, api_key) {
                Ok(client) => client
                    .write_tag(tag.ids, RequestedValue::Num(0.0))
                    .await
                    .is_ok(),
                Err(_) => false,
            };
            println!(
                "  復旧確認 (書込テスト): {}",
                if restored {
                    "成功（write-controlは復旧済み）"
                } else {
                    "失敗（write-controlが復旧していない可能性）"
                }
            );

            let detail = format!(
                "{write_detail}, enable呼び出し={}, 復旧確認={restored}",
                enable_result.is_ok()
            );
            StepResult::from_bool("503確認", write_ok, detail)
        }
        Err(message) => {
            println!("  無効化失敗: {message}（この場合503確認自体は検証できません）");
            // disable 自体が失敗した＝write-control は何も変更されていない
            // ため、enable への復旧は不要（むしろ「手動で復旧してください」と
            // 表示すると、実際には無効化されていないのに利用者を無用に
            // 不安にさせる）。よってここでは enable を呼ばず、その旨のみ示す。
            println!("  無効化に失敗したため write-control は変更されていません（復旧不要）。");
            let detail = format!("disable失敗: {message}（write-control未変更のため復旧不要）");
            StepResult::from_bool("503確認", false, detail)
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("=== banto-tagclient 実 Hub 検証ツール ===");
    let hub_url = env::var("HUB_URL").unwrap_or_else(|_| DEFAULT_HUB_URL.to_owned());
    let api_key = match env::var("HUB_API_KEY") {
        Ok(value) => value,
        Err(_) => {
            eprintln!(
                "環境変数 HUB_API_KEY が未設定です。banto-hub の POST /api/api-keys で発行したキーを設定してください。"
            );
            std::process::exit(1);
        }
    };
    // ロックダウン済みの Hub（本番の実態）で項目6の write-control 切り替えに
    // 管理セッションの認証が要る場合のみ設定する。試運転モードでは不要。
    let admin_token = env::var("HUB_ADMIN_TOKEN").ok();
    println!("Hub URL: {hub_url}\n");

    let mut results = Vec::new();

    let (result1, catalog) = step1_catalog(&hub_url, &api_key).await;
    results.push(result1);
    println!();

    results.push(step2_read(&hub_url, &api_key).await);
    println!();

    results.push(step3_subscribe(&hub_url, &api_key, catalog.as_ref()).await);
    println!();

    results.push(step4_write(&hub_url, &api_key, catalog.as_ref()).await);
    println!();

    results.push(step5_forbidden(&hub_url, &api_key, catalog.as_ref()).await);
    println!();

    results.push(
        step6_unavailable(&hub_url, &api_key, catalog.as_ref(), admin_token.as_deref()).await,
    );
    println!();

    println!("=== 検証結果サマリ ===");
    for result in &results {
        println!(
            "[{}] {} - {}",
            result.status.mark(),
            result.name,
            result.detail
        );
    }
    // スキップも「検証できていない」という点では成功ではないため、
    // 終了コードの判定では失敗と同様に非成功として扱う（表示のみ区別する）。
    let all_pass = results.iter().all(|result| result.status.is_pass());
    println!();
    println!(
        "総合結果: {}",
        if all_pass {
            "全項目成功"
        } else {
            "一部項目が失敗・想定外、またはスキップ"
        }
    );
    if !all_pass {
        std::process::exit(1);
    }
}
