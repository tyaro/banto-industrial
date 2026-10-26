//! Optional, read-only per-connection discovery. Drivers own the meaning of
//! their device ranges; the Hub only validates and displays these results.

use crate::Step;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Model identified on the same physical connection as the diagnostic.
pub struct DiagnosticModel {
    /// Raw model name bytes returned by the device.
    pub raw_name: Vec<u8>,
    /// Display name interpreted by the driver.
    pub name: String,
    /// Device model code.
    pub code: u16,
    /// Driver-specific family name.
    pub family: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A device count may be exact, only a lower bound, or unavailable.
pub enum CountBound {
    /// Exact configured point count.
    Exact(u32),
    /// Proven minimum count; addresses above it remain unknown.
    AtLeast(u32),
    /// Acquisition or applicability could not be established.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Driver-owned source and unit for one configured device area.
pub struct AreaCount {
    /// Device area name.
    pub area: String,
    /// Applicability scope, such as a direct or extended mapping.
    pub scope: String,
    /// Unit of the count, such as words or bit points.
    pub unit: String,
    /// Proven bound, never inferred by the Hub.
    pub bound: CountBound,
    /// Device register or rule that produced the bound.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Driver's verdict for a whole tag, including its complete occupied span.
pub enum TagVerdict {
    /// Entire tag is proven within an applicable configured area.
    Inside(String),
    /// Entire tag is proven outside an applicable configured area.
    Outside(String),
    /// Evidence is incomplete; this is not an unlimited range.
    Unknown(String),
    /// Address or type syntax is invalid.
    Invalid(String),
    /// The model or transport does not support this tag.
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One result in the same order as the requested tags.
pub struct TagDiagnostic {
    /// Requested canonical address, checked by the Host.
    pub address: String,
    /// Verdict and its reason.
    pub verdict: TagVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Snapshot of one connection's read-only device discovery.
pub struct DiagnosticReport {
    /// Model information, if the device returned it.
    pub model: Option<DiagnosticModel>,
    /// Explanation when model information is unavailable.
    pub model_reason: Option<String>,
    /// Driver-interpreted configured area counts.
    pub counts: Vec<AreaCount>,
    /// One verdict per input tag, in input order.
    pub tags: Vec<TagDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
/// One step of the optional read-only diagnostic state machine.
pub enum DiagnosticProgress {
    /// Send a read-only diagnostic frame through the existing transport.
    Send(Step),
    /// Discovery and tag verdicts are complete.
    Done(DiagnosticReport),
}
