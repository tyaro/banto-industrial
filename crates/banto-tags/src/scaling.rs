//! Linear raw <-> engineering-unit scaling (recorder-requirements.md §2
//! "タグ" definition: "名前 + PLC アドレス + データ型 + スケーリング + 単位 +
//! 小数桁").
//!
//! Pure functions only - no clamping to the engineering range happens here.
//! Whether/how to clamp an out-of-range reading for display (e.g. a sensor
//! momentarily reporting past its calibrated span) is a decision for the
//! display layer (R1's trend/gauge widgets), not this crate.

use banto_core::{BantoError, FieldError};
use serde::{Deserialize, Serialize};

/// A validated raw-range -> engineering-range linear mapping. All four
/// bounds are always present on a `Scaling` value - "no scaling" is
/// represented as `Option<Scaling> == None` at the call site ([`crate::tag::Tag`]'s
/// four nullable columns collapse to this), never as degenerate bounds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scaling {
    pub raw_lo: f64,
    pub raw_hi: f64,
    pub eng_lo: f64,
    pub eng_hi: f64,
}

impl Scaling {
    /// Build a `Scaling` from four nullable columns (docs/plan.md I1 spec:
    /// "全NULL=スケーリングなし、部分NULLは検証エラー"), plus the "raw_lo ==
    /// raw_hi は拒否" rule ([`scale_raw`] divides by `raw_hi - raw_lo`, so a
    /// degenerate raw span has no defined mapping).
    ///
    /// `field` names the field group in the returned `FieldError` (today's
    /// one call site, [`crate::tag::validate_tag_input`], always passes
    /// `"scaling"`; parameterized so this stays reusable if a future entity
    /// needs its own scaling group).
    pub fn from_parts(
        raw_lo: Option<f64>,
        raw_hi: Option<f64>,
        eng_lo: Option<f64>,
        eng_hi: Option<f64>,
        field: &str,
    ) -> Result<Option<Self>, BantoError> {
        match (raw_lo, raw_hi, eng_lo, eng_hi) {
            (None, None, None, None) => Ok(None),
            (Some(raw_lo), Some(raw_hi), Some(eng_lo), Some(eng_hi)) => {
                if raw_lo == raw_hi {
                    return Err(BantoError::Validation {
                        field_errors: vec![FieldError {
                            field: field.to_string(),
                            message: "raw_lo と raw_hi は異なる値にしてください".to_string(),
                        }],
                    });
                }
                Ok(Some(Self {
                    raw_lo,
                    raw_hi,
                    eng_lo,
                    eng_hi,
                }))
            }
            _ => Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: field.to_string(),
                    message:
                        "raw_lo/raw_hi/eng_lo/eng_hi は全て指定するか、全て未指定にしてください"
                            .to_string(),
                }],
            }),
        }
    }
}

/// Linear transform from a raw PLC reading to its engineering-unit value:
/// `eng_lo + (raw - raw_lo) * (eng_hi - eng_lo) / (raw_hi - raw_lo)`.
///
/// Not clamped to `[eng_lo, eng_hi]` - see the module doc comment. `raw_hi
/// == raw_lo` never occurs through a [`Scaling`] built by [`Scaling::from_parts`]
/// (which rejects it), but this function does not re-check it: it is a
/// pure transform over an already-valid `Scaling`, matching a raw reading
/// that legitimately falls outside `[raw_lo, raw_hi]` (e.g. sensor overrange)
/// to an extrapolated engineering value rather than an error.
pub fn scale_raw(raw: f64, scaling: &Scaling) -> f64 {
    let raw_span = scaling.raw_hi - scaling.raw_lo;
    let eng_span = scaling.eng_hi - scaling.eng_lo;
    scaling.eng_lo + (raw - scaling.raw_lo) * eng_span / raw_span
}

/// Inverse of [`scale_raw`]: engineering value -> raw. Not used by v1's
/// read-only collection path (recorder-requirements.md §7: no PLC writes)
/// but kept alongside `scale_raw` for the future recipe-download use case
/// referenced in plan.md §3.
///
/// Degenerate when `eng_hi == eng_lo` (a flat scaling with zero engineering
/// span): `Scaling::from_parts` does not reject this (it is a valid, if
/// unusual, `scale_raw` mapping - every raw value maps to the same
/// engineering constant), so `unscale` on such a value divides by zero and
/// returns `+-inf`/`NaN` - same "caller's responsibility, not clamped or
/// guarded" contract as the rest of this module.
pub fn unscale(eng: f64, scaling: &Scaling) -> f64 {
    let raw_span = scaling.raw_hi - scaling.raw_lo;
    let eng_span = scaling.eng_hi - scaling.eng_lo;
    scaling.raw_lo + (eng - scaling.eng_lo) * raw_span / eng_span
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positive() -> Scaling {
        Scaling {
            raw_lo: 0.0,
            raw_hi: 4095.0,
            eng_lo: 0.0,
            eng_hi: 100.0,
        }
    }

    #[test]
    fn scale_raw_maps_endpoints_exactly() {
        let s = positive();
        assert_eq!(scale_raw(0.0, &s), 0.0);
        assert_eq!(scale_raw(4095.0, &s), 100.0);
    }

    #[test]
    fn scale_raw_maps_midpoint() {
        let s = positive();
        assert!((scale_raw(2047.5, &s) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn scale_raw_extrapolates_beyond_range_without_clamping() {
        let s = positive();
        assert!(scale_raw(4195.0, &s) > 100.0);
        assert!(scale_raw(-100.0, &s) < 0.0);
    }

    /// e.g. a loop wired to read descending: raw 0 -> eng 100, raw 4095 ->
    /// eng 0 (`eng_hi < eng_lo`).
    #[test]
    fn scale_raw_supports_negative_slope() {
        let s = Scaling {
            raw_lo: 0.0,
            raw_hi: 4095.0,
            eng_lo: 100.0,
            eng_hi: 0.0,
        };
        assert_eq!(scale_raw(0.0, &s), 100.0);
        assert_eq!(scale_raw(4095.0, &s), 0.0);
        assert!((scale_raw(2047.5, &s) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn scale_raw_supports_negative_eng_range() {
        let s = Scaling {
            raw_lo: 0.0,
            raw_hi: 100.0,
            eng_lo: -50.0,
            eng_hi: 50.0,
        };
        assert_eq!(scale_raw(0.0, &s), -50.0);
        assert_eq!(scale_raw(100.0, &s), 50.0);
        assert_eq!(scale_raw(50.0, &s), 0.0);
    }

    #[test]
    fn scale_raw_supports_negative_raw_range() {
        let s = Scaling {
            raw_lo: -32768.0,
            raw_hi: 32767.0,
            eng_lo: 0.0,
            eng_hi: 100.0,
        };
        assert!((scale_raw(-32768.0, &s) - 0.0).abs() < 1e-9);
        assert!((scale_raw(32767.0, &s) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn unscale_is_the_inverse_of_scale_raw() {
        let s = positive();
        for raw in [0.0, 1234.5, 4095.0] {
            let eng = scale_raw(raw, &s);
            assert!((unscale(eng, &s) - raw).abs() < 1e-6);
        }
    }

    #[test]
    fn unscale_supports_negative_slope() {
        let s = Scaling {
            raw_lo: 0.0,
            raw_hi: 4095.0,
            eng_lo: 100.0,
            eng_hi: 0.0,
        };
        assert!((unscale(75.0, &s) - 1023.75).abs() < 1e-6);
    }

    #[test]
    fn unscale_degenerate_eng_span_yields_non_finite() {
        // eng_hi == eng_lo is allowed by from_parts (not rejected), so this
        // exercises the documented "not guarded" divide-by-zero contract.
        let s = Scaling {
            raw_lo: 0.0,
            raw_hi: 100.0,
            eng_lo: 50.0,
            eng_hi: 50.0,
        };
        // Any eng value other than exactly 50.0 has no finite raw preimage.
        assert!(!unscale(60.0, &s).is_finite());
    }

    #[test]
    fn from_parts_all_none_is_no_scaling() {
        assert_eq!(
            Scaling::from_parts(None, None, None, None, "scaling").unwrap(),
            None
        );
    }

    #[test]
    fn from_parts_all_some_builds_scaling() {
        let s = Scaling::from_parts(Some(0.0), Some(4095.0), Some(0.0), Some(100.0), "scaling")
            .unwrap()
            .unwrap();
        assert_eq!(s, positive());
    }

    #[test]
    fn from_parts_partial_null_is_a_validation_error() {
        let err =
            Scaling::from_parts(Some(0.0), Some(4095.0), None, Some(100.0), "scaling").unwrap_err();
        assert!(matches!(err, BantoError::Validation { .. }));
    }

    #[test]
    fn from_parts_partial_null_reports_the_given_field_name() {
        let err = Scaling::from_parts(None, Some(4095.0), None, None, "scaling").unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors.len(), 1);
                assert_eq!(field_errors[0].field, "scaling");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn from_parts_rejects_equal_raw_bounds() {
        let err = Scaling::from_parts(Some(10.0), Some(10.0), Some(0.0), Some(100.0), "scaling")
            .unwrap_err();
        match err {
            BantoError::Validation { field_errors } => {
                assert_eq!(field_errors.len(), 1);
                assert_eq!(field_errors[0].field, "scaling");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn from_parts_allows_equal_eng_bounds() {
        // Degenerate but not rejected per spec (only raw_lo == raw_hi is
        // rejected) - every raw value maps to the same constant eng value.
        let s = Scaling::from_parts(Some(0.0), Some(100.0), Some(50.0), Some(50.0), "scaling")
            .unwrap()
            .unwrap();
        assert_eq!(scale_raw(0.0, &s), 50.0);
        assert_eq!(scale_raw(100.0, &s), 50.0);
    }
}
