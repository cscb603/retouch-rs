//! Perceptual (human-vision) slider mapping (M5).
//!
//! Real photographic controls should NOT be pure-linear in their raw parameter:
//! human vision follows Weber–Fechner / Stevens power laws, so equal *finger
//! movement* should produce equal *perceived* change, and extreme drags should
//! ease off (weighted decay) so you can never blow out or crush the image.
//!
//! This module converts a *uniform perceptual* slider position `pos ∈ [-1, 1]`
//! (or `[0, 1]` for one-sided controls) into the raw pipeline value. The GUI
//! drives sliders in `pos` space; `Adjustments` (and TOML presets) always store
//! the raw value, so the engine stays preset-compatible and identity-safe.
//!
//! Three curve kinds:
//! - `Linear`    : raw == pos (used only where the raw value is *already*
//!                 perceptually uniform — exposure EV, hue degrees).
//! - `SoftKnee`  : `raw = center + half·tanh(K·pos)/tanh(K)`. Steep & responsive
//!                 near the neutral center, slope → 0 at the extremes. This is
//!                 the "后台中和 + 加权衰减" the user asked for.
//! - `LogSat`    : `raw = center·exp(pos·ln(half))` (center usually 1.0). Equal
//!                 *multiplicative* steps = equal perceived saturation change.

/// Perceptual curve kind for a single slider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveKind {
    /// Identity (raw == center + half·pos). For EV / hue degrees.
    Linear,
    /// tanh soft-knee: neutral center, diminishing returns toward extremes.
    SoftKnee,
    /// Logarithmic factor centered at `center` (e.g. saturation, 1.0 = identity).
    LogSat,
}

/// Soft-knee steepness. Higher = reaches full range sooner but still eases off
/// at the very ends. 2.2 gives a pleasant "tight in the middle, soft at edges".
pub const SOFT_K: f32 = 2.2;

/// Convert a uniform perceptual slider position into a raw pipeline value.
/// `pos` is in `[-1, 1]` (bipolar) or `[0, 1]` remapped to `[-1, 1]` by the
/// caller; `center` is the raw value at the slider's neutral point; `half` is
/// the raw half-range (bipolar) or full range (unipolar, from center).
#[inline]
pub fn slider_to_raw(curve: CurveKind, center: f32, half: f32, pos: f32) -> f32 {
    let p = pos.clamp(-1.0, 1.0);
    match curve {
        CurveKind::Linear => center + half * p,
        CurveKind::SoftKnee => center + half * (SOFT_K * p).tanh() / SOFT_K.tanh(),
        CurveKind::LogSat => {
            if center == 0.0 {
                0.0
            } else {
                center * (p * half.ln()).exp()
            }
        }
    }
}

/// Inverse of [`slider_to_raw`]: raw pipeline value → uniform slider position
/// in `[-1, 1]`. Used to position the slider when loading a preset / CLI value.
#[inline]
pub fn raw_to_slider(curve: CurveKind, center: f32, half: f32, raw: f32) -> f32 {
    match curve {
        CurveKind::Linear => ((raw - center) / half).clamp(-1.0, 1.0),
        CurveKind::SoftKnee => {
            let x = (((raw - center) / half).clamp(-1.0, 1.0)) * SOFT_K.tanh();
            (x.atanh() / SOFT_K).clamp(-1.0, 1.0)
        }
        CurveKind::LogSat => {
            if center == 0.0 {
                0.0
            } else {
                ((raw / center).ln() / half.ln()).clamp(-1.0, 1.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_identity_through_center() {
        for p in [-1.0, -0.3, 0.0, 0.3, 1.0] {
            let r = slider_to_raw(CurveKind::Linear, 0.0, 3.0, p);
            assert!((r - 3.0 * p).abs() < 1e-5);
            let back = raw_to_slider(CurveKind::Linear, 0.0, 3.0, r);
            assert!((back - p).abs() < 1e-5);
        }
    }

    #[test]
    fn softknee_is_neutral_at_center_and_eases_at_edges() {
        // center maps to center
        assert!(slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, 0.0).abs() < 1e-5);
        // symmetric
        let a = slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, 0.5);
        let b = slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, -0.5);
        assert!((a + b).abs() < 1e-5);
        // round-trip
        for p in [-1.0, -0.42, 0.0, 0.17, 0.9] {
            let r = slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, p);
            let back = raw_to_slider(CurveKind::SoftKnee, 0.0, 1.0, r);
            assert!((back - p).abs() < 1e-4, "softknee roundtrip {} -> {} -> {}", p, r, back);
        }
        // weighted decay: slope near center > slope near edge
        let slope_mid = slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, 0.1)
            - slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, 0.0);
        let slope_edge = slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, 1.0)
            - slider_to_raw(CurveKind::SoftKnee, 0.0, 1.0, 0.9);
        assert!(slope_mid > slope_edge, "soft-knee must decay at edges");
    }

    #[test]
    fn logsat_centered_at_identity() {
        // pos 0 -> center (1.0), pos 1 -> half (3.0), pos -1 -> 1/half (1/3)
        assert!((slider_to_raw(CurveKind::LogSat, 1.0, 3.0, 0.0) - 1.0).abs() < 1e-5);
        assert!((slider_to_raw(CurveKind::LogSat, 1.0, 3.0, 1.0) - 3.0).abs() < 1e-4);
        assert!((slider_to_raw(CurveKind::LogSat, 1.0, 3.0, -1.0) - 1.0 / 3.0).abs() < 1e-4);
        // round-trip
        for p in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            let r = slider_to_raw(CurveKind::LogSat, 1.0, 3.0, p);
            let back = raw_to_slider(CurveKind::LogSat, 1.0, 3.0, r);
            assert!((back - p).abs() < 1e-4);
        }
        // equal multiplicative step feels equal: 1->2 and 0.5->1 map to equal pos
        let dp1 = raw_to_slider(CurveKind::LogSat, 1.0, 3.0, 2.0)
            - raw_to_slider(CurveKind::LogSat, 1.0, 3.0, 1.0);
        let dp2 = raw_to_slider(CurveKind::LogSat, 1.0, 3.0, 1.0)
            - raw_to_slider(CurveKind::LogSat, 1.0, 3.0, 0.5);
        assert!((dp1 - dp2).abs() < 1e-4, "log-sat steps must be perceptually equal");
    }
}
