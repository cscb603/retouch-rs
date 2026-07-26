//! TOML preset support (M3).
//!
//! The preset schema deliberately mirrors **darktable-cli module param names**
//! so that `retouch_app` (which currently drives darktable-cli via XMP sidecars
//! keyed by darktable module names) can migrate to retouch-rs with a near
//! mechanical translation. Where retouch-rs has no 1:1 darktable twin, the
//! section keeps the closest conceptual darktable name.
//!
//! ```toml
//! [exposure]                 # darktable: exposure
//! ev = 0.0
//! [tone_map]                 # darktable: filmicrgb / sigmoid (our shoulder)
//! mode = "none"              # none | agx | filmic
//! [defake]                   # darktable: colorzones (chroma control, our anti-fake-color)
//! enabled = true
//! chroma_decay = 0.1
//! fix_sky = true
//! protect_skin = true
//! [grade]                    # darktable: shadhi + levels + toneequal micro-grade
//! brightness_lift = 0.06
//! contrast = 0.15
//! dehaze = 0.25
//! shadow_lift = 0.15
//! deep_shadow_lift = 0.15
//! [white_balance]            # darktable: channelmixerrgb (color calibration)
//! temperature = 0.0
//! tint = 0.0
//! [color]                    # darktable: vibrance / colisa / colorequal
//! saturation = 1.0
//! vibrance = 0.0
//! hue_rotate = 0.0
//! split_shadow = 0.0
//! split_highlight = 0.0
//! [hsl.blue]                 # darktable: colorzones HSL per region
//! hue = 0.0
//! sat = 1.4
//! light = 1.0
//! ```

use crate::advanced::{Advanced, FreqSepSkin, PyramidFusion};
use crate::detail::Detail;
use crate::geometry::Geometry;
use crate::pipeline::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Tone-map preset value, serialized as a lowercase string (`none`/`agx`/`filmic`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ToneMapPreset {
    #[default]
    None,
    Agx,
    Filmic,
}

impl From<ToneMapPreset> for ToneMapMode {
    fn from(t: ToneMapPreset) -> Self {
        match t {
            ToneMapPreset::None => ToneMapMode::None,
            ToneMapPreset::Agx => ToneMapMode::Agx,
            ToneMapPreset::Filmic => ToneMapMode::Filmic,
        }
    }
}

impl From<ToneMapMode> for ToneMapPreset {
    fn from(t: ToneMapMode) -> Self {
        match t {
            ToneMapMode::None => ToneMapPreset::None,
            ToneMapMode::Agx => ToneMapPreset::Agx,
            ToneMapMode::Filmic => ToneMapPreset::Filmic,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DefakePreset {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_chroma_decay")]
    pub chroma_decay: f32,
    #[serde(default = "default_true")]
    pub fix_sky: bool,
    #[serde(default = "default_true")]
    pub protect_skin: bool,
    #[serde(default = "default_true")]
    pub gamut_softclip: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GradePreset {
    #[serde(default)]
    pub brightness_lift: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub dehaze: f32,
    #[serde(default)]
    pub shadow_lift: f32,
    #[serde(default)]
    pub deep_shadow_lift: f32,
    /// 胶片感 S 曲线 (-0.25..0.35, 0 = 关)
    #[serde(default)]
    pub film_curve: f32,
    /// 光比融合 (-0.6..0.6, 0 = 关)
    #[serde(default)]
    pub light_ratio: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WhiteBalancePreset {
    #[serde(default)]
    pub temperature: f32,
    #[serde(default)]
    pub tint: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ColorPreset {
    #[serde(default = "default_one")]
    pub saturation: f32,
    #[serde(default)]
    pub vibrance: f32,
    #[serde(default)]
    pub hue_rotate: f32,
    #[serde(default)]
    pub split_shadow: f32,
    #[serde(default)]
    pub split_highlight: f32,
}

/// 粉嫩肤色预设 (M5). darktable 无 1:1 对应，置于 conceptual 模块。
/// 新增 high-level 控制：去黄 / 减淡 / 加红 / 加粉，后台解析为 OKLCH 目标。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkinPreset {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub strength: f32,
    /// 目标肤色色相 (OKLCH deg)，~25 = 粉嫩
    #[serde(default)]
    pub hue_target: f32,
    /// 目标肤色彩度
    #[serde(default)]
    pub chroma_target: f32,
    /// 肤色提亮
    #[serde(default)]
    pub light_lift: f32,
    /// 遮罩羽化 (deg)
    #[serde(default)]
    pub smoothness: f32,
    #[serde(default = "default_true")]
    pub protect_non_skin: bool,
    /// 去黄：0 = 不处理，1 = 最强
    #[serde(default)]
    pub yellow_reduce: f32,
    /// 减淡/提亮：0 = 不处理，1 = 最强
    #[serde(default)]
    pub lighten: f32,
    /// 加红：0 = 不处理，1 = 最强
    #[serde(default)]
    pub redden: f32,
    /// 加粉：0 = 不处理，1 = 最强
    #[serde(default)]
    pub pinken: f32,
}

impl Default for SkinPreset {
    /// Healthy 粉嫩 targets (disabled) so a preset that omits `[skin]` still
    /// yields a sensible skin when the user enables it.
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 0.5,
            hue_target: 25.0,
            chroma_target: 0.10,
            light_lift: 0.01,
            smoothness: 16.0,
            protect_non_skin: true,
            yellow_reduce: 0.0,
            lighten: 0.0,
            redden: 0.0,
            pinken: 0.0,
        }
    }
}

/// 多分区亮度融合预设 (M6)。darktable 对应 toneequal（亮度分区），但本工具用 4 区高斯融合。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ZonePreset {
    #[serde(default)]
    pub shadows: f32,
    #[serde(default)]
    pub dark_mid: f32,
    #[serde(default)]
    pub light_mid: f32,
    #[serde(default)]
    pub highlights: f32,
}

/// One ACR hue-band entry under `[hsl.<band>]`. Unspecified fields default to
/// identity (hue=0, sat=1, light=1) so a band can tweak only what it needs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HslBand {
    #[serde(default)]
    pub hue: f32,
    #[serde(default = "default_one")]
    pub sat: f32,
    #[serde(default = "default_one")]
    pub light: f32,
}

fn default_one() -> f32 {
    1.0
}
fn default_chroma_decay() -> f32 {
    0.1
}
fn default_true() -> bool {
    true
}

/// Exposure section (darktable: `exposure` module, param `ev`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExposurePreset {
    #[serde(default)]
    pub ev: f32,
}

/// Tone-map section (darktable: `filmicrgb` / `sigmoid` shoulder modules).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToneMapSection {
    #[serde(default)]
    pub mode: ToneMapPreset,
}

/// 几何预处理预设 (M4b). darktable 无 1:1 对应（其几何在导入阶段用 crop/rotate）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GeometryPreset {
    /// 归一化裁剪 `(x, y, w, h)` 在 0..1。None = 不裁剪。
    #[serde(default)]
    pub crop: Option<(f32, f32, f32, f32)>,
    /// 粗旋转 90° 步进（顺时针），0..3。
    #[serde(default)]
    pub quarter_turns: u8,
    /// 微调旋转角度（逆时针，度），用于摆正地平线。
    #[serde(default)]
    pub rotate_deg: f32,
    /// 水平翻转。
    #[serde(default)]
    pub flip_h: bool,
    /// 垂直翻转。
    #[serde(default)]
    pub flip_v: bool,
    /// 透视 / 梯形校正 `(v_key, h_key)` 在 -1..1。None = 无。
    #[serde(default)]
    pub perspective: Option<(f32, f32)>,
}

/// 细节后处理预设 (M5)。darktable 对应：降噪 / 锐化 / 柔光模块。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetailPreset {
    #[serde(default)]
    pub denoise: f32,
    #[serde(default)]
    pub sharpen: f32,
    #[serde(default)]
    pub diffuse: f32,
}

/// 高级修图预设 (原 M6)。darktable 对应：频谱磨皮（无 1:1）/ 拉普拉斯细节。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdvancedPreset {
    #[serde(default)]
    pub freqsep_enabled: bool,
    #[serde(default)]
    pub freqsep_strength: f32,
    #[serde(default)]
    pub freqsep_texture_keep: f32,
    #[serde(default)]
    pub freqsep_smoothness: f32,
    #[serde(default)]
    pub freqsep_mask_feather: f32,
    #[serde(default)]
    pub pyramid_enabled: bool,
    #[serde(default)]
    pub pyramid_strength: f32,
    #[serde(default)]
    pub pyramid_detail_scale: f32,
}

/// A full retouch-rs preset. Mirrors `Adjustments` but with darktable-flavored
/// section/key names and `serde` (de)serialization. Each section name matches a
/// darktable module; keys match that module's params where a 1:1 exists.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Preset {
    #[serde(default)]
    pub exposure: ExposurePreset,
    #[serde(default)]
    pub tone_map: ToneMapSection,
    #[serde(default)]
    pub defake: DefakePreset,
    #[serde(default)]
    pub grade: GradePreset,
    #[serde(default)]
    pub white_balance: WhiteBalancePreset,
    #[serde(default)]
    pub color: ColorPreset,
    /// 粉嫩肤色模块 (M5)
    #[serde(default)]
    pub skin: SkinPreset,
    /// 多分区亮度融合 (M6)
    #[serde(default)]
    pub zones: ZonePreset,
    /// 几何预处理 (M4b)
    #[serde(default)]
    pub geometry: GeometryPreset,
    /// 细节后处理 (M5)
    #[serde(default)]
    pub detail: DetailPreset,
    /// 高级修图 (原 M6)
    #[serde(default)]
    pub advanced: AdvancedPreset,
    /// Per-hue-region HSL overrides, keyed by band name. Empty = identity.
    #[serde(default)]
    pub hsl: HashMap<String, HslBand>,
    /// 整体效果混合比例 (0..1, 默认 1.0)
    #[serde(default = "default_mix")]
    pub mix: f32,
}

fn default_mix() -> f32 {
    1.0
}

impl Preset {
    /// Load a preset from a TOML file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let txt = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read preset {}: {}", path.display(), e))?;
        toml::from_str(&txt).map_err(|e| format!("invalid preset {}: {}", path.display(), e))
    }

    /// Convert to a render `Adjustments`. Unknown HSL band names are warned and
    /// skipped (never silently produce an identity result for valid bands).
    pub fn to_adjustments(&self) -> Adjustments {
        let mut hsl = HslRegions::default();
        for (name, b) in &self.hsl {
            match HslRegions::band_index(name) {
                Some(i) => {
                    hsl.hue_shift[i] = b.hue;
                    hsl.sat_mult[i] = b.sat;
                    hsl.light_mult[i] = b.light;
                }
                None => eprintln!(
                    "warning: preset hsl band '{}' unknown (expected red|orange|yellow|green|aqua|blue|purple|magenta), ignored",
                    name
                ),
            }
        }
        Adjustments {
            exposure_ev: self.exposure.ev,
            tone_map: self.tone_map.mode.into(),
            defake: DefakeColor {
                enabled: self.defake.enabled,
                chroma_decay: self.defake.chroma_decay,
                fix_sky: self.defake.fix_sky,
                protect_skin: self.defake.protect_skin,
                gamut_softclip: self.defake.gamut_softclip,
            },
            grade: Grade {
                brightness_lift: self.grade.brightness_lift,
                contrast: self.grade.contrast,
                dehaze: self.grade.dehaze,
                shadow_lift: self.grade.shadow_lift,
                deep_shadow_lift: self.grade.deep_shadow_lift,
                film_curve: self.grade.film_curve,
                light_ratio: self.grade.light_ratio,
            },
            white_balance: WhiteBalance {
                temp: self.white_balance.temperature,
                tint: self.white_balance.tint,
            },
            color: ColorGrade {
                saturation: self.color.saturation,
                vibrance: self.color.vibrance,
                hue_rotate: self.color.hue_rotate,
                split_shadow: self.color.split_shadow,
                split_highlight: self.color.split_highlight,
            },
            skin: SkinTone {
                enabled: self.skin.enabled,
                strength: self.skin.strength,
                hue_target: self.skin.hue_target,
                chroma_target: self.skin.chroma_target,
                light_lift: self.skin.light_lift,
                smoothness: self.skin.smoothness,
                protect_non_skin: self.skin.protect_non_skin,
                yellow_reduce: self.skin.yellow_reduce,
                lighten: self.skin.lighten,
                redden: self.skin.redden,
                pinken: self.skin.pinken,
            },
            zones: ZoneGrade {
                lift: [
                    self.zones.shadows,
                    self.zones.dark_mid,
                    self.zones.light_mid,
                    self.zones.highlights,
                ],
            },
            geometry: Geometry {
                crop: self.geometry.crop,
                quarter_turns: self.geometry.quarter_turns,
                rotate_deg: self.geometry.rotate_deg,
                flip_h: self.geometry.flip_h,
                flip_v: self.geometry.flip_v,
                perspective: self.geometry.perspective,
            },
            detail: Detail {
                denoise: self.detail.denoise,
                sharpen: self.detail.sharpen,
                diffuse: self.detail.diffuse,
            },
            advanced: Advanced {
                freqsep: FreqSepSkin {
                    enabled: self.advanced.freqsep_enabled,
                    strength: self.advanced.freqsep_strength,
                    texture_keep: self.advanced.freqsep_texture_keep,
                    smoothness: self.advanced.freqsep_smoothness,
                    mask_feather: self.advanced.freqsep_mask_feather,
                },
                pyramid: PyramidFusion {
                    enabled: self.advanced.pyramid_enabled,
                    strength: self.advanced.pyramid_strength,
                    detail_scale: self.advanced.pyramid_detail_scale,
                },
            },
            hsl,
            color_plan: None,
            mix: self.mix,
        }
    }
}

impl Adjustments {
    /// Serialize the current adjustments back into a `Preset` (for the `Dump`
    /// CLI and the GUI "export settings" action).
    pub fn to_preset(&self) -> Preset {
        let mut hsl = HashMap::new();
        let names = [
            "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
        ];
        for (i, name) in names.iter().enumerate() {
            if self.hsl.hue_shift[i] != 0.0
                || self.hsl.sat_mult[i] != 1.0
                || self.hsl.light_mult[i] != 1.0
            {
                hsl.insert(
                    name.to_string(),
                    HslBand {
                        hue: self.hsl.hue_shift[i],
                        sat: self.hsl.sat_mult[i],
                        light: self.hsl.light_mult[i],
                    },
                );
            }
        }
        Preset {
            exposure: ExposurePreset {
                ev: self.exposure_ev,
            },
            tone_map: ToneMapSection {
                mode: self.tone_map.into(),
            },
            defake: DefakePreset {
                enabled: self.defake.enabled,
                chroma_decay: self.defake.chroma_decay,
                fix_sky: self.defake.fix_sky,
                protect_skin: self.defake.protect_skin,
                gamut_softclip: self.defake.gamut_softclip,
            },
            grade: GradePreset {
                brightness_lift: self.grade.brightness_lift,
                contrast: self.grade.contrast,
                dehaze: self.grade.dehaze,
                shadow_lift: self.grade.shadow_lift,
                deep_shadow_lift: self.grade.deep_shadow_lift,
                film_curve: self.grade.film_curve,
                light_ratio: self.grade.light_ratio,
            },
            white_balance: WhiteBalancePreset {
                temperature: self.white_balance.temp,
                tint: self.white_balance.tint,
            },
            color: ColorPreset {
                saturation: self.color.saturation,
                vibrance: self.color.vibrance,
                hue_rotate: self.color.hue_rotate,
                split_shadow: self.color.split_shadow,
                split_highlight: self.color.split_highlight,
            },
            skin: SkinPreset {
                enabled: self.skin.enabled,
                strength: self.skin.strength,
                hue_target: self.skin.hue_target,
                chroma_target: self.skin.chroma_target,
                light_lift: self.skin.light_lift,
                smoothness: self.skin.smoothness,
                protect_non_skin: self.skin.protect_non_skin,
                yellow_reduce: self.skin.yellow_reduce,
                lighten: self.skin.lighten,
                redden: self.skin.redden,
                pinken: self.skin.pinken,
            },
            zones: ZonePreset {
                shadows: self.zones.lift[0],
                dark_mid: self.zones.lift[1],
                light_mid: self.zones.lift[2],
                highlights: self.zones.lift[3],
            },
            geometry: GeometryPreset {
                crop: self.geometry.crop,
                quarter_turns: self.geometry.quarter_turns,
                rotate_deg: self.geometry.rotate_deg,
                flip_h: self.geometry.flip_h,
                flip_v: self.geometry.flip_v,
                perspective: self.geometry.perspective,
            },
            detail: DetailPreset {
                denoise: self.detail.denoise,
                sharpen: self.detail.sharpen,
                diffuse: self.detail.diffuse,
            },
            advanced: AdvancedPreset {
                freqsep_enabled: self.advanced.freqsep.enabled,
                freqsep_strength: self.advanced.freqsep.strength,
                freqsep_texture_keep: self.advanced.freqsep.texture_keep,
                freqsep_smoothness: self.advanced.freqsep.smoothness,
                freqsep_mask_feather: self.advanced.freqsep.mask_feather,
                pyramid_enabled: self.advanced.pyramid.enabled,
                pyramid_strength: self.advanced.pyramid.strength,
                pyramid_detail_scale: self.advanced.pyramid.detail_scale,
            },
            hsl,
            mix: self.mix,
        }
    }
}

/// Write a preset to a TOML file (pretty-printed).
pub fn dump_preset(preset: &Preset, path: &Path) -> Result<(), String> {
    let txt =
        toml::to_string_pretty(preset).map_err(|e| format!("cannot serialize preset: {}", e))?;
    std::fs::write(path, txt).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

/// Convenience: load a preset file.
pub fn load_preset(path: &Path) -> Result<Preset, String> {
    Preset::load(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Preset -> Adjustments -> Preset must round-trip (a Dump of a Dump is
    /// stable), proving the schema is lossless and the migration surface works.
    #[test]
    fn preset_roundtrip_lossless() {
        let pre = Preset {
            exposure: ExposurePreset { ev: 0.2 },
            tone_map: ToneMapSection {
                mode: ToneMapPreset::Agx,
            },
            defake: DefakePreset {
                enabled: true,
                chroma_decay: 0.42,
                fix_sky: true,
                protect_skin: false,
                gamut_softclip: true,
            },
            grade: GradePreset {
                brightness_lift: 0.06,
                contrast: 0.15,
                dehaze: 0.25,
                shadow_lift: 0.1,
                deep_shadow_lift: 0.1,
                film_curve: 0.0,
                light_ratio: 0.0,
            },
            white_balance: WhiteBalancePreset {
                temperature: 0.3,
                tint: -0.1,
            },
            color: ColorPreset {
                saturation: 1.2,
                vibrance: 0.3,
                hue_rotate: 10.0,
                split_shadow: 20.0,
                split_highlight: -10.0,
            },
            skin: SkinPreset::default(),
            zones: ZonePreset::default(),
            geometry: GeometryPreset {
                crop: Some((0.1, 0.1, 0.8, 0.8)),
                quarter_turns: 1,
                rotate_deg: 90.0,
                flip_h: true,
                flip_v: false,
                perspective: Some((0.2, -0.1)),
            },
            detail: DetailPreset {
                denoise: 0.4,
                sharpen: 0.5,
                diffuse: 0.3,
            },
            advanced: AdvancedPreset {
                freqsep_enabled: true,
                freqsep_strength: 0.6,
                freqsep_texture_keep: 0.7,
                freqsep_smoothness: 0.4,
                freqsep_mask_feather: 0.5,
                pyramid_enabled: true,
                pyramid_strength: 0.5,
                pyramid_detail_scale: 1.2,
            },
            hsl: {
                let mut m = HashMap::new();
                m.insert(
                    "blue".to_string(),
                    HslBand {
                        hue: 0.0,
                        sat: 1.4,
                        light: 1.0,
                    },
                );
                m.insert(
                    "red".to_string(),
                    HslBand {
                        hue: 12.0,
                        sat: 1.1,
                        light: 0.95,
                    },
                );
                m
            },
            mix: 1.0,
        };
        let adj = pre.to_adjustments();
        let back = adj.to_preset();
        // Re-derive adjustments and compare field-by-field.
        let adj2 = back.to_adjustments();
        assert!((adj.exposure_ev - adj2.exposure_ev).abs() < 1e-6);
        assert_eq!(adj.tone_map, adj2.tone_map);
        assert_eq!(adj.defake, adj2.defake);
        assert!((adj.grade.contrast - adj2.grade.contrast).abs() < 1e-6);
        assert!((adj.white_balance.temp - adj2.white_balance.temp).abs() < 1e-6);
        assert!((adj.color.saturation - adj2.color.saturation).abs() < 1e-6);
        assert!((adj.color.hue_rotate - adj2.color.hue_rotate).abs() < 1e-6);
        // HSL bands preserved.
        assert!((adj.hsl.hue_shift[5] - 0.0).abs() < 1e-6); // blue hue
        assert!((adj.hsl.sat_mult[5] - 1.4).abs() < 1e-6); // blue sat
        assert!((adj.hsl.hue_shift[0] - 12.0).abs() < 1e-6); // red hue
        assert!((adj.hsl.light_mult[0] - 0.95).abs() < 1e-6); // red light
                                                              // geometry / detail / advanced preserved through the round-trip.
        assert_eq!(adj.geometry.rotate_deg, 90.0);
        assert_eq!(adj.geometry.quarter_turns, 1);
        assert!(adj.geometry.flip_h);
        assert_eq!(adj.geometry.crop, Some((0.1, 0.1, 0.8, 0.8)));
        assert_eq!(adj.geometry.perspective, Some((0.2, -0.1)));
        assert!((adj.detail.denoise - 0.4).abs() < 1e-6);
        assert!((adj.detail.diffuse - 0.3).abs() < 1e-6);
        assert!(adj.advanced.freqsep.enabled);
        assert!((adj.advanced.freqsep.strength - 0.6).abs() < 1e-6);
        assert!((adj.advanced.freqsep.texture_keep - 0.7).abs() < 1e-6);
        assert!(adj.advanced.pyramid.enabled);
        assert!((adj.advanced.pyramid.strength - 0.5).abs() < 1e-6);
        assert!((adj.advanced.pyramid.detail_scale - 1.2).abs() < 1e-6);
    }

    /// `Adjustments::default()` (no preset, no flags) must serialize to an
    /// all-identity preset so `Dump` of an empty render is clean.
    #[test]
    fn default_preset_is_identity() {
        let pre = Adjustments::default().to_preset();
        assert_eq!(pre.exposure.ev, 0.0);
        assert_eq!(pre.tone_map.mode, ToneMapPreset::None);
        assert!(!pre.defake.enabled);
        assert!(pre.hsl.is_empty());
        let adj = pre.to_adjustments();
        assert_eq!(adj, Adjustments::default());
    }
}
