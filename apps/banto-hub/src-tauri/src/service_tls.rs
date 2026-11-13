//! #644 PR4（2026-11-14 オーナー決定）: Service モードの webview と health probe を、Hub の TLS の口
//! （`https://127.0.0.1:8723/`）と `%ProgramData%\BantoHub\tls\` の証明書の pin に縛る。
//!
//! # 何を塞ぐか
//!
//! 以前の Service モードは `http://127.0.0.1:8722` の openapi に profile-id があるかだけで本物の Hub
//! と見なして webview を移していた。profile-id は偽装できるので、Hub が止まっている間に 8722 を
//! 先取りしたプロセスの偽の画面へパスワード・セッショントークンが渡り、`capabilities/default.json`
//! の `remote.urls`（`http://127.0.0.1:*/*`）によって偽の画面からシェルの invoke
//! （`switch_to_service` 等）も呼べた。
//!
//! # 二重の pin
//!
//! - **probe**（[`service_probe`]）: Service へ進むか（起動・再試行・切り替え・見張り）は
//!   `banto_hub_core::pinned_hub_health::PinnedHubHealthProbe` が決める。8723 の相手が pin と完全に
//!   一致する証明書で TLS を話し、openapi の profile-id が合うときだけ `Healthy`。
//! - **webview**（[`install_certificate_pin`]、Windows）: WebView2 は Hub の自己署名の証明書を
//!   信頼しないので、8723 の資源ごとに `ServerCertificateErrorDetected` が来る。ハンドラ
//!   （[`certificate_error_action`]）は、要求の URI が `https://127.0.0.1:8723/…`・エラーの種類が
//!   「信頼できない証明書」（`CERTIFICATE_IS_INVALID`）・相手の証明書がその場で読んだ pin と DER で
//!   完全一致、のすべてを満たすときだけ `ALWAYS_ALLOW`、それ以外（読めない・COM の失敗を含む）は
//!   すべて `CANCEL`。WebView2 は `ALWAYS_ALLOW` を「RequestUri のホスト＋サーバー証明書」の組で
//!   セッションの間覚えるので、先取りしたプロセスが別の証明書を出せばまたイベントが来て断る。
//!   `--renew` の後の新しい証明書も、その時点の `tls\` を読み直して許す（再起動は要らない）。
//!   イベントは WebSocket を含むすべての web 資源で来る。probe が `Healthy` と言った後に相手が
//!   入れ替わっても（TOCTOU）、webview の側で断る。
//!
//! Windows が信頼する証明書（Windows の root に入った CA が出したもの）にはイベントが来ないので、
//! webview の pin はそれを縛れない。LocalMachine の root に足せるのは管理者、CurrentUser の root に
//! 足せるのはその利用者本人だけで、どちらも #644 の相手（ポートを先取りする別のプロセス）の外。
//! probe（rustls）は Windows の root を使わないので、その場合も pin と一致しなければ Service へ
//! 進まない。
//!
//! # TLS なしの Hub には繋がない
//!
//! Hub のサービスは `tls\` が無い・壊れ・露出・8723 が埋まっているとき TLS なしで起動する（PR1b の
//! 決定 A・B）。そのときシェルは平文の 8722 へ navigate せず、Fallback の窓に理由と対処
//! （[`TLS_REMEDY`]、`tray_status::describe_health_outcome`）を出す。
//!
//! Desktop モード（このプロセスが握る自分の Hub）は今の main では平文のまま（#1121 で TLS 化）。

use std::path::PathBuf;

use banto_hub_core::pinned_hub_health::{HubCertPemSource, PinnedHubHealthProbe};

/// Hub の TLS の口のホスト。Hub は 127.0.0.1 だけで TLS を待ち受け、`BANTO_BIND` には追従しない
/// （`banto_hub_core::pinned_hub_health::HUB_TLS_HOST` と同じ値）。
pub(crate) const SERVICE_TLS_HOST: &str = "127.0.0.1";

/// Hub の TLS の口のポート。Hub のサービスの既定（`banto_hub_tls_trust::DEFAULT_TLS_PORT`）を
/// 共有する - ここだけで決めない。`PORT` 環境変数は平文の口だけを変える。
pub(crate) const SERVICE_TLS_PORT: u16 = banto_hub_tls_trust::DEFAULT_TLS_PORT;

/// TLS が使えないときの対処（Fallback の窓に出す。ChronoGazer の `hub_tls` の案内と揃える）。
pub(crate) const TLS_REMEDY: &str = "Hub の TLS が使えないので接続しません。管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行して、Hub のサービスを再起動してください";

/// Service モードの navigate 先（`https://127.0.0.1:8723/`）。
pub(crate) fn service_url() -> String {
    format!("https://{SERVICE_TLS_HOST}:{SERVICE_TLS_PORT}/")
}

/// Service へ進むかを決める probe（モジュール doc「二重の pin」）。`plain_host` は
/// [`crate::resolve_navigate_host`] の値で、平文の口が消えたか（Service→Desktop の待ち）にだけ使う。
pub(crate) fn service_probe(root: PathBuf, plain_host: String) -> PinnedHubHealthProbe {
    let cert: HubCertPemSource = Box::new(read_hub_cert_pem);
    PinnedHubHealthProbe::new(root, plain_host, SERVICE_TLS_PORT, cert)
}

/// pin にする Hub の証明書（PEM）を、置き場を確かめてから読む（読み取り専用。一般利用者でも
/// 動く）。`Err` は表示用の理由と対処。
#[cfg(windows)]
pub(crate) fn read_hub_cert_pem() -> Result<Vec<u8>, String> {
    banto_hub_tls_trust::load_pinned_hub_cert(&banto_hub_tls_trust::win::WindowsTlsTrustOps)
        .map(|pin| pin.cert_pem)
        .map_err(|found| format!("{}。{}", found.reason, remedy_text(&found.remedy)))
}

/// 非 Windows には Hub のサービスが無い（証明書の置き場も無い）。
#[cfg(not(windows))]
pub(crate) fn read_hub_cert_pem() -> Result<Vec<u8>, String> {
    Err("この OS には Hub のサービスの証明書の置き場がありません（Hub のサービスは Windows 専用です）".to_string())
}

/// 置き場の確かめの失敗の対処（文言は ChronoGazer の `hub_tls::remedy_text` と揃える。片方を
/// 直したらもう一方も。シェルも毎回読み直すので、pin を書き換える手順は要らない）。
#[cfg_attr(not(windows), allow(dead_code))]
fn remedy_text(remedy: &banto_hub_tls_trust::Remedy) -> String {
    use banto_hub_tls_trust::Remedy;
    const PREPARE: &str = "banto-hub-elev.exe prepare-hub-tls";
    match remedy {
        Remedy::Retry => "原因を取り除いてからやり直してください。続くときは、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行して置き場を確かめてください".to_owned(),
        Remedy::Prepare => format!(
            "Hub をサービスで動かし、管理者の PowerShell で {PREPARE} を実行してから、Hub のサービスを再起動してください"
        ),
        Remedy::FixRoot => "Hub の置き場の所有者を Administrators にし、SYSTEM・Administrators 以外への書き込み・削除・ACL と所有者の変更の許可を外してください".to_owned(),
        Remedy::RecreateDir(tls_path) => format!(
            "{} の中身を確かめてこのフォルダを消し、管理者の PowerShell で {PREPARE} を実行してから、Hub のサービスを再起動してください",
            tls_path.display()
        ),
        Remedy::Renew => format!(
            "管理者の PowerShell で {PREPARE} --renew を実行して作り直し、Hub のサービスを再起動してください"
        ),
    }
}

/// `ServerCertificateErrorDetected` の判断（純関数。モジュール doc「二重の pin」）。`Ok(())` なら
/// 許す（`ALWAYS_ALLOW`）、`Err(理由)` なら断る（`CANCEL`、理由はログにだけ出す）。
///
/// 確かめる順: URI → エラーの種類 → 相手の証明書 → pin（pin はここまで通ったときだけ読む）。
/// - `request_uri`: `https`（WebSocket は `wss`）・ホスト `127.0.0.1`・ポート [`SERVICE_TLS_PORT`] だけ。
/// - `untrusted_authority`: エラーの種類が「信頼できない証明書」（`CERTIFICATE_IS_INVALID`）か。
///   名前の不一致・期限切れ・失効などは、pin と一致しても断る（probe の rustls も断る）。
/// - `server_pem`: 相手の証明書（`ToPemEncoding`。取れなければ `Err`）。
/// - `read_pin`: その場で `tls\` を確かめて読む（[`read_hub_cert_pem`]）。
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn certificate_error_action(
    request_uri: &str,
    untrusted_authority: bool,
    server_pem: Result<String, String>,
    read_pin: impl FnOnce() -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let uri =
        tauri::Url::parse(request_uri).map_err(|_| "要求の URI を解釈できません".to_string())?;
    // WebSocket（`wss`）も同じ口・同じ pin（イベントは WebSocket を含むすべての資源で来る）。
    let is_hub_tls = matches!(uri.scheme(), "https" | "wss")
        && uri.host_str() == Some(SERVICE_TLS_HOST)
        && uri.port_or_known_default() == Some(SERVICE_TLS_PORT);
    if !is_hub_tls {
        return Err(format!(
            "Hub の TLS の口（https://{SERVICE_TLS_HOST}:{SERVICE_TLS_PORT}）ではない相手の証明書の誤りです（{}://{}）",
            uri.scheme(),
            uri.host_str().unwrap_or("")
        ));
    }
    if !untrusted_authority {
        return Err(
            "想定外の種類の証明書の誤りです（信頼できない証明書だけを pin で確かめます）"
                .to_string(),
        );
    }
    let server_pem = server_pem?;
    let server_der = banto_hub_tls_trust::parse_single_certificate(server_pem.as_bytes())
        .map_err(|why| format!("相手の証明書を読めません: {why}"))?;
    let pin_pem = read_pin()?;
    let pin_der = banto_hub_tls_trust::parse_single_certificate(&pin_pem)
        .map_err(|why| format!("Hub の証明書（pin）を読めません: {why}"))?;
    if server_der != pin_der {
        return Err("相手の証明書が Hub の証明書（pin）と一致しません（別のプロセスがポートを先に取っている恐れがあります）".to_string());
    }
    Ok(())
}

/// メインの窓の WebView2 に `ServerCertificateErrorDetected` のハンドラを 1 回だけ付ける（窓を
/// 作った直後の `setup` から呼ぶ。Desktop モードの平文の http では来ないので害は無い）。付けられ
/// なければログに残すだけ - そのときは WebView2 の既定（証明書の誤りのページ）になり、Service の
/// 画面は開かない（安全側）。
#[cfg(windows)]
pub(crate) fn install_certificate_pin(window: &tauri::WebviewWindow) {
    use banto_hub_core::hub_log::log_err_line;
    let result = window.with_webview(|webview| {
        // SAFETY: WebView2 の COM の呼び出し。`with_webview` は窓の UI スレッドで呼ぶ。
        if let Err(err) = unsafe { register_handler(&webview) } {
            log_err_line(&format!(
                "banto-hub-shell: Hub の証明書の pin を webview に付けられませんでした（Service の画面は開けません）: {err}"
            ));
        }
    });
    if let Err(err) = result {
        log_err_line(&format!(
            "banto-hub-shell: Hub の証明書の pin を webview に付けられませんでした（Service の画面は開けません）: {err}"
        ));
    }
}

/// ハンドラの登録（COM）。トークンは捨てる（窓と同じ寿命で、外さない）。
#[cfg(windows)]
unsafe fn register_handler(webview: &tauri::webview::PlatformWebview) -> windows_core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_14;
    use webview2_com::ServerCertificateErrorDetectedEventHandler;
    use windows_core::Interface;

    let core = unsafe { webview.controller().CoreWebView2() }?;
    let core: ICoreWebView2_14 = core.cast()?;
    let handler = ServerCertificateErrorDetectedEventHandler::create(Box::new(|_sender, args| {
        if let Some(args) = args {
            // SAFETY: WebView2 がこのイベントの間だけ有効な引数を渡す（UI スレッド）。
            unsafe { decide_and_set(&args) };
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { core.add_ServerCertificateErrorDetected(&handler, &mut token) }
}

/// イベントの引数から [`certificate_error_action`] を通して `SetAction` する。COM の呼び出しの
/// 失敗はすべて断る側に倒す（`SetAction(CANCEL)` も失敗すれば WebView2 の既定 = 誤りのページで、
/// 許すことはない）。
#[cfg(windows)]
unsafe fn decide_and_set(
    args: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ServerCertificateErrorDetectedEventArgs,
) {
    use banto_hub_core::hub_log::{log_err_line, log_line};
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW,
        COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL, COREWEBVIEW2_WEB_ERROR_STATUS,
        COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID,
    };
    use windows_core::PWSTR;

    let mut uri = PWSTR::null();
    let request_uri = match unsafe { args.RequestUri(&mut uri) } {
        Ok(()) => webview2_com::take_pwstr(uri),
        Err(err) => {
            log_err_line(&format!(
                "banto-hub-shell: 証明書の誤りの URI を取れないので断ります: {err}"
            ));
            let _ = unsafe { args.SetAction(COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL) };
            return;
        }
    };
    let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
    let untrusted_authority = unsafe { args.ErrorStatus(&mut status) }.is_ok()
        && status == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID;
    let server_pem = unsafe { args.ServerCertificate() }
        .and_then(|cert| {
            let mut pem = PWSTR::null();
            unsafe { cert.ToPemEncoding(&mut pem) }.map(|()| webview2_com::take_pwstr(pem))
        })
        .map_err(|err| format!("相手の証明書を取れません: {err}"));
    let decision = certificate_error_action(
        &request_uri,
        untrusted_authority,
        server_pem,
        read_hub_cert_pem,
    );
    let action = match decision {
        Ok(()) => {
            log_line(&format!(
                "banto-hub-shell: Hub の証明書（pin と一致）を許しました: {request_uri}"
            ));
            COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW
        }
        Err(reason) => {
            log_err_line(&format!(
                "banto-hub-shell: 証明書の誤りを断りました（{request_uri}、種類 {}）: {reason}",
                status.0
            ));
            COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL
        }
    };
    if let Err(err) = unsafe { args.SetAction(action) } {
        log_err_line(&format!(
            "banto-hub-shell: 証明書の誤りの扱いを WebView2 に渡せませんでした（既定＝誤りのページになります）: {err}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUB_URI: &str = "https://127.0.0.1:8723/api/v1/openapi.json";

    /// 証明書を 2 枚（本物・先取り）作る。
    fn two_certs() -> (String, String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let a = banto_hub_core::hub_tls_prepare::generate_identity(now).unwrap();
        let b = banto_hub_core::hub_tls_prepare::generate_identity(now).unwrap();
        (a.cert_pem, b.cert_pem)
    }

    fn pin(pem: &str) -> impl FnOnce() -> Result<Vec<u8>, String> + '_ {
        move || Ok(pem.as_bytes().to_vec())
    }

    #[test]
    fn allows_only_the_pinned_certificate_on_the_hub_tls_port() {
        let (hub, _) = two_certs();
        assert_eq!(
            certificate_error_action(HUB_URI, true, Ok(hub.clone()), pin(&hub)),
            Ok(())
        );
        // WebSocket も同じ口・同じ pin なら許す。
        assert_eq!(
            certificate_error_action(
                "wss://127.0.0.1:8723/api/v1/stream",
                true,
                Ok(hub.clone()),
                pin(&hub)
            ),
            Ok(())
        );
    }

    #[test]
    fn cancels_a_different_certificate() {
        let (hub, squatter) = two_certs();
        assert!(certificate_error_action(HUB_URI, true, Ok(squatter), pin(&hub)).is_err());
    }

    #[test]
    fn cancels_when_the_pin_cannot_be_read_or_the_server_certificate_is_missing() {
        let (hub, _) = two_certs();
        assert!(
            certificate_error_action(HUB_URI, true, Ok(hub.clone()), || Err(
                "置き場がありません".to_string()
            ))
            .is_err()
        );
        assert!(
            certificate_error_action(HUB_URI, true, Err("取れません".to_string()), pin(&hub))
                .is_err()
        );
        assert!(
            certificate_error_action(HUB_URI, true, Ok("not pem".to_string()), pin(&hub)).is_err()
        );
    }

    #[test]
    fn cancels_other_hosts_ports_schemes_and_error_kinds_without_reading_the_pin() {
        let (hub, _) = two_certs();
        let never = || -> Result<Vec<u8>, String> { panic!("pin を読んではいけない") };
        for uri in [
            "https://127.0.0.1:8722/",
            "https://localhost:8723/",
            "https://192.168.0.10:8723/",
            "https://127.0.0.2:8723/",
            "http://127.0.0.1:8723/",
            "ws://127.0.0.1:8723/",
            "not a uri",
        ] {
            assert!(
                certificate_error_action(uri, true, Ok(hub.clone()), never).is_err(),
                "{uri}"
            );
        }
        // 名前の不一致・期限切れ等は、同じ証明書でも断る。
        assert!(certificate_error_action(HUB_URI, false, Ok(hub.clone()), never).is_err());
    }

    #[test]
    fn the_service_url_is_the_hub_tls_port() {
        assert_eq!(service_url(), "https://127.0.0.1:8723/");
        assert_eq!(SERVICE_TLS_PORT, 8723);
    }
}
