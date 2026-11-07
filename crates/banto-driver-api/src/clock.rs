//! Optional PLC clock set (WIT `clock-v1`). The host chooses the local civil
//! time and when to run; the driver owns which device registers carry it,
//! the per-model procedure and how success is recognized. A driver without
//! this contract keeps its read/write behavior unchanged.

use crate::error::ItemError;
use crate::frame::Step;

/// Local civil date and time chosen by the host (WIT `civil-time`). No time
/// zone and no day of the week: the driver derives the weekday from the date
/// and validates the calendar itself.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CivilTime {
    /// Year, e.g. 2026.
    pub year: u16,
    /// Month, 1-12.
    pub month: u8,
    /// Day of the month, 1-31.
    pub day: u8,
    /// Hour, 0-23.
    pub hour: u8,
    /// Minute, 0-59.
    pub minute: u8,
    /// Second, 0-59.
    pub second: u8,
}

impl CivilTime {
    /// A civil time from its six fields. Not validated here: the driver
    /// refuses an impossible date before anything is sent.
    pub const fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }
}

/// [`FrameConnection::set_clock_next`](crate::FrameConnection::set_clock_next)'s
/// answer (WIT `clock-progress`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum ClockProgress {
    /// The next request to send through the existing transport.
    Send(Step),
    /// The end of this clock set. `Err` is the device's or the driver's
    /// refusal (unsupported model, gate disabled, end code, outcome unknown);
    /// the connection stays usable.
    Done(Result<(), ItemError>),
}
