//! Disposable GitHub-hosted Windows runner only. Never run locally.
//! No service is started; run-service is deliberately unsupported.

#[cfg(not(windows))]
fn main() {
    eprintln!("delete_pending_probe requires a disposable GitHub-hosted Windows runner");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run() {
        eprintln!("probe-error: {error}");
        std::process::exit(2);
    }
}

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use banto_service_control::{
        install_service, uninstall_service, ServiceSpec, ServiceStartKind,
    };

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> i32;
    }

    fn env(name: &str) -> Result<String, String> {
        std::env::var(name).map_err(|_| format!("missing {name}"))
    }

    fn within(path: &Path, root: &Path) -> Result<PathBuf, String> {
        let resolved = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        if resolved == root || !resolved.starts_with(root) || !resolved.is_file() {
            return Err("executable outside disposable root".into());
        }
        Ok(resolved)
    }

    fn guard() -> Result<PathBuf, String> {
        for (key, value) in [
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("RUNNER_OS", "Windows"),
            ("BANTO_973_SCM_TEST", "github-hosted-disposable-only-v1"),
        ] {
            if env(key)? != value {
                return Err(format!("refused environment: {key}"));
            }
        }
        // SAFETY: read-only check of this process's token; no elevation is requested.
        if unsafe { IsUserAnAdmin() } == 0 {
            return Err("administrator token required".into());
        }
        let temp = std::fs::canonicalize(env("RUNNER_TEMP")?).map_err(|e| e.to_string())?;
        let raw = PathBuf::from(env("BANTO_973_SCM_ROOT")?);
        let root = std::fs::canonicalize(&raw).map_err(|e| e.to_string())?;
        if root == temp || !root.starts_with(&temp) || !root.is_dir() {
            return Err("invalid disposable root".into());
        }
        use std::os::windows::fs::MetadataExt;
        let mut ancestor = raw.as_path();
        loop {
            if std::fs::symlink_metadata(ancestor)
                .map_err(|e| e.to_string())?
                .file_attributes()
                & 0x400
                != 0
            {
                return Err("reparse point in disposable root".into());
            }
            if std::fs::canonicalize(ancestor).map_err(|e| e.to_string())? == temp {
                break;
            }
            ancestor = ancestor.parent().ok_or("root ancestor missing")?;
        }
        let marker = format!(
            "{}\n{}\n",
            env("GITHUB_RUN_ID")?,
            env("BANTO_973_EXPECTED_SHA")?
        );
        if std::fs::read_to_string(root.join(".banto-973-ci")).map_err(|e| e.to_string())? != marker
        {
            return Err("disposable marker mismatch".into());
        }
        within(&std::env::current_exe().map_err(|e| e.to_string())?, &root)?;
        Ok(root)
    }

    pub fn run() -> Result<(), String> {
        let root = guard()?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let stem = me.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let (action, name, executable) = match args.as_slice() {
            [action] if action == "service-uninstall" && stem == "chronogazer-elev" => {
                ("uninstall", "ChronoGazer", None)
            }
            [action] if action == "uninstall" && stem == "chronogazer-server" => {
                ("uninstall", "ChronoGazer", None)
            }
            [action, name, exe] if action == "install" => {
                ("install", name.as_str(), Some(PathBuf::from(exe)))
            }
            [action, name] if action == "uninstall" => ("uninstall", name.as_str(), None),
            _ => return Err("unsupported action (run-service/start are forbidden)".into()),
        };
        if name != "ChronoGazer"
            && !(name.starts_with("Banto973-")
                && name.len() < 100
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        {
            return Err("service name outside test namespace".into());
        }
        // The orchestrator records an absent-name ownership intent before the first install.
        let owned =
            std::fs::read_to_string(root.join("owned-services.txt")).map_err(|e| e.to_string())?;
        if !owned.lines().any(|line| line == name) {
            return Err("service not in this run's ownership ledger".into());
        }
        if action == "install" {
            let executable = within(&executable.ok_or("missing executable")?, &root)?;
            let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
            let result = install_service(&ServiceSpec {
                name,
                display_name: name,
                description: Some("Disposable #973 stopped-service test; never start"),
                executable,
                launch_arguments: vec!["run-service".into()],
                start_kind: ServiceStartKind::OnDemand,
                preshutdown_timeout: Duration::from_secs(30),
            });
            println!("install-result={result:?}");
            if result.is_err() {
                std::process::exit(1);
            }
        } else {
            let result = uninstall_service(name);
            println!("uninstall-result={result:?}");
            if result.is_err() {
                std::process::exit(1);
            }
        }
        Ok(())
    }
}
