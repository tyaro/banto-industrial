//! Service-only `data.dir` policy (#659, stage A).
//!
//! `run-service` has already opened the data root with `Mode::Guarded` and
//! closed its tree before the engine reads this setting. Existing components
//! are checked relative to that held handle without following reparse points.
//! The `Mode::Plain` branch exists for the service state-machine's FakeAcl
//! tests. It is not a security boundary and is never used by RealAcl.
//!
//! After policy validation, missing children are created relative to held
//! handles with operation-mode ACLs, and the final child stays held by the
//! collector. SQLite still reopens paths and sidecars; this does not solve
//! races involving those opens or old write-capable handles.

use std::path::{Component, Path, PathBuf};

#[cfg(windows)]
use banto_safe_root::guard::paths_match;
use banto_safe_root::guard::{is_plain_child_name, is_same_or_inside};
use banto_safe_root::root::ManagedDir;
use banto_safe_root::SafeRoot;
use std::io::Write;
use std::sync::Arc;

/// Validate without writes, then open or create each child from held handles.
/// The returned directory must be held for the collector's full lifetime.
pub(crate) fn open_service_data_dir(
    root: &SafeRoot,
    configured: &str,
) -> Result<(PathBuf, Arc<ManagedDir>), String> {
    let path = validate_service_data_dir(root, configured)?;
    let relative = path
        .strip_prefix(root.final_path())
        .expect("validator returns a child of this root");
    let held = root
        .open_relative_operation_dir(relative)
        .map_err(|error| format!("data.dir のフォルダを安全に開けません: {error:?}"))?;
    Ok((path, Arc::new(held)))
}

/// Service-only writable probe. Creation and deletion stay relative to the
/// held child; no path is reopened between validation and the probe.
pub(crate) fn check_service_writable_dir(dir: &ManagedDir) -> Result<(), String> {
    for _ in 0..16 {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
        let name = format!(
            ".chronogazer-write-check-{:016x}.tmp",
            u64::from_le_bytes(bytes)
        );
        let Some(mut file) = dir.create_new(&name).map_err(|error| error.to_string())? else {
            continue;
        };
        let written = file.write_all(b"ok").and_then(|()| file.flush());
        drop(file);
        let removed = dir.remove(&name);
        written.map_err(|error| error.to_string())?;
        if !removed.map_err(|error| error.to_string())? {
            return Err("書き込み確認用ファイルが消えました".to_string());
        }
        return Ok(());
    }
    Err("書き込み確認用の名前が重複しました".to_string())
}

/// List from the held directory and delete each selected name through that
/// same handle. `remove` reopens the entry without following and rechecks its
/// identity after a list-then-swap. SQLite sidecars are intentionally skipped.
pub(crate) fn prune_service_data_files(
    dir: &ManagedDir,
    retention_days: u32,
    today: banto_tstore::LocalDate,
    pinned: &[banto_tstore::LocalDate],
) -> Result<banto_tstore::PruneReport, banto_tstore::TstoreError> {
    prune_service_data_files_with(dir, retention_days, today, pinned, &|_| {})
}

fn prune_service_data_files_with(
    dir: &ManagedDir,
    retention_days: u32,
    today: banto_tstore::LocalDate,
    pinned: &[banto_tstore::LocalDate],
    before_remove: &dyn Fn(&str),
) -> Result<banto_tstore::PruneReport, banto_tstore::TstoreError> {
    use banto_safe_root::root::EntryKind;
    let mut files: Vec<_> = dir
        .list()
        .map_err(|error| banto_tstore::TstoreError::Storage(error.to_string()))?
        .into_iter()
        .filter(|entry| entry.kind == EntryKind::File && entry.reparse_tag.is_none())
        .filter_map(|entry| banto_tstore::parse_data_file_info(dir.final_path(), &entry.name))
        .collect();
    files.sort_by_key(|file| (file.date, file.seq));
    let plan = banto_tstore::plan_prune_candidates(files, retention_days, today, pinned);
    for path in &plan.deleted {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("parsed data file names are UTF-8");
        before_remove(name);
        let removed = dir
            .remove(name)
            .map_err(|error| banto_tstore::TstoreError::Storage(error.to_string()))?;
        if !removed {
            return Err(banto_tstore::TstoreError::Storage(format!(
                "保持期間の削除対象が消えました: {name}"
            )));
        }
    }
    Ok(plan)
}

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
        // Compare whole components, not a string prefix. The display and
        // final spellings may differ; a new verbatim prefix is not accepted.
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
    use crate::collect::CollectorState;
    use crate::engine::{EngineConfig, EngineListen, EngineRuntime, EngineStartError};
    use crate::hub::UnavailableKeyStore;
    use crate::instance_lock::ChronoGazerHost;
    use banto_safe_root::acl::current_user_sid;
    use banto_safe_root::guard::KnownFolders;
    use banto_safe_root::root::ManagedSpec;
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

    fn plain(path: &Path) -> SafeRoot {
        open_root(&RootSpec {
            expected: path.to_path_buf(),
            known: KnownFolders::default(),
            owners: Vec::new(),
            create_access: None,
            create: false,
            mode: Mode::Plain,
        })
        .expect("plain simulation root")
    }

    fn held_child(path: &Path) -> ManagedDir {
        let root = guarded(path);
        let mut children = root
            .open_managed(
                &[ManagedSpec {
                    key: "data",
                    parent: None,
                    name: "data",
                    create: false,
                }],
                None,
            )
            .unwrap();
        children.take("data").unwrap()
    }

    #[test]
    fn operation_acl_is_checked_before_creating_a_missing_grandchild() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        std::fs::create_dir(&root_path).unwrap();
        std::fs::create_dir(root_path.join("data")).unwrap();
        let root = guarded(&root_path);
        assert!(open_service_data_dir(&root, "data/new").is_err());
        assert!(!root_path.join("data/new").exists());
    }

    #[test]
    fn held_child_probe_and_prune_leave_outside_dummy_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root_path).unwrap();
        std::fs::create_dir(root_path.join("data")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let old = "20260101-001.sqlite3";
        // Create in reverse date/sequence order; the report must retain the
        // path-based tstore scan's (date, seq) ordering.
        for name in ["20260102-001.sqlite3", "20260101-002.sqlite3", old] {
            std::fs::write(root_path.join("data").join(name), b"inside").unwrap();
        }
        std::fs::write(outside.join(old), b"outside").unwrap();
        let held = held_child(&root_path);
        check_service_writable_dir(&held).unwrap();
        let report =
            prune_service_data_files(&held, 0, banto_tstore::LocalDate::new(2026, 10, 26), &[])
                .unwrap();
        // A Windows temp path may use an 8.3 alias while the held handle
        // reports its long spelling. Compare full paths from that handle.
        assert_eq!(
            report.deleted,
            [
                held.final_path().join("20260101-001.sqlite3"),
                held.final_path().join("20260101-002.sqlite3"),
                held.final_path().join("20260102-001.sqlite3"),
            ]
        );
        assert!(!root_path.join("data").join(old).exists());
        assert_eq!(std::fs::read(outside.join(old)).unwrap(), b"outside");
    }

    #[test]
    fn held_child_cannot_be_swapped_for_an_outside_link() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root_path).unwrap();
        std::fs::create_dir(root_path.join("data")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("dummy"), b"keep").unwrap();
        let held = held_child(&root_path);
        assert!(
            std::fs::rename(root_path.join("data"), root_path.join("parked")).is_err(),
            "a held Windows directory must not be swapped by renaming"
        );
        assert!(
            std::os::windows::fs::symlink_dir(&outside, root_path.join("data")).is_err(),
            "the still-occupied child name cannot become an outside link"
        );
        check_service_writable_dir(&held).unwrap();
        assert!(root_path.join("data").is_dir());
        assert!(!root_path.join("data").is_symlink());
        assert_eq!(std::fs::read(outside.join("dummy")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    }

    #[test]
    fn prune_rechecks_a_name_swapped_to_a_directory_after_listing() {
        let temp = tempfile::tempdir().unwrap();
        let root_path = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root_path).unwrap();
        std::fs::create_dir(root_path.join("data")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let old = "20260101-001.sqlite3";
        std::fs::write(root_path.join("data").join(old), b"inside").unwrap();
        std::fs::write(outside.join(old), b"outside").unwrap();
        let held = held_child(&root_path);
        let result = prune_service_data_files_with(
            &held,
            0,
            banto_tstore::LocalDate::new(2026, 10, 26),
            &[],
            &|name| {
                let target = root_path.join("data").join(name);
                std::fs::remove_file(&target).unwrap();
                std::fs::create_dir(&target).unwrap();
            },
        );
        assert!(result.is_err(), "swapped directory must not be removed");
        assert!(root_path.join("data").join(old).is_dir());
        assert_eq!(std::fs::read(outside.join(old)).unwrap(), b"outside");
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
        let verbatim = format!(r"\\?\{}", path.join("data").display());
        assert!(validate_service_data_dir(&root, &verbatim).is_err());
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
    async fn allowed_internal_dir_starts_collector_and_shuts_down_in_plain_simulation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("root");
        std::fs::create_dir(&path).unwrap();
        let root = Arc::new(plain(&path));

        let engine = EngineRuntime::start(service_config(&path, root.clone()))
            .await
            .expect("allowed service data.dir starts");
        let child = root.final_path().join("data");
        assert!(child.is_dir(), "the service created its internal child");
        assert_eq!(engine.services().collect.data_dir(), child);
        assert!(
            std::fs::read_dir(&child).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("write-check")),
            "the handle-relative writable probe was cleaned up"
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if matches!(
                    engine.services().collect.state(),
                    CollectorState::NoTargets { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("collector processed the empty registry");
        assert!(
            !engine
                .shutdown(std::time::Duration::from_secs(10))
                .await
                .timed_out,
            "clean shutdown"
        );
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
