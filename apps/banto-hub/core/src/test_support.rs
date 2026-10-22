//! Shared `#[cfg(test)]`-only helpers for this crate's **unit** test modules
//! (`#[cfg(test)] mod tests` blocks living inside `src/*.rs`, e.g.
//! `hub.rs::tests`) - not the `tests/*.rs` integration tests, which have
//! their own `tests/common/mod.rs` (see
//! `apps/banto-hub/core/tests/common/mod.rs`'s module doc for the full
//! writeup of the leak this also fixes: closing a WAL-mode `SqlitePool` on
//! Windows does not synchronously release its file handles, so
//! `remove_dir_all` issued right after can transiently fail).
//!
//! ## Why this module exists (2026-08-08 audit finding)
//!
//! `hub.rs::tests::manager_env` used to build its temp dir with
//! `tempfile::tempdir()` directly and return `(SqlitePool, tempfile::TempDir,
//! CollectorManager)`. Every call site destructured it as
//! `let (_pool, _dir, manager) = manager_env().await;` - and Rust drops a
//! `let (a, b, c) = ...` pattern's bindings in *reverse* textual order
//! (`c`, then `b`, then `a`), so `_dir` (position 2) dropped **before**
//! `_pool` (position 1). `tempfile::TempDir::drop` does one, non-retrying
//! `remove_dir_all` and swallows the error - with `_pool`'s registry file
//! still open at that point, it silently failed every time. Measured: one
//! `cargo test -p banto-hub-core --lib` run left 5 directories behind
//! (`.tmpXXXXXXXX`, each holding a live `registry.sqlite3`).
//!
//! Two independent things had to change, not just adding a retry:
//! 1. **Drop order**: unlike a `struct`'s fields (which always drop in
//!    forward declaration order regardless of how a caller uses the value),
//!    a *tuple*'s bindings drop in reverse of how the caller's `let`
//!    pattern lists them - so the fix has to live at each call site, not
//!    just in this module. `manager_env` now returns
//!    `(TempDir, CollectorManager, SqlitePool)` (dir *first*, pool *last*),
//!    and every call site destructures naturally in that order, so the
//!    pool - now bound last - drops *first*, before `dir`'s cleanup runs.
//! 2. **Retry**: even with correct ordering, the residual OS-level close
//!    lag `apps/banto-hub/core/tests/common/mod.rs` documents still
//!    applies, so [`TempDir`] retries the same way `TempEnv` there does -
//!    which is also why every test using it must run on a multi-thread
//!    tokio runtime with >= 2 workers (a blocking retry inside `Drop::drop`
//!    on a single-threaded runtime starves the only worker thread and
//!    prevents the background close from ever being polled - see that same
//!    module doc for the measured 0%-vs-100% success rates).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

const RETRY_DELAY: Duration = Duration::from_millis(50);
// 40 (~2s) was the original budget; measured flaky under a full `cargo
// test -p banto-hub-core --lib` run (136 tests, high thread contention) -
// 2 of `hub.rs`'s `manager_env`-based tests occasionally still leaked even
// with correct drop ordering, consistent with the OS-level residual close
// lag this retry exists for simply taking longer under load. Doubled to
// give more headroom without changing the common-case cost (convergence is
// still typically within the first 1-2 attempts).
const MAX_ATTEMPTS: u32 = 80;

/// マイグレーション済みの空 DB ファイルのバイト列（1 プロセスで 1 回だけ作る）。
static TEMPLATE_DB: tokio::sync::OnceCell<Vec<u8>> = tokio::sync::OnceCell::const_new();

/// マイグレーション済みのテンプレート DB を `path` にコピーしてから
/// [`crate::db::init_db`] で開く。`init_db` を毎回ゼロから走らせる代わりに使う。
///
/// #ci-speed（2026-10-19）: windows-latest の CI で、`init_db` の全マイグレー
/// ション（1 本ごとにコミット = ディスクフラッシュ）を毎回走らせる unit test
/// （`controller::tests::racing_start_and_begin_quit_never_silently_stop_a_new_run`
/// の 50 回ループ、`write_control::tests::a_failed_or_crashed_resume_always_restarts_stopped`
/// の 42 ケース）が 140〜150 秒かかった（ローカル Linux では 7〜9 秒。原因は
/// 仮説で、CI の前後比較で確かめる）。テンプレートは `init_db` で作った後に
/// `wal_checkpoint(TRUNCATE)` して pool を閉じ、本体ファイルのバイト列だけ保持する
/// （チェックポイント後なので `-wal` / `-shm` は不要）。各回はそれを書き出して
/// 本物の `init_db` を通すので、接続設定も検証（旧形式チェック・適用済み
/// マイグレーションの照合）も本番と同じで、テストが確かめる内容は変わらない。
pub(crate) async fn init_db_from_template(path: &Path) -> sqlx::SqlitePool {
    let bytes = TEMPLATE_DB
        .get_or_init(|| async {
            let dir = TempDir::new("db-template");
            let template = dir.path().join("template.sqlite3");
            let pool = crate::db::init_db(&template)
                .await
                .expect("init_db (template)");
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&pool)
                .await
                .expect("wal_checkpoint");
            pool.close().await;
            std::fs::read(&template).expect("read template db")
            // `dir` はここで drop され、バイト列を読み終えた後に消える。
        })
        .await;
    std::fs::write(path, bytes).expect("write template db");
    crate::db::init_db(path).await.expect("init_db (copy)")
}

/// A fresh temp directory for one unit test, cleaned up on drop. See this
/// module's doc comment for why `drop` retries and why callers must control
/// the drop order relative to any `SqlitePool` pointed at a file inside it.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "banto-hub-core-unit-test-{}-{label}-{id}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for attempt in 1..=MAX_ATTEMPTS {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    std::thread::sleep(RETRY_DELAY);
                }
                Err(err) => {
                    eprintln!(
                        "TempDir: giving up removing {:?} after {attempt} attempts: {err}",
                        self.0
                    );
                }
            }
        }
    }
}

/// R2b-0: the drivers a unit test injects into `HubSessions::new` /
/// `HubConfig` (docs/banto-driver-api-design.md §6.4・§7.4). Until R2b-0 this
/// was `banto_drivers::builtin()`; core no longer depends on `banto-drivers`
/// (not even as a dev-dependency, §2.4), so a driver-only change does not
/// select this crate in CI. The `src` tests are about the hub's own machinery
/// (the session directory, routing, the runtime), not about a protocol, so a
/// fake is enough:
///
/// - `registry`: **fake session-type** drivers for `slmp` / `modbus-tcp`
///   ([`FakePlcDriver`], R2b-3a-2, docs/banto-hub-driver-io-design.md §4.2:
///   the broker dials through the registry, so the same two ids as the
///   built-ins make `HubSessions::is_broker_managed` answer exactly as in
///   production), plus the host-provided protocols (`push` / `postgres` /
///   `virtual`, R2b-1a), as a host registers them. A fake session connects
///   iff something accepts TCP on the settings' `host:port` (a dead port
///   fails like a real driver's dial), and behind it is a tiny fake device
///   ([`FakeSession`]: numeric reads answer [`FAKE_WORD`], writes are
///   accepted) - there is no protocol on the wire. The fakes accept every
///   setting (the real rules are covered in `banto-hub-host`,
///   `tests/connection_validation_consistency.rs`) and read addresses with
///   `banto_plc`'s grammar of their id (R2b-2c), so the Hub's plan and its
///   immediate reads / writes parse as in production. (Design Q14 (a): the
///   Hub's REST test harness is not moved to the real drivers here.)
///   Collection reads go through the same fakes since R2b-3a-3 (the broker
///   read client; there is no direct collection client any more).
///
/// The tests that need a real SLMP / Modbus session went to
/// `apps/banto-hub/host/tests/` (R2b-0).
pub(crate) fn test_drivers() -> crate::broker_glue::HubDrivers {
    let mut registry = banto_driver_api::DriverRegistry::new();
    for protocol in ["modbus-tcp", "slmp"] {
        registry
            .register(banto_driver_api::DriverEntry::session(std::sync::Arc::new(
                FakePlcDriver::new(protocol),
            )))
            .expect("fake PLC driver ids are distinct");
    }
    crate::host_provided::register_host_provided(&mut registry)
        .expect("host-provided ids are distinct");
    crate::broker_glue::HubDrivers::new(std::sync::Arc::new(registry))
}

/// A connection row of `protocol` at `127.0.0.1:port`, built from its
/// `settings` like a stored row (R2b-3a-2: the broker dials with the
/// settings, not the flat columns). SLMP carries `cpu_type = "R"`, Modbus
/// `unit_id = 1`.
pub(crate) fn plc_conn(id: i64, protocol: &str, port: u16) -> banto_tags::PlcConnection {
    let settings = match protocol {
        "modbus-tcp" => serde_json::json!({
            "host": "127.0.0.1", "port": port, "unit_id": 1, "word_order": "high_low",
        }),
        _ => serde_json::json!({
            "host": "127.0.0.1", "port": port, "word_order": "low_high", "cpu_type": "R",
        }),
    };
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("a JSON object literal")
    };
    banto_tags::PlcConnection::from_settings(
        id,
        format!("conn-{id}"),
        protocol.to_string(),
        true,
        false,
        settings,
        1,
    )
}

/// R2b-1d / R2b-3a-2: a stand-in for `banto-driver-slmp` / `banto-driver-modbus`
/// in [`test_drivers`]'s registry - the id is right, every setting is accepted, no
/// secret keys, reads addresses with `banto_plc`'s grammar of its id (R2b-2c), and
/// as a session-type driver dials `host:port` from the settings (see
/// [`test_drivers`]).
pub(crate) struct FakePlcDriver {
    descriptor: banto_driver_api::DriverDescriptor,
}

impl FakePlcDriver {
    pub(crate) fn new(protocol: &str) -> Self {
        Self {
            descriptor: banto_driver_api::DriverDescriptor::new(
                banto_driver_api::DriverId::new(protocol).expect("valid driver id"),
                "0.0.0-test",
                protocol,
                banto_driver_api::Capabilities::READ_WRITE,
            ),
        }
    }
}

impl banto_driver_api::DriverBase for FakePlcDriver {
    fn descriptor(&self) -> &banto_driver_api::DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(
        &self,
        _settings: &banto_driver_api::Settings,
    ) -> Vec<banto_driver_api::FieldError> {
        Vec::new()
    }
    /// R2b-2c: reads an address with `banto_plc`'s grammar of this stand-in's id
    /// (what the real drivers do; canonical = `Display`), because the Hub's plan and
    /// its immediate reads / writes now ask the driver. Any other id refuses.
    fn parse_address(
        &self,
        text: &str,
        kind: &banto_driver_api::TagKind,
    ) -> Result<banto_driver_api::DriverAddress, banto_driver_api::AddressError> {
        let id = &self.descriptor.id;
        let parsed = match id.as_str() {
            "modbus-tcp" => banto_plc::Address::parse(text),
            "slmp" => banto_plc::Address::parse_slmp(text),
            _ => {
                return Err(banto_driver_api::AddressError::new(
                    "fake driver: no address rules",
                ))
            }
        }
        .map_err(|err| banto_driver_api::AddressError::new(err.to_string()))?;
        // R2b-2d: the Modbus stand-in reports the real area and its writability
        // (what the parse API returns), SLMP stays a generic writable area.
        let (area, writable, bit) = match parsed.as_modbus_ref() {
            Some((area, _, bit)) => (area.to_string(), area.is_writable(), bit),
            None => (
                "area".to_string(),
                true,
                parsed.as_slmp().and_then(|(_, _, bit)| bit),
            ),
        };
        let address = banto_driver_api::DriverAddress::new(
            id.clone(),
            parsed.to_string(),
            &area,
            writable,
            kind.word_count(),
        );
        Ok(match bit {
            Some(bit) => address.with_bit(bit),
            None => address,
        })
    }
    fn offset_address(
        &self,
        canonical: &str,
        _kind: &banto_driver_api::TagKind,
        _n: i64,
    ) -> Result<String, banto_driver_api::AddressError> {
        Ok(canonical.to_owned())
    }
    fn address_help(&self) -> String {
        String::new()
    }
    /// R2b-3a-2: the connection test's point (the real drivers name `40001` /
    /// `D0`; any address of the stand-in's grammar reads [`FAKE_WORD`]).
    fn probe(&self) -> Option<(String, banto_driver_api::TagKind)> {
        let text = match self.descriptor.id.as_str() {
            "modbus-tcp" => "40001",
            _ => "D0",
        };
        Some((
            text.to_string(),
            banto_driver_api::TagKind::numeric(banto_driver_api::DataType::U16),
        ))
    }
}

impl banto_driver_api::SessionDriver for FakePlcDriver {
    /// TCP connect to the settings' `host:port` under the context's connect
    /// timeout, then a [`FakeSession`].
    fn connect(
        &self,
        settings: &banto_driver_api::Settings,
        cx: banto_driver_api::ConnectContext,
    ) -> banto_driver_api::BoxFuture<
        'static,
        Result<Box<dyn banto_driver_api::Session>, banto_driver_api::FatalError>,
    > {
        use banto_driver_api::FatalError;
        let host = settings.get("host").unwrap_or_default().to_string();
        let port = settings.get("port").and_then(|p| p.parse::<u16>().ok());
        Box::pin(async move {
            let Some(port) = port else {
                return Err(FatalError::InvalidConfig("fake driver: no port".into()));
            };
            let stream = tokio::time::timeout(
                cx.connect_timeout,
                tokio::net::TcpStream::connect((host.as_str(), port)),
            )
            .await
            .map_err(|_| {
                FatalError::Io(format!("fake driver: connect to {host}:{port} timed out"))
            })?
            .map_err(|err| {
                FatalError::Io(format!("fake driver: connect to {host}:{port}: {err}"))
            })?;
            Ok(Box::new(FakeSession { _stream: stream }) as Box<dyn banto_driver_api::Session>)
        })
    }
}

/// The word every numeric read on a [`FakeSession`] returns (a bit reads as
/// `true`). Not `0` / `1`, so a test waiting for a real (non-default) sample
/// gets one - `hub::tests::invalid_plc_bit_metadata_is_stable_across_derived_stale`
/// collects a simulation connection, which the broker dials through these
/// fakes (the in-process simulator only supplies the listening port).
pub(crate) const FAKE_WORD: f64 = 1234.0;

/// A connected socket with no protocol behind it: a tiny fake device. Every
/// numeric read answers [`FAKE_WORD`], every write is accepted, a string read
/// is a per-item `Unsupported` (strings are protocol behaviour - tested with
/// the real drivers in `banto-hub-host` and the driver crates).
struct FakeSession {
    _stream: tokio::net::TcpStream,
}

impl banto_driver_api::Session for FakeSession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [banto_driver_api::TagSpec],
    ) -> banto_driver_api::BoxFuture<
        'a,
        Result<
            Vec<Result<banto_driver_api::Value, banto_driver_api::ItemError>>,
            banto_driver_api::FatalError,
        >,
    > {
        use banto_driver_api::{DataType, ItemError, TagKind, Value};
        Box::pin(async move {
            Ok(specs
                .iter()
                .map(|spec| match spec.kind {
                    TagKind::Numeric(DataType::Bit) => Ok(Value::Bit(true)),
                    TagKind::Numeric(_) => Ok(Value::F64(FAKE_WORD)),
                    _ => Err(ItemError::Unsupported(
                        "fake driver: string reads are not supported".to_string(),
                    )),
                })
                .collect())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [banto_driver_api::WriteItem],
    ) -> banto_driver_api::BoxFuture<
        'a,
        Result<Vec<Result<(), banto_driver_api::ItemError>>, banto_driver_api::FatalError>,
    > {
        Box::pin(async move { Ok(items.iter().map(|_| Ok(())).collect()) })
    }

    fn disconnect(&mut self) -> banto_driver_api::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// R2b-1e: a stand-in for the DPAPI protector on every OS (the startup upkeep and the
/// queue relocation tests of `crate::upgrade` and `crate::pending_changes`). Not
/// encryption: blob = `sha256("tag" + entropy)` + plaintext XOR a keystream derived
/// from the entropy, so the blob never holds the plaintext bytes and opening under
/// another reference / key fails (entropy binding), like DPAPI.
#[derive(Debug, Default)]
pub(crate) struct XorTestProtector;

fn xor_tag(entropy: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"tag");
    hash.update(entropy);
    hash.finalize().into()
}

fn xor_keystream(entropy: &[u8], len: usize) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::with_capacity(len);
    let mut counter: u64 = 0;
    while out.len() < len {
        let mut hash = Sha256::new();
        hash.update(b"ks");
        hash.update(entropy);
        hash.update(counter.to_le_bytes());
        out.extend_from_slice(&hash.finalize());
        counter += 1;
    }
    out.truncate(len);
    out
}

impl banto_secret_store::Protector for XorTestProtector {
    fn protect(&self, plain: &[u8], entropy: &[u8]) -> banto_secret_store::Result<Vec<u8>> {
        let mut blob = xor_tag(entropy).to_vec();
        blob.extend(
            plain
                .iter()
                .zip(xor_keystream(entropy, plain.len()))
                .map(|(p, k)| p ^ k),
        );
        Ok(blob)
    }

    fn unprotect(
        &self,
        blob: &[u8],
        entropy: &[u8],
    ) -> banto_secret_store::Result<zeroize::Zeroizing<Vec<u8>>> {
        if blob.len() < 32 || blob[..32] != xor_tag(entropy) {
            return Err(banto_secret_store::SecretStoreError::Unprotect { os_error: None });
        }
        let body = &blob[32..];
        Ok(zeroize::Zeroizing::new(
            body.iter()
                .zip(xor_keystream(entropy, body.len()))
                .map(|(c, k)| c ^ k)
                .collect(),
        ))
    }
}

/// R2b-1e: a protector whose every call fails (a DPAPI failure), for "the upkeep keeps
/// the row as it is and does not stop the start".
#[derive(Debug, Default)]
pub(crate) struct FailingProtector;

impl banto_secret_store::Protector for FailingProtector {
    fn protect(&self, _plain: &[u8], _entropy: &[u8]) -> banto_secret_store::Result<Vec<u8>> {
        Err(banto_secret_store::SecretStoreError::Protect { os_error: Some(5) })
    }

    fn unprotect(
        &self,
        _blob: &[u8],
        _entropy: &[u8],
    ) -> banto_secret_store::Result<zeroize::Zeroizing<Vec<u8>>> {
        Err(banto_secret_store::SecretStoreError::Unprotect { os_error: Some(5) })
    }
}

/// [`crate::secrets::ConnectionSecrets`] with [`XorTestProtector`] (the DPAPI path's shape
/// on every OS: `seal_scheme() == DpapiMachineV1`).
pub(crate) fn protected_secrets() -> crate::secrets::ConnectionSecrets {
    crate::secrets::ConnectionSecrets::new(Some(std::sync::Arc::new(XorTestProtector)))
}
