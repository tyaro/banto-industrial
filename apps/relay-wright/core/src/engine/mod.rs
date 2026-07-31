//! relay-wright's PLC access broker (W3-A, `luminous-discovering-goblet.md`'s
//! "アーキテクチャ決定（PLCアクセスブローカー）" and "W3-A").
//!
//! ## What lives here (and what deliberately does not)
//!
//! The broker is the one component that owns a **live** SLMP session. The plan
//! keeps `banto-tags` a passive definition store (invariant: the service layer
//! is tauri/axum/tokio-free), so the live socket lives in the app instead, as a
//! per-CPU broker task. One task owns one `slmp::SLMPClient` and every read
//! (future condition poller) and write (future auto-writer) to that CPU passes
//! through it - a single serialization point that structurally prevents a read
//! and a write to the same CPU from interleaving on the wire, and sidesteps the
//! MELSEC concurrent-session ceiling.
//!
//! W3-A is **infrastructure only**. The broker executes whatever read/write it
//! is handed; it has no notion of rules, arming, rate limiting, or write
//! auditing. Those belong to W3-B's auto-write engine
//! (`engine/{poller,rule_engine,writer}.rs`), which will hold the broker's
//! [`BrokerHandle`]s. Nothing here makes an automatic write.
//!
//! See [`broker`] for the concurrency design (channel shape, how serialization
//! is guaranteed, the reconnect/backoff and queued-request policies) and for how
//! read vs write submission is separated so W3-B can lock write access down.

pub mod broker;

pub use broker::{BackoffConfig, BrokerError, BrokerHandle, BrokerSupervisor, ReadOnlyHandle};
