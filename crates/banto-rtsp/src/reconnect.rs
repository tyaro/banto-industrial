//! Overflow-safe integer exponential reconnect backoff.

use std::time::Duration;

use crate::{RtspConfigError, RtspError};

/// The network transport a future RTSP supervisor should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RtspTransport {
    Tcp,
    Udp,
}

/// Integer exponential backoff policy with a hard upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    initial_delay: Duration,
    max_delay: Duration,
    factor: u32,
}

impl ReconnectPolicy {
    pub fn new(
        initial_delay: Duration,
        max_delay: Duration,
        factor: u32,
    ) -> Result<Self, RtspError> {
        if initial_delay.is_zero() || max_delay.is_zero() || factor < 1 || initial_delay > max_delay
        {
            return Err(RtspConfigError::InvalidReconnectPolicy.into());
        }
        Ok(Self {
            initial_delay,
            max_delay,
            factor,
        })
    }

    pub const fn initial_delay(&self) -> Duration {
        self.initial_delay
    }

    pub const fn max_delay(&self) -> Duration {
        self.max_delay
    }

    pub const fn factor(&self) -> u32 {
        self.factor
    }

    /// Returns `initial_delay * factor.pow(attempt)`, saturated at max_delay.
    /// Exponentiation by squaring keeps very large attempt values bounded in
    /// runtime and all intermediate arithmetic is clamped before overflow.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let max_nanos = self.max_delay.as_nanos();
        let mut result = self.initial_delay.as_nanos();
        let mut base = u128::from(self.factor);
        let mut exponent = attempt;

        while exponent != 0 && result < max_nanos {
            if exponent & 1 == 1 {
                result = saturating_mul_at(result, base, max_nanos);
            }
            exponent >>= 1;
            if exponent != 0 {
                base = saturating_mul_at(base, base, max_nanos);
            }
        }

        nanos_to_duration(result.min(max_nanos))
    }
}

fn saturating_mul_at(left: u128, right: u128, limit: u128) -> u128 {
    left.checked_mul(right)
        .map_or(limit, |product| product.min(limit))
}

fn nanos_to_duration(nanos: u128) -> Duration {
    let seconds = (nanos / 1_000_000_000) as u64;
    let subsecond_nanos = (nanos % 1_000_000_000) as u32;
    Duration::new(seconds, subsecond_nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_initial_then_exponential_delays_and_saturates() {
        let policy =
            ReconnectPolicy::new(Duration::from_millis(100), Duration::from_millis(750), 2)
                .unwrap();

        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(750));
        assert_eq!(policy.delay_for_attempt(100), Duration::from_millis(750));
    }

    #[test]
    fn handles_factor_one_and_large_attempt_without_overflow() {
        let constant =
            ReconnectPolicy::new(Duration::from_nanos(7), Duration::from_secs(1), 1).unwrap();
        assert_eq!(
            constant.delay_for_attempt(u32::MAX),
            Duration::from_nanos(7)
        );

        let overflowing =
            ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(60), u32::MAX)
                .unwrap();
        assert_eq!(
            overflowing.delay_for_attempt(u32::MAX),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn rejects_zero_invalid_factor_and_reversed_ranges() {
        assert!(ReconnectPolicy::new(Duration::ZERO, Duration::from_secs(1), 2).is_err());
        assert!(ReconnectPolicy::new(Duration::from_secs(1), Duration::ZERO, 2).is_err());
        assert!(ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(1), 0).is_err());
        assert!(ReconnectPolicy::new(Duration::from_secs(2), Duration::from_secs(1), 2).is_err());
    }
}
