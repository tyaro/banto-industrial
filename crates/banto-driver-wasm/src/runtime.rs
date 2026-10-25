//! Wasmtime component execution. The canonical WIT is in banto-driver-wasm;
//! `../wit/driver.wit` is a checked-in byte-for-byte copy for host builds.
//! No WASI linker is installed, so the guest has no filesystem or socket I/O.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder, Trap};

wasmtime::component::bindgen!({ path: "wit", world: "driver" });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCause {
    Trap,
    EpochDeadline,
    MemoryLimit,
    Component,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub operation: &'static str,
    pub cause: RuntimeCause,
    pub detail: String,
}

#[derive(Debug)]
pub struct RuntimeError(pub Diagnostic);

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WASM {}: {:?}: {}",
            self.0.operation, self.0.cause, self.0.detail
        )
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone)]
pub struct Diagnostics(Arc<Mutex<Vec<Diagnostic>>>);

impl Diagnostics {
    pub fn snapshot(&self) -> Vec<Diagnostic> {
        self.0.lock().expect("diagnostic lock").clone()
    }
    fn record(&self, error: RuntimeError) -> RuntimeError {
        self.0
            .lock()
            .expect("diagnostic lock")
            .push(error.0.clone());
        error
    }
}

#[derive(Debug)]
struct MemoryLimitExceeded;
impl std::fmt::Display for MemoryLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WASM memory limit exceeded")
    }
}
impl std::error::Error for MemoryLimitExceeded {}

struct Limits(StoreLimits);
impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > 64 * 1024 * 1024 {
            return Err(MemoryLimitExceeded.into());
        }
        self.0.memory_growing(current, desired, maximum)
    }
    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.0.table_growing(current, desired, maximum)
    }
}

pub(crate) struct HostState {
    limits: Limits,
}

pub struct Instance {
    pub store: Store<HostState>,
    pub driver: Driver,
}

pub struct Runtime {
    engine: Engine,
    component: Component,
    diagnostics: Diagnostics,
    ticking: Arc<AtomicBool>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

impl Runtime {
    pub fn new(bytes: &[u8]) -> Result<Self, RuntimeError> {
        let mut config = Config::new();
        config.wasm_component_model(true).epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| classify("engine", e))?;
        let component = Component::new(&engine, bytes).map_err(|e| classify("compile", e))?;
        let ticking = Arc::new(AtomicBool::new(true));
        let tick_enabled = ticking.clone();
        let tick_engine = engine.clone();
        let ticker = std::thread::spawn(move || {
            while tick_enabled.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
                tick_engine.increment_epoch();
            }
        });
        Ok(Self {
            engine,
            component,
            diagnostics: Diagnostics(Arc::new(Mutex::new(Vec::new()))),
            ticking,
            ticker: Some(ticker),
        })
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.clone()
    }

    pub fn instantiate(&self) -> Result<Instance, RuntimeError> {
        let mut store = Store::new(
            &self.engine,
            HostState {
                limits: Limits(
                    StoreLimitsBuilder::new()
                        .memory_size(64 * 1024 * 1024)
                        .build(),
                ),
            },
        );
        store.limiter(|state| &mut state.limits);
        // One second, with a 10 ms epoch ticker. Calls have a bounded CPU budget.
        store.set_epoch_deadline(100);
        let linker = Linker::new(&self.engine);
        let driver = Driver::instantiate(&mut store, &self.component, &linker)
            .map_err(|e| self.record("instantiate", e))?;
        Ok(Instance { store, driver })
    }

    pub fn record(&self, operation: &'static str, error: wasmtime::Error) -> RuntimeError {
        self.diagnostics.record(classify(operation, error))
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.ticking.store(false, Ordering::Relaxed);
        if let Some(handle) = self.ticker.take() {
            let _ = handle.join();
        }
    }
}

pub fn classify(operation: &'static str, error: wasmtime::Error) -> RuntimeError {
    let cause = if error.chain().any(|e| e.is::<MemoryLimitExceeded>()) {
        RuntimeCause::MemoryLimit
    } else {
        match error.downcast_ref::<Trap>() {
            Some(Trap::Interrupt) => RuntimeCause::EpochDeadline,
            Some(_) => RuntimeCause::Trap,
            None => RuntimeCause::Component,
        }
    };
    RuntimeError(Diagnostic {
        operation,
        cause,
        detail: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_causes_are_typed() {
        assert_eq!(
            classify("read", MemoryLimitExceeded.into()).0.cause,
            RuntimeCause::MemoryLimit
        );
        assert_eq!(
            classify("read", Trap::Interrupt.into()).0.cause,
            RuntimeCause::EpochDeadline
        );
        assert_eq!(
            classify("read", Trap::UnreachableCodeReached.into())
                .0
                .cause,
            RuntimeCause::Trap
        );
        assert_eq!(
            classify("read", Trap::MemoryOutOfBounds.into()).0.cause,
            RuntimeCause::Trap
        );
    }
}
