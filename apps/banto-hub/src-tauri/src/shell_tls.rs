//! Desktop origin is process-owned, exclusive, ephemeral and pinned in memory.
//! The static owner intentionally never drops: switching/stopping Hub must not
//! release a port which WebView2 may still display or hold a capability for.
use banto_hub_core::shell_transport::ShellTransport;
use std::sync::{Arc, OnceLock};
use tauri::{Manager, Url, WebviewWindow};

static TRANSPORT: OnceLock<Result<Arc<ShellTransport>, String>> = OnceLock::new();
pub(crate) const SERVE_FAILURE: &str = "管理画面の待ち受けが異常終了しました。ポートはこのプロセスが保持しており、別のアプリへは接続しません。収集状態を確認してから、Hub を終了し起動し直してください";

pub(crate) fn transport() -> Result<&'static Arc<ShellTransport>, String> {
    TRANSPORT
        .get()
        .ok_or_else(|| "シェルの HTTPS は準備されていません".to_string())?
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) fn initialize(app: &tauri::AppHandle) -> Result<(), String> {
    let value = tauri::async_runtime::block_on(ShellTransport::start());
    // Retain even if capability registration fails; never leave an authorized port free.
    TRANSPORT
        .set(value)
        .map_err(|_| "シェルの HTTPS は既に初期化されています".to_string())?;
    let transport = transport()?;
    app.add_capability(capability(&transport.url()))
        .map_err(|e| format!("シェルの origin の権限を登録できません: {e}"))
}

fn capability(origin: &str) -> tauri::ipc::CapabilityBuilder {
    let mut capability = tauri::ipc::CapabilityBuilder::new("desktop-shell")
        .local(false)
        .window(crate::MAIN_WINDOW_LABEL)
        .remote(format!("{origin}*"));
    for permission in [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "allow-host-switch-status",
        "allow-switch-to-service",
        "allow-switch-to-desktop",
        "allow-set-service-autostart",
        "allow-restore-setting-mode",
        "allow-retry-startup",
        "allow-sink-service-status",
        "allow-sink-service-start",
        "allow-sink-service-stop",
    ] {
        capability = capability.permission(permission);
    }
    capability
}

pub(crate) fn is_origin(url: &Url, origin: &Url) -> bool {
    url.origin() == origin.origin() && url.username().is_empty() && url.password().is_none()
}

pub(crate) fn desktop_origin(url: &Url) -> bool {
    transport()
        .ok()
        .and_then(|t| Url::parse(&t.url()).ok())
        .is_some_and(|origin| is_origin(url, &origin))
}

pub(crate) fn navigation_allowed(app: &tauri::AppHandle, url: &Url) -> bool {
    let state = app.state::<crate::AppState>();
    if state
        .placeholder_url
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|local| is_origin(url, local))
    {
        return true;
    }
    let allowed = match &*state.view.lock().unwrap() {
        crate::ShellView::Desktop => desktop_origin(url),
        crate::ShellView::Service { .. } => is_origin(
            url,
            &Url::parse(&crate::service_tls::service_url()).unwrap(),
        ),
        crate::ShellView::Fallback(_) => false,
    };
    allowed
}

pub(crate) fn require_command_origin(window: &WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(|e| e.to_string())?;
    if window.label() == crate::MAIN_WINDOW_LABEL && navigation_allowed(window.app_handle(), &url) {
        Ok(())
    } else {
        Err("この画面からシェルの操作はできません".into())
    }
}

pub(crate) fn read_pin(request_uri: &str) -> Result<Vec<u8>, String> {
    let mut url = Url::parse(request_uri).map_err(|e| e.to_string())?;
    if url.scheme() == "wss" {
        let _ = url.set_scheme("https");
    }
    if !desktop_origin(&url) {
        return Err("シェルの HTTPS origin と一致しません".into());
    }
    Ok(transport()?.certificate_pem().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_authorize_exact_port_and_have_no_static_http_alternative() {
        use tauri::ipc::RuntimeCapability;
        let static_capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert_eq!(
            static_capability["remote"]["urls"],
            serde_json::json!(["https://127.0.0.1:8723/*"])
        );
        for origin in ["https://127.0.0.1:49152/", "http://127.0.0.1:49152/"] {
            let tauri::utils::acl::capability::CapabilityFile::Capability(value) =
                capability(origin).build()
            else {
                panic!("single capability expected")
            };
            assert_eq!(value.remote.unwrap().urls, vec![format!("{origin}*")]);
            assert!(!value.local);
            assert_eq!(value.windows, vec![crate::MAIN_WINDOW_LABEL]);
        }
    }
    #[test]
    fn origin_comparison_rejects_ports_schemes_hosts_and_credentials() {
        let origin = Url::parse("https://127.0.0.1:49152/").unwrap();
        assert!(is_origin(
            &Url::parse("https://127.0.0.1:49152/status?x=1").unwrap(),
            &origin
        ));
        for url in [
            "http://127.0.0.1:49152/",
            "https://127.0.0.1:49153/",
            "https://localhost:49152/",
            "https://user@127.0.0.1:49152/",
        ] {
            assert!(!is_origin(&Url::parse(url).unwrap(), &origin), "{url}");
        }
    }
}
