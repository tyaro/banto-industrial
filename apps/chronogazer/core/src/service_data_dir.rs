//! Service-only `data.dir` policy (#659, stage A).
//!
//! `run-service` has already opened the data root with `Mode::Guarded` and
//! closed its tree before the engine reads this setting. Existing components
//! are checked relative to that held handle without following reparse points.
//! The `Mode::Plain` branch exists for the service state-machine's FakeAcl
//! tests. It is not a security boundary and is never used by RealAcl.
//!
//! Missing children remain subject to the root's operation-mode ACL. The
//! collector still opens its path later; this does not solve races involving
//! old write-capable handles or SQLite sidecars.

use std::path::{Component, Path, PathBuf};

use banto_safe_root::guard::{is_plain_child_name, is_same_or_inside, paths_match};
use banto_safe_root::SafeRoot;

/// Resolve a service setting to a child of the held root without changing the
/// filesystem. A rejected value cannot reach the writable probe or collector.
pub(crate) fn validate_service_data_dir(
    root: &SafeRoot,
    configured: &str,
) -> Result<PathBuf, String> {
    let value = configured.trim();
    let value = if value.is_empty() { "./data" } else { value };
    let input = Path::new(value);

    let relative = if input.is_absolute() {
        // Compare whole components, not a string prefix. The second spelling
        // accepts a path copied from GetFinalPathNameByHandleW (\\?\C:\...).
        let base = [root.path(), root.final_path()]
            .into_iter()
            .find(|base| is_same_or_inside(input, base))
            .ok_or_else(|| refusal(value))?;
        input
            .components()
            .skip(base.components().count())
            .map(|component| component.as_os_str())
            .collect::<PathBuf>()
    } else {
        input.to_path_buf()
    };

    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| refusal(value))?;
                if !is_plain_child_name(name) {
                    return Err(refusal(value));
                }
                components.push(name);
            }
            _ => return Err(refusal(value)),
        }
    }
    if components.is_empty() {
        // The root itself also contains DB, logs and secrets; require a child.
        return Err(refusal(value));
    }

    let mut relative = PathBuf::new();
    for component in &components {
        relative.push(component);
    }
    let resolved = root.final_path().join(&relative);

    #[cfg(windows)]
    if let Some(held_root) = root.pinned() {
        let mut opened = None;
        for name in &components {
            let parent = opened.as_ref().unwrap_or(held_root);
            let expected = parent.final_path().join(name);
            let child = parent
                .open_child_dir(name)
                .map_err(|error| format!("data.dir の要素を確認できません: {error}"))?;
            let Some(child) = child else { break };
            if !child.info().is_dir
                || child.info().is_reparse_point
                || !paths_match(child.final_path(), &expected)
                || !is_same_or_inside(child.final_path(), root.final_path())
            {
                return Err(refusal(value));
            }
            opened = Some(child);
        }
        return Ok(resolved);
    }

    // FakeAcl uses Mode::Plain in in-process service simulations. This branch
    // checks ordinary links for regressions, but must never authorize a real
    // Windows service: only the held Guarded handle does that.
    for depth in 1..=components.len() {
        let child = root
            .final_path()
            .join(components[..depth].iter().collect::<PathBuf>());
        match std::fs::symlink_metadata(&child) {
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err(refusal(value));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(format!("data.dir の要素を確認できません: {error}")),
        }
    }
    Ok(resolved)
}

fn refusal(value: &str) -> String {
    format!("サービスの data.dir はデータルート配下の通常のフォルダだけを指定できます: {value:?}")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, EngineListen, EngineRuntime, EngineStartError};
    use crate::hub::UnavailableKeyStore;
    use crate::instance_lock::ChronoGazerHost;
    use banto_safe_root::acl::current_user_sid;
    use banto_safe_root::guard::KnownFolders;
    use banto_safe_root::{open_root, Mode, RootSpec};
    use std::sync::Arc;

    fn guarded(path: &Path) -> SafeRoot {
        open_root(&RootSpec {
            expected: path.to_path_buf(),
            known: KnownFolders::default(),
            owners: vec![current_user_sid().expect("test user")],
            create_access: None,
            create: false,
            mode: Mode::Guarded,
        })
        .expect("guarded test root")
    }

    #[test]
    fn internal_relative_and_absolute_paths_are_accepted_without_creating_them() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(path.join("data")).unwrap();
        let root = guarded(&path);

        let relative = validate_service_data_dir(&root, "./data/new").unwrap();
        assert_eq!(relative, root.final_path().join("data/new"));
        assert!(
            !relative.exists(),
            "validation must not create the directory"
        );

        let absolute = path.join("data");
        assert_eq!(
            validate_service_data_dir(&root, absolute.to_str().unwrap()).unwrap(),
            root.final_path().join("data")
        );
        let case_variant = absolute.to_string_lossy().to_uppercase();
        assert!(paths_match(
            &validate_service_data_dir(&root, &case_variant).unwrap(),
            &root.final_path().join("data")
        ));
        assert_eq!(
            validate_service_data_dir(&root, root.final_path().join("data").to_str().unwrap())
                .unwrap(),
            root.final_path().join("data")
        );
        assert_eq!(
            validate_service_data_dir(&root, "").unwrap(),
            root.final_path().join("data")
        );
    }

    #[test]
    fn outside_parent_unc_drive_root_and_non_directory_are_rejected_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("file"), b"keep").unwrap();
        let root = guarded(&path);
        let outside = temp.path().join("outside");
        for value in [
            outside.to_str().unwrap(),
            path.to_str().unwrap(),
            ".",
            "../outside",
            "data/../outside",
            r"\\server\share\outside",
            r"C:\",
            "C:relative",
            "file/child",
            "data/CON",
        ] {
            assert!(validate_service_data_dir(&root, value).is_err(), "{value}");
        }
        assert!(!outside.exists());
        assert_eq!(std::fs::read(path.join("file")).unwrap(), b"keep");
    }

    #[test]
    fn reparse_child_is_rejected_without_following_the_junction() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let linked = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(path.join("link"))
            .arg(&outside)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink /J");
        if !linked.success() {
            assert!(
                std::env::var_os("CI").is_none(),
                "CI could not create the test junction"
            );
            eprintln!("skip: directory junction unavailable in this sandbox");
            return;
        }
        let root = guarded(&path);
        assert!(validate_service_data_dir(&root, "link/new").is_err());
        assert!(!outside.join("new").exists());
    }

    fn service_config(path: &Path, root: Arc<SafeRoot>) -> EngineConfig {
        EngineConfig {
            db_path: path.join("chronogazer.sqlite3"),
            drivers: crate::test_support::drivers(),
            data_base: root.final_path().to_path_buf(),
            service_data_root: Some(root),
            key_store: Arc::new(UnavailableKeyStore),
            attach_hub: false,
            host: ChronoGazerHost::Service,
            listen: EngineListen::Disabled,
            allow_setup: false,
            seed_viewer_public: false,
            log_label: "service-data-dir-test",
            audit_origin: "rest",
            store_config_required: true,
            store_dir_check_required: true,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saved_outside_value_stops_before_probe_or_collector_on_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("old-day.txt"), b"keep").unwrap();
        let root = Arc::new(guarded(&path));

        for setting in [outside.to_str().unwrap(), "../outside"] {
            // Models a previously saved or restored DB: no recovery UI writes
            // the setting during this service startup.
            let opened = EngineRuntime::open(service_config(&path, root.clone()))
                .await
                .unwrap();
            crate::settings::set_data_dir(&opened.settings(), setting)
                .await
                .unwrap();
            opened.close().await;

            let error = match EngineRuntime::start(service_config(&path, root.clone())).await {
                Ok(engine) => {
                    engine.shutdown(std::time::Duration::from_secs(5)).await;
                    panic!("service accepted {setting:?}")
                }
                Err(error) => error,
            };
            assert!(
                matches!(error, EngineStartError::StoreDirNotAllowed { .. }),
                "{error:?}"
            );
            assert_eq!(std::fs::read(outside.join("old-day.txt")).unwrap(), b"keep");
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
            assert!(!path.join("data").exists(), "collector did not start");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restored_outside_value_is_refused_before_touching_the_old_data() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("old-day.txt"), b"keep").unwrap();
        let root = Arc::new(guarded(&path));

        // Create a real backup containing the historical external setting.
        let mut setup = service_config(&path, root.clone());
        setup.host = ChronoGazerHost::Serve;
        setup.service_data_root = None;
        setup.store_dir_check_required = false;
        let engine = EngineRuntime::start(setup).await.unwrap();
        crate::settings::set_data_dir(&engine.services().settings, outside.to_str().unwrap())
            .await
            .unwrap();
        let backup = engine.services().backup.create().await.unwrap();
        assert!(
            !engine
                .shutdown(std::time::Duration::from_secs(10))
                .await
                .timed_out
        );
        drop(engine); // shutdown closes the pool; dropping releases A1.

        let db = path.join("chronogazer.sqlite3");
        std::fs::write(&db, b"damaged DB".repeat(100)).unwrap();
        let prepared =
            crate::startup_recovery::restore_from_backup(&db, &backup.file_name, "20261026-120000")
                .await
                .unwrap();
        let opened =
            EngineRuntime::open_with_recovery_lock(service_config(&path, root), prepared.lock)
                .await
                .unwrap();
        let error = match opened.start().await {
            Ok(engine) => {
                engine.shutdown(std::time::Duration::from_secs(5)).await;
                panic!("restored external data.dir was accepted")
            }
            Err(error) => error,
        };
        assert!(matches!(error, EngineStartError::StoreDirNotAllowed { .. }));
        assert_eq!(std::fs::read(outside.join("old-day.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }
}
