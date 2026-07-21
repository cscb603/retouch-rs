//! AI-facing parameter schema.
//!
//! The whole point: an agent must be able to *read* what it can control and
//! *how* (ranges, units, plain-language meaning) without anyone hand-maintaining
//! a doc. We already have a single source of truth — `params::registry()` — so
//! the schema is generated from it. Change a slider's range in `params.rs` and
//! the AI sees it next run, automatically.
//!
//! This is strictly better than retouch_app's hand-written `HARD_BOUNDS`
//! dictionary (which drifted from the actual darktable modules). Here the
//! bounds *are* the slider bounds, so there is no second copy to desync.

use crate::params::{registry, Field, ParamSpec};
use crate::perceptual::CurveKind;
use serde::Serialize;

#[derive(Serialize, Debug, Clone)]
pub struct ParamSchemaEntry {
    /// Stable id — the JSON key an agent writes in a `--params` patch.
    pub id: String,
    /// Human label (Chinese), as shown in the GUI.
    pub label: String,
    /// One-sentence, friend-style explanation (the hover tooltip).
    pub description: String,
    /// Logical group (exposure / color / skin / geometry / detail / ...).
    pub group: String,
    /// Minimum raw value the slider allows (what `Field::set` accepts).
    pub min: f32,
    /// Maximum raw value the slider allows.
    pub max: f32,
    /// Default / neutral raw value (slider center).
    pub default: f32,
    /// Unit suffix for the value (e.g. "EV", "×", "°", "").
    pub unit: String,
    /// Perceptual curve kind (how the slider position maps to raw value).
    pub curve: String,
    /// True if neutral is at the midpoint (e.g. temperature, tint).
    pub bipolar: bool,
}

fn group_of(f: &Field) -> &'static str {
    match f {
        Field::ExposureEv => "exposure",
        Field::FilmCurve | Field::LightRatio | Field::BrightnessLift | Field::Contrast
        | Field::Dehaze | Field::ShadowLift | Field::DeepShadowLift => "tone",
        Field::WBTemp | Field::WBTint => "white_balance",
        Field::Saturation | Field::Vibrance | Field::HueRotate | Field::SplitShadow
        | Field::SplitHighlight => "color",
        Field::SkinStrength | Field::SkinHue | Field::SkinChroma | Field::SkinLight
        | Field::SkinYellowReduce | Field::SkinLighten | Field::SkinRedden | Field::SkinPinken => {
            "skin"
        }
        Field::HslHue(_) | Field::HslSat(_) | Field::HslLight(_) => "hsl",
        Field::Zone(_) => "zone",
        Field::GeomRotate | Field::GeomPerspV | Field::GeomPerspH => "geometry",
        Field::DetailDenoise | Field::DetailSharpen | Field::DetailDiffuse => "detail",
        Field::FreqSepStrength | Field::FreqSepTexture | Field::FreqSepSmooth
        | Field::FreqSepFeather | Field::PyramidStrength | Field::PyramidScale => "advanced",
    }
}

fn curve_name(c: CurveKind) -> &'static str {
    match c {
        CurveKind::Linear => "linear",
        CurveKind::SoftKnee => "soft_knee",
        CurveKind::LogSat => "log_sat",
    }
}

/// Build the full AI-readable parameter schema from the live registry.
pub fn param_schema() -> Vec<ParamSchemaEntry> {
    registry()
        .into_iter()
        .map(|spec: ParamSpec| {
            let min = spec.to_raw(-1.0);
            let max = spec.to_raw(1.0);
            let default = spec.to_raw(0.0);
            ParamSchemaEntry {
                id: spec.field.id(),
                label: spec.label.clone(),
                description: spec.tooltip.to_string(),
                group: group_of(&spec.field).to_string(),
                min,
                max,
                default,
                unit: spec.unit.to_string(),
                curve: curve_name(spec.curve).to_string(),
                bipolar: spec.bipolar,
            }
        })
        .collect()
}
