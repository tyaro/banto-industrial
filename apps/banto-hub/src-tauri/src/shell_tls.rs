//! Desktop origin is process-owned, exclusive, ephemeral and pinned in memory.
//! The static owner intentionally never drops: switching/stopping Hub must not
//! release a port which WebView2 may still display or hold a capability for.
use banto_hub_core::shell_transport::ShellTransport;
use std::sync::{Arc, OnceLock};
use tauri::{Manager, Url, WebviewWindow};

static TRANSPORT: OnceLock<Initialization<Arc<ShellTransport>>> = OnceLock::new();

/// Retain the bound origin even when registration fails (including a partially
/// applied capability), but expose it only after registration succeeds.
struct Initialization<T> {
    retained: Option<T>,
    authorization: Result<(), String>,
}

impl<T> Initialization<T> {
    fn new(bind: Result<T, String>, register: impl FnOnce(&T) -> Result<(), String>) -> Self {
        match bind {
            Ok(transport) => Self {
                authorization: register(&transport),
                retained: Some(transport),
            },
            Err(error) => Self {
                retained: None,
                authorization: Err(error),
            },
        }
    }

    fn usable(&self) -> Result<&T, String> {
        self.authorization.as_ref().map_err(Clone::clone)?;
        self.retained
            .as_ref()
            .ok_or_else(|| "シェルの待ち受けがありません".into())
    }
}
pub(crate) const SERVE_FAILURE: &str = "管理画面の待ち受けが異常終了しました。ポートはこのプロセスが保持しており、別のアプリへは接続しません。収集状態を確認してから、Hub を終了し起動し直してください";

pub(crate) fn transport() -> Result<&'static Arc<ShellTransport>, String> {
    TRANSPORT
        .get()
        .ok_or_else(|| "シェルの HTTPS は準備されていません".to_string())?
        .usable()
}

pub(crate) fn initialize(app: &tauri::AppHandle) -> Result<(), String> {
    // Registration and retention happen once together. Repeated setup must not
    // authorize another origin and then release that origin's reserved socket.
    TRANSPORT
        .get_or_init(|| {
            Initialization::new(
                tauri::async_runtime::block_on(ShellTransport::start()),
                |transport| {
                    app.add_capability(capability(&transport.url()))
                        .map_err(|e| format!("シェルの origin の権限を登録できません: {e}"))
                },
            )
        })
        .usable()
        .map(|_| ())
}

fn capability(origin: &str) -> tauri::ipc::CapabilityBuilder {
    // The management UI uses @tauri-apps/api/core invoke for the app commands
    // below and @tauri-apps/api/event listen (whose returned disposer unlistens).
    // No window/webview/app/plugin core APIs are used, so core:default is unnecessary.
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
    // Custom Tauri schemes have opaque Url origins, even for the same host.
    url.scheme() == origin.scheme()
        && url.host_str() == origin.host_str()
        && url.port_or_known_default() == origin.port_or_known_default()
        && url.username().is_empty()
        && url.password().is_none()
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
    fn custom_scheme_placeholder_requires_the_exact_host() {
        let local = Url::parse("tauri://localhost/").unwrap();
        assert!(is_origin(
            &Url::parse("tauri://localhost/status").unwrap(),
            &local
        ));
        for url in [
            "tauri://evil/",
            "tauri://localhost:1234/",
            "https://localhost/",
            "tauri://user@localhost/",
        ] {
            assert!(!is_origin(&Url::parse(url).unwrap(), &local), "{url}");
        }
    }
    #[test]
    fn failed_bind_skips_capability_and_preserves_diagnostic_reason() {
        let state =
            Initialization::<()>::new(Err("injected certificate/bind failure".into()), |_| {
                panic!("must not authorize an unbound origin")
            });
        assert!(state.retained.is_none());
        assert_eq!(
            state.usable().unwrap_err(),
            "injected certificate/bind failure"
        );
    }

    #[test]
    fn failed_registration_retains_bound_socket_but_does_not_expose_desktop_origin() {
        let socket = Arc::new(());
        let state = Initialization::new(Ok(socket.clone()), |_| {
            Err("injected partial registration failure".into())
        });
        assert_eq!(
            Arc::strong_count(&socket),
            2,
            "the reserved socket must remain owned"
        );
        assert_eq!(
            state.usable().unwrap_err(),
            "injected partial registration failure"
        );
    }

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
            let permissions = serde_json::to_value(value.permissions).unwrap();
            let core: Vec<_> = permissions
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|permission| permission.as_str())
                .filter(|permission| permission.starts_with("core:"))
                .collect();
            assert_eq!(
                core,
                ["core:event:allow-listen", "core:event:allow-unlisten"],
                "UI invokes app commands and only uses event.listen/disposer among core APIs"
            );
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
