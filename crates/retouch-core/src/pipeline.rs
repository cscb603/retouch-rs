//! Core rendering pipeline for retouch-rs.
//!
//! M0 scope: prove the non-linear OKLCH architecture round-trips JPG/TIFF
//! with no perceivable color shift.
//! M1 scope: exposure (linear multiplier) + AgX/Filmic tone map (shoulder
//! compression that prevents highlight fake-color when pushing exposure).
//! M2a scope: de-fake-color (always-on by default) — luminance-linked chroma
//! decay + sky/skin hue constraints + self-developed gamut soft-clip.
//!
//! Pipeline (per pixel):
//!   sRGB u8  ->  linear f32  ->  exposure  ->  tone map (AgX/Filmic)  ->
//!   OKLCH(L,C,H)  ->  [de-fake-color grade]  ->  linear f32
//!   ->  [gamut soft-clip]  ->  sRGB u8
//!
//! Gamma encode/decode is done manually (simple, dependency-free, exact),
//! palette handles the OKLCH rotation, zentone handles tone mapping, and the
//! gamut soft-clip is self-developed (bisection on chroma; esoc-color, which
//! the design originally referenced, does not exist).

use image::{DynamicImage, RgbImage};
use palette::{IntoColor, LinSrgb, OklabHue, Oklch};
use zentone::{AgxLook, ToneMap, ToneMapCurve};

use crate::advanced::{apply_advanced, Advanced, FreqSepSkin};
use crate::color_engine::{apply_color_correction, ColorPlan};
use crate::detail::{apply_detail, Detail};
use crate::geometry::{apply_geometry, Geometry};

/// Tone-map / shoulder-compression mode applied in the linear-light stage,
/// right after exposure. This is what replaces the traditional "highlights"
/// slider that, on linear RGB, produces fake cyan/blue highlights.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ToneMapMode {
    #[default]
    None,
    Agx,
    Filmic,
}

impl ToneMapMode {
    fn curve(self) -> Option<ToneMapCurve> {
        match self {
            ToneMapMode::None => None,
            ToneMapMode::Agx => Some(ToneMapCurve::Agx(AgxLook::Default)),
            ToneMapMode::Filmic => Some(ToneMapCurve::HableFilmic),
        }
    }
}

/// De-fake-color parameters (M2a). This is the tool's core differentiator and
/// is **on by default** in the product profile (`Adjustments::photo_default`),
/// but **off** in `Adjustments::default()`/`identity()` so the M0/M1 identity
/// round-trip property is preserved for tests and pure passthrough.
///
/// All operations happen in OKLCH where L/C/H are decoupled, so reducing
/// chroma never shifts hue — the root cause of darktable/PS fake color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DefakeColor {
    /// Master switch. When false the whole stage is skipped (identity).
    pub enabled: bool,
    /// Luminance-linked chroma decay strength (0..1, ~0.1 is a gentle default).
    /// Desaturates over-bright highlights and crushes shadow color-noise.
    pub chroma_decay: f32,
    /// Constrain sky hue (H in [210,240] deg): cap chroma to kill neon blue.
    pub fix_sky: bool,
    /// Protect skin hue (H in [20,45] deg): cap chroma to avoid magenta/wax.
    pub protect_skin: bool,
    /// Gamut soft-clip: when OKLCH maps out of sRGB, reduce C (not truncate).
    pub gamut_softclip: bool,
}

impl Default for DefakeColor {
    /// Default is **off** to preserve identity semantics. Use
    /// `DefakeColor::on()` (or `Adjustments::photo_default`) for the product
    /// always-on behavior.
    fn default() -> Self {
        Self {
            enabled: false,
            chroma_decay: 0.1,
            fix_sky: true,
            protect_skin: true,
            gamut_softclip: true,
        }
    }
}

impl DefakeColor {
    /// The always-on product default: visible-but-safe chroma decay + sky/skin
    /// caps + gamut soft-clip. chroma_decay 0.3 is strong enough that toggling
    /// 去假色 produces an obvious reduction of over-saturated highlights/fringes.
    pub fn on() -> Self {
        Self {
            enabled: true,
            chroma_decay: 0.3,
            ..Self::default()
        }
    }
}

/// 粉嫩肤色模块 (M5). A dedicated skin-tone beautification that detects skin
/// pixels in OKLCH (a hue-gaussian × chroma-gate × lightness-gate probability)
/// and gently pulls them toward a healthy target. The high-level controls
/// （去黄 / 减淡 / 加红 / 加粉） are combined into the underlying OKLCH target
/// so the user doesn't need to understand hue degrees. Non-skin pixels are left
/// fully intact.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkinTone {
    /// Master switch. When false the whole stage is skipped (identity).
    pub enabled: bool,
    /// Overall strength 0..1 (blend between original skin and target skin).
    pub strength: f32,
    /// OKLCH hue target derived from high-level controls. 0..360, but internally
    /// set by `yellow_reduce` / `pinken`.
    pub hue_target: f32,
    /// OKLCH chroma target derived from high-level controls.
    pub chroma_target: f32,
    /// Extra lightness lift for skin pixels derived from high-level controls.
    pub light_lift: f32,
    /// Mask feathering: hue-gaussian sigma (deg). Wider = more pixels treated
    /// as skin. ~16° is a good default.
    pub smoothness: f32,
    /// If true, only pixels with skin probability above a small threshold are
    /// touched; everything else is fully preserved.
    pub protect_non_skin: bool,
    /// 去黄：shift skin hue away from yellow toward pink/red. 0 = off, 1 = strong.
    pub yellow_reduce: f32,
    /// 减淡/提亮：subtle skin-only brightening. 0 = off, 1 = strong.
    pub lighten: f32,
    /// 加红：boost red/pink chroma. 0 = off, 1 = strong.
    pub redden: f32,
    /// 加粉：shift skin hue toward pink and boost pink chroma. 0 = off, 1 = strong.
    pub pinken: f32,
}

impl Default for SkinTone {
    /// Off by default (preserves identity / the validated factory look), but
    /// carries *healthy* 粉嫩 targets so that simply enabling it (GUI checkbox
    /// or `--skin`) yields a good look instead of zeroed/desaturated skin.
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 0.5,
            hue_target: 30.0,
            chroma_target: 0.07,
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

impl SkinTone {
    /// A ready-to-use "粉嫩" (pink, healthy) preset.
    pub fn pink() -> Self {
        Self {
            enabled: true,
            strength: 0.5,
            hue_target: 30.0,
            chroma_target: 0.075,
            light_lift: 0.015,
            smoothness: 16.0,
            protect_non_skin: true,
            yellow_reduce: 0.0,
            lighten: 0.0,
            redden: 0.0,
            pinken: 0.35,
        }
    }

    /// Resolve the effective OKLCH target from the high-level friend controls.
    /// Base is a healthy pink; user sliders push it toward:
    ///   - 去黄 (yellow_reduce): reduces chroma + gentle hue shift away from
    ///     yellow (to neutral/pink), NOT a pure hue rotation toward red.
    ///   - 减淡 (lighten): lightness lift.
    ///   - 加红 (redden): boost chroma.
    ///   - 加粉 (pinken): shift hue toward pink + boost chroma.
    pub fn resolved_targets(&self) -> (f32, f32, f32) {
        // 健康肤色基准色相 ~30°（粉嫩但不偏红；低于 ~25° 在黄种人皮肤上易显红/洋红）。
        // 去黄 / 加粉 只做极小色相偏移，主效果是「降彩度去黄」或「轻提粉彩度」，
        // 绝不把色相向红端大幅旋转（那正是旧版「皮肤变红」的根因）。
        let base_hue = 30.0;
        let hue = base_hue + self.pinken * 6.0 - self.yellow_reduce * 4.0;
        // 彩度严格封顶 ≤0.09：人眼记忆中的健康肤色彩度本就很低（OKLCH C≈0.05–0.09），
        // 旧版 0.10+ 在暖黄皮肤上过饱和 → 显红。去黄降彩度、加粉轻提、加红极轻。
        let chroma = (self.chroma_target + self.redden * 0.025 + self.pinken * 0.015
            - self.yellow_reduce * 0.03)
            .clamp(0.03, 0.09);
        let lift = self.light_lift + self.lighten * 0.05;
        (hue.rem_euclid(360.0), chroma, lift)
    }
}

/// Skin-tone probability in OKLCH (0..1). Combines a hue gaussian centered on
/// the skin band with chroma / lightness gates so saturated objects (a red
/// ball) and neutrals (a gray wall) are NOT mistaken for skin.
///
/// 关键修正（按色彩科学 + 用户实测）：
/// - 指甲 / 眼白 / 牙齿 / 高光：亮度高且彩度极低 → 直接判为非皮肤，
///   否则会被误当皮肤「加粉」而发灰发白（旧版「指甲发灰」根因）。
/// - 肤色亮度带收紧到 0.18..0.85：高于此多为高光/亮部，不该美颜。
#[inline]
fn skin_probability(ok: &Oklch<f32>, smooth: f32) -> f32 {
    let h = ok.hue.into_positive_degrees();
    let mut dh = (h - 33.0).abs();
    if dh > 180.0 {
        dh = 360.0 - dh;
    }
    let hue_w = (-(dh * dh) / (2.0 * smooth * smooth)).exp(); // gaussian on hue
    let c = ok.chroma;
    // 指甲 / 眼白 / 高光：高亮 + 低彩度 → 非皮肤（避免被美颜发灰）
    if ok.l > 0.80 && c < 0.07 {
        return 0.0;
    }
    // Chroma gate: skin has moderate chroma. Neutrals (c~0) are not skin; very
    // saturated pixels are probably an object, so their weight tapers off.
    let chroma_w = if c < 0.005 {
        0.0
    } else if c > 0.20 {
        (0.20 / c).clamp(0.0, 1.0)
    } else {
        let up = smoothstep(0.005, 0.05, c);
        let down = 1.0 - smoothstep(0.13, 0.20, c);
        (up * down).clamp(0.0, 1.0)
    };
    // Lightness gate: 收紧到 flesh 带 0.18..0.85（高于此多为高光/亮部）
    let l = ok.l;
    let l_w = smoothstep(0.18, 0.28, l) * (1.0 - smoothstep(0.82, 0.90, l));
    (hue_w * chroma_w * l_w).clamp(0.0, 1.0)
}

/// Apply the skin-tone beautification in OKLCH. Hue is lerped along the shortest
/// arc; chroma is lerped toward the target (only raises toward healthy, never
/// introduces fake color); lightness gets a subtle optional lift.
///
/// `orig` = 原图同像素 OKLCH，用于阴影检测：皮肤区域深影（orig.L < 0.35）时
/// 做极轻微淡化（+0.02 以内），让浓阴影下的皮肤更通透自然，而非死黑/暗红。
#[inline]
fn apply_skin_tone(oklch: &mut Oklch<f32>, st: &SkinTone, orig: &Oklch<f32>) {
    if !st.enabled || st.strength <= 0.0 {
        return;
    }
    let p = skin_probability(oklch, st.smoothness);
    if p <= 0.0 {
        return;
    }
    if st.protect_non_skin && p < 0.15 {
        return;
    }
    let w = (st.strength * p).clamp(0.0, 1.0);
    let (hue_target, chroma_target, light_lift) = st.resolved_targets();
    // hue: shortest-path lerp toward target
    let h = oklch.hue.into_positive_degrees();
    let mut dh = hue_target - h;
    if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }
    let new_h = (h + dh * w).rem_euclid(360.0);
    oklch.hue = OklabHue::from_degrees(new_h);
    // chroma: lerp toward target (healthy, not over-saturated)
    oklch.chroma = (oklch.chroma + (chroma_target - oklch.chroma) * w).max(0.0);
    // lightness: subtle lift + 阴影淡化
    let mut lift = light_lift;
    // 阴影柔和化：皮肤区域且原图 L<0.35 → 极轻微提亮（上限 0.025），
    // 让浓影部皮肤自然通透，而不是死黑或暗红。仅作用于高皮肤概率像素。
    if p > 0.4 && orig.l < 0.35 {
        let shadow_soften = (0.35 - orig.l) * 0.08 * st.strength;
        lift += shadow_soften.min(0.025); // 硬上限，绝不提过头
    }
    if lift > 0.0 {
        oklch.l = (oklch.l + lift * w).clamp(0.0, 1.0);
    }
}

/// 多分区融合 (multi-zone luminance fusion, M6). Per-luminance-zone lightness
/// offsets, blended smoothly by a gaussian weight on OKLCH `L` (no hard seams
/// between zones — this is the "融合" the user asked for). A tone-equalizer
/// style control: lift shadows / mids / highs independently and coherently.
/// All-zero `lift` = identity (strict no-op).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneGrade {
    /// Lightness offset per zone: [暗部, 阴影, 中间调, 高光]. 0 = unchanged.
    /// Positive lifts that luminance region.
    pub lift: [f32; 4],
}

impl Default for ZoneGrade {
    fn default() -> Self {
        Self { lift: [0.0; 4] }
    }
}

/// Zone centers in OKLCH L (0..1): deep shadow, mid shadow, mid highlight, high.
const ZONE_CENTERS: [f32; 4] = [0.12, 0.32, 0.6, 0.85];
/// Gaussian sigma (in L units) for the smooth fusion between zones. Wider =
/// more overlap between adjacent zones, so the boundary between e.g. shadows
/// and mids is a gentle ramp rather than a visible step. 0.34 is the sweet
/// spot after the "边界羽化更自然" pass: overlapping enough that no single
/// pixel is dominated by just one zone.
const ZONE_WIDTH: f32 = 0.34;

/// Raw per-pixel zone lift (single-scale). Used as the input to the multi-scale
/// fusion below; the returned delta is then blurred at multiple scales so the
/// final adjustment is edge-aware and natural.
#[inline]
fn zone_delta(l: f32, z: &ZoneGrade) -> f32 {
    if z.lift == [0.0; 4] {
        return 0.0;
    }
    let mut wsum = 0.0;
    let mut delta = 0.0;
    for i in 0..4 {
        let d = (l - ZONE_CENTERS[i]) / ZONE_WIDTH;
        let w = (-0.5 * d * d).exp(); // gaussian weight
        wsum += w;
        delta += w * z.lift[i];
    }
    if wsum > 0.0 {
        delta /= wsum; // normalize so the result stays within the max lift
    }
    delta
}

/// Lightness-only grade (M2b micro-adjust preview). Operates on the OKLCH
/// `L` channel ONLY — hue and chroma are never touched, so this can reshape
/// contrast / depth / brightness without ever introducing fake color.
///
/// All fields default to 0.0 (identity). Positive values push toward more
/// punch; `brightness_lift` is deliberately a *soft* lift (highlights roll off
/// instead of blowing out) per the user's "柔和过渡" requirement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Grade {
    /// Soft overall brightness lift in EV-ish units (e.g. 0.3). Uses
    /// `L' = 1 - (1-L)·2^(-k)` so highlights are preserved (no blow-out).
    pub brightness_lift: f32,
    /// Global contrast around mid-gray (0.5). Linear gain `(1+contrast)`.
    pub contrast: f32,
    /// Mid-tone local-contrast boost ("dehaze / clarity"): increases separation
    /// near 0.5 without shifting the mean — adds 层次感, not brightness.
    pub dehaze: f32,
    /// Shadow recovery: lifts ONLY the dark end (L < 0.30) to fix "dead-black"
    /// crush from contrast/dehaze, while midtones and highlights are untouched.
    pub shadow_lift: f32,
    /// Deep-shadow (Blacks) recovery: lifts ONLY the very darkest end (L < 0.15)
    /// on top of `shadow_lift`, so the deepest blacks keep a little detail while
    /// the mid-darks stay at the gentler `shadow_lift` level — a natural roll-off.
    pub deep_shadow_lift: f32,
    /// 胶片感过渡 (film-like transition): a smooth sine S-curve with a flat
    /// toe + shoulder. `> 0` opens film contrast (shadows deepen, highlights
    /// bloom); `< 0` flattens (dreamy). The sine form makes the slope → 0 at
    /// both ends, so it is a gentle filmic roll-off, never a harsh clip. This is
    /// the "减少纯线性生硬感" control. 0 = off.
    pub film_curve: f32,
    /// 光比控制融合 (light-ratio fusion): ONE perceptual control that fuses
    /// shadow-lift + midtone-separation + highlight-compression into a single
    /// smooth, mean-preserving multi-knee curve. `> 0` opens the light ratio
    /// (more depth); `< 0` compresses it (flatter). 0 = off.
    pub light_ratio: f32,
}

/// White balance (color temperature / tint). Applied as linear-RGB channel
/// gains — the physically correct place for WB (before tone mapping). All-zero
/// = identity (no shift).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WhiteBalance {
    /// Color-temperature offset, roughly -1..1. >0 = warmer (amber: R up, B
    /// down); <0 = cooler (blue: R down, B up).
    pub temp: f32,
    /// Tint offset, roughly -1..1. >0 = magenta (G down); <0 = green (G up).
    pub tint: f32,
}

/// Creative color grade (M2b). All operations are chroma/hue only — they never
/// touch lightness, so they cannot introduce fake color. `saturation == 1.0`
/// (the default) is identity; `vibrance`/`hue_rotate`/`split_*` are 0 = off.
/// Saturation is zone-aware: midtones get the full slider amount, shadows and
/// highlights are tapered for a film-like, non-digital response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGrade {
    /// Perceptual saturation multiplier. 1.0 = unchanged, <1 desaturates (most in
    /// midtones, less in extremes), >1 boosts (most in midtones, less in extremes).
    pub saturation: f32,
    /// Smart vibrance: boosts low-chroma pixels more than already-saturated
    /// ones (so reds don't clip while muddy mid-tones come alive). 0 = off.
    pub vibrance: f32,
    /// Global hue rotation in degrees (-180..180), creative cast. Neutral-safe
    /// (only affects pixels that already have chroma).
    pub hue_rotate: f32,
    /// Split-tone: add this hue (degrees) to SHADOWS only. 0 = off.
    pub split_shadow: f32,
    /// Split-tone: add this hue (degrees) to HIGHLIGHTS only. 0 = off.
    pub split_highlight: f32,
}

impl Default for ColorGrade {
    fn default() -> Self {
        Self {
            saturation: 1.0,
            vibrance: 0.0,
            hue_rotate: 0.0,
            split_shadow: 0.0,
            split_highlight: 0.0,
        }
    }
}

/// Per-hue-region HSL (M2c). Mirrors ACR's HSL panel: the hue wheel is split
/// into 8 bands (red / orange / yellow / green / aqua / blue / purple /
/// magenta). For each band you can rotate HUE, scale SATURATION, and scale
/// LIGHTNESS independently. All operations stay in OKLCH so they cannot
/// introduce fake color, and a pixel's adjustment is a seamless blend of the
/// two bands it sits between (triangular partition-of-unity weights — no seams
/// at band boundaries, correct for ACR's non-uniform 30°/60° band spacing).
///
/// Each array is indexed by band: 0=red,1=orange,2=yellow,3=green,4=aqua,
/// 5=blue,6=purple,7=magenta. Defaults: `hue_shift=0` (no rotation),
/// `sat_mult=1.0` (unchanged), `light_mult=1.0` (unchanged) → full identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HslRegions {
    /// Hue rotation (degrees) per band, additive. 0 = no shift.
    pub hue_shift: [f32; 8],
    /// Saturation multiplier per band. 1.0 = unchanged, 0 = gray, >1 boost.
    pub sat_mult: [f32; 8],
    /// Lightness multiplier per band. 1.0 = unchanged, <1 darken, >1 brighten
    /// that hue's pixels (perceptually gentle, reversible).
    pub light_mult: [f32; 8],
}

impl Default for HslRegions {
    fn default() -> Self {
        Self {
            hue_shift: [0.0; 8],
            sat_mult: [1.0; 8],
            light_mult: [1.0; 8],
        }
    }
}

impl HslRegions {
    /// Parse an ACR-style band name (with common aliases) to its index.
    /// Returns `None` for unknown names (caller should reject).
    pub fn band_index(name: &str) -> Option<usize> {
        match name.to_ascii_lowercase().as_str() {
            "red" => Some(0),
            "orange" => Some(1),
            "yellow" => Some(2),
            "green" => Some(3),
            "aqua" | "cyan" => Some(4),
            "blue" => Some(5),
            "purple" | "violet" => Some(6),
            "magenta" => Some(7),
            _ => None,
        }
    }
}

/// ACR hue-wheel band centers expressed in **OKLCH hue degrees** so the band
/// *labels* line up with what users see (sRGB red ≈ 29°, yellow ≈ 102°, green
/// ≈ 142°, cyan ≈ 195°, blue ≈ 264°, magenta ≈ 327° in OKLCH). OKLCH 0° is a
/// pinkish red, NOT sRGB red, so we cannot reuse the naive HSL/HSV 0/30/60…
/// angles — that would make "red" act on pink. The triangular blend tolerates
/// the uneven spacing fine (partition of unity by nearest-two weights).
const HSL_BAND_CENTERS: [f32; 8] = [29.0, 53.0, 110.0, 142.0, 195.0, 264.0, 294.0, 328.0];

/// Triangular (partition-of-unity) weights for a hue against the two nearest
/// bands. Returns `(i1, w1, i2, w2)` with `w1 + w2 == 1`. Because we always
/// pick the *two nearest* centers, the pixel lies between them on the shorter
/// arc, so linear weights give a seamless blend even with ACR's uneven 30°/60°
/// band spacing (no dead zones, no over-application at band centers).
#[inline]
fn hsl_band_weights(h: f32) -> (usize, f32, usize, f32) {
    let mut i1 = 0usize;
    let mut d1 = f32::MAX;
    let mut i2 = 0usize;
    let mut d2 = f32::MAX;
    for (k, &c) in HSL_BAND_CENTERS.iter().enumerate() {
        let mut d = (h - c).abs();
        if d > 180.0 {
            d = 360.0 - d;
        }
        if d < d1 {
            d2 = d1;
            i2 = i1;
            d1 = d;
            i1 = k;
        } else if d < d2 {
            d2 = d;
            i2 = k;
        }
    }
    let w1 = if d1 + d2 > 0.0 { d2 / (d1 + d2) } else { 1.0 };
    (i1, w1, i2, 1.0 - w1)
}

/// Apply per-hue-region HSL in OKLCH. Hue rotation is additive; saturation and
/// lightness use a weighted geometric mean (reversible, bounded). Identity
/// (all `hue_shift=0`, `sat_mult=1`, `light_mult=1`) is a strict no-op so it
/// never disturbs the M0 round-trip property.
#[inline]
fn apply_hsl_regions(oklch: &mut Oklch<f32>, r: &HslRegions) {
    let mut any = false;
    for i in 0..8 {
        if r.hue_shift[i] != 0.0 || r.sat_mult[i] != 1.0 || r.light_mult[i] != 1.0 {
            any = true;
            break;
        }
    }
    if !any {
        return;
    }
    let h = oklch.hue.into_positive_degrees();
    let (i1, w1, i2, w2) = hsl_band_weights(h);

    let hue_rot = w1 * r.hue_shift[i1] + w2 * r.hue_shift[i2];
    // Weighted geometric mean → reversible & bounded (ln-space blend).
    let sat_f = (w1 * r.sat_mult[i1].ln() + w2 * r.sat_mult[i2].ln()).exp();
    let light_f = (w1 * r.light_mult[i1].ln() + w2 * r.light_mult[i2].ln()).exp();

    oklch.chroma = (oklch.chroma * sat_f).max(0.0);
    oklch.l = (oklch.l * light_f).clamp(0.0, 1.0);
    oklch.hue = OklabHue::from_degrees((h + hue_rot).rem_euclid(360.0));
}

/// Rendering parameters.
///
/// M1 fields: `exposure_ev`, `tone_map`.
/// M2a field: `defake` (de-fake-color, off in `default`, on in `photo_default`).
/// M2b fields: `grade` (lightness-only L ops) + `white_balance` (linear-RGB temp/
/// tint) + `color` (OKLCH chroma/hue creative grade).
/// M2c field: `hsl` (per-hue-region H/S/L). All default to identity.
#[derive(Clone, Debug, PartialEq)]
pub struct Adjustments {
    /// Exposure compensation in stops. Linear multiplier = 2^ev.
    pub exposure_ev: f32,
    /// Tone-map / shoulder-compression mode (default `None` = identity).
    pub tone_map: ToneMapMode,
    /// De-fake-color stage (default off; product profile turns it on).
    pub defake: DefakeColor,
    /// Lightness-only grade (default all-zero = identity).
    pub grade: Grade,
    /// White balance (color temperature / tint), linear-RGB gains. Default = identity.
    pub white_balance: WhiteBalance,
    /// Creative color grade (saturation/vibrance/hue/split-tone), OKLCH. Default = identity.
    pub color: ColorGrade,
    /// Per-hue-region HSL (8 ACR bands). Default = identity (no per-region shift).
    pub hsl: HslRegions,
    /// 粉嫩肤色模块 (M5). Default off (identity-safe).
    pub skin: SkinTone,
    /// 多分区亮度融合 (M6). Default all-zero = identity.
    pub zones: ZoneGrade,
    /// 几何预处理 (M4b): 裁剪 / 旋转 / 翻转 / 透视. Default identity.
    pub geometry: Geometry,
    /// 细节后处理 (M5): 降噪 / 锐化 / 柔光. Default all 0 = identity.
    pub detail: Detail,
    /// 高级修图 (原 M6): 频谱磨皮 / 金字塔融合. Default all off = identity.
    pub advanced: Advanced,
    /// 色彩引擎计划（一键中性时设置，用于像素级颜色修正；None=跳过）。
    pub color_plan: Option<ColorPlan>,
    /// 整体效果：把修后的图按该比例与原图混合。1.0 = 完整效果，0.0 = 原图，
    /// 中间值可"融合一点原图"（如轻度过度修图时拉回 80%）。不参与 AI 创意参数。
    pub mix: f32,
}

impl Default for Adjustments {
    /// Identity: no exposure / grade / color, but full-effect blend (mix=1.0),
    /// so `render` of an identity adjustment yields the original image.
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            tone_map: ToneMapMode::None,
            defake: DefakeColor::default(),
            grade: Grade::default(),
            white_balance: WhiteBalance::default(),
            color: ColorGrade::default(),
            hsl: HslRegions::default(),
            skin: SkinTone::default(),
            zones: ZoneGrade::default(),
            geometry: Geometry::default(),
            detail: Detail::default(),
            advanced: Advanced::default(),
            color_plan: None,
            mix: 1.0,
        }
    }
}

impl Adjustments {
    /// Pure identity: no exposure, no tone map, no de-fake-color, no grade.
    pub fn identity() -> Self {
        Self::default()
    }

    /// Product default look: NO tone-map (keeps original brightness/exposure)
    /// + always-on de-fake-color (gentle, does not alter the picture at rest)
    /// + a gentle "factory micro-grade" that adds depth without washing out:
    /// soft brightness lift 0.06, contrast 0.15, dehaze 0.25, shadow lift 0.15
    /// (Shadows) and deep-shadow lift 0.15 (Blacks) so the deepest blacks keep
    /// a little detail while mid-darks stay gentle. AgX/Filmic shoulder
    /// compression is opt-in — applied only when the user pushes exposure
    /// (exposure_ev > 0) or explicitly selects a filmic look, because at
    /// `ev=0` it only brightens and washes out detail.
    pub fn photo_default() -> Self {
        Self {
            exposure_ev: 0.0,
            tone_map: ToneMapMode::None,
            defake: DefakeColor::on(),
            grade: Grade {
                brightness_lift: 0.06,
                contrast: 0.15,
                dehaze: 0.25,
                shadow_lift: 0.15,
                deep_shadow_lift: 0.15,
                film_curve: 0.0,
                light_ratio: 0.0,
            },
            white_balance: WhiteBalance::default(),
            color: ColorGrade::default(),
            hsl: HslRegions::default(),
            skin: SkinTone::default(),
            zones: ZoneGrade::default(),
            geometry: Geometry::default(),
            detail: Detail::default(),
            advanced: Advanced::default(),
            color_plan: None,
            mix: 1.0,
        }
    }

    /// 导入图片时的起点参数（商业软件标准）：
    /// - 首张（相册为空）= 原图**零修改**（`default()`，恒等渲染即原图）；
    ///   绝不在导入时自动套"照片默认"调味，否则用户会看到"自动修了一下"。
    /// - 后续图（相册非空）= 沿用当前工作参数（用户已调的参数应保留）。
    pub fn import_baseline_adj(album_empty: bool, current: &Adjustments) -> Adjustments {
        if album_empty {
            Adjustments::default()
        } else {
            current.clone()
        }
    }
}

/// sRGB encoded 8-bit -> linear-light f32 (exact sRGB transfer function).
#[inline]
fn srgb_to_linear(u: u8) -> f32 {
    let c = u as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// linear-light f32 -> sRGB encoded 8-bit.
#[inline]
fn linear_to_srgb(l: f32) -> u8 {
    let c = if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Hermite smoothstep in [edge0, edge1] -> [0,1].
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Apply the de-fake-color grade in OKLCH. Mutates `oklch.chroma` only;
/// hue and lightness are never touched (that is the whole point — no hue drift).
#[inline]
fn apply_defake(oklch: &mut Oklch<f32>, d: &DefakeColor) {
    let l = oklch.l;
    let h = oklch.hue.into_positive_degrees(); // 0..360
    let mut c = oklch.chroma;

    // 1. Luminance-linked chroma decay: fade C in the highlights (prevents neon
    //    fringes / fake color on bright skies, speculars, over-exposed skin) and
    //    in the deep shadows (crushes chroma noise). The highlight ramp starts
    //    at L=0.6 (not 0.75) so the effect is actually *visible* on normal
    //    photos — the previous 0.75 threshold rarely fired. Mid-tones stay
    //    untouched so overall color richness is preserved.
    if d.chroma_decay > 0.0 {
        let hi = smoothstep(0.60, 1.0, l); // 0 below L=0.60, ramps to 1 at white
        let lo = 1.0 - smoothstep(0.0, 0.18, l); // 1 at black, 0 above L=0.18
        let atten = d.chroma_decay * hi.max(lo);
        c *= 1.0 - atten;
    }

    // 1b. Global fake-color knee: any pixel whose chroma is implausibly high for
    //     its lightness (the classic "荧光/伪色" over-saturation) gets softly
    //     pulled back. This is what makes 去假色 do something on ordinary images
    //     even when the highlights/shadows are clean. Scales with chroma_decay.
    if d.chroma_decay > 0.0 {
        // Plausible max chroma peaks around mid-lightness and tapers toward
        // black/white; anything well beyond it reads as artificial.
        let plausible = 0.16 * (1.0 - (2.0 * l - 1.0).powi(2)).max(0.0) + 0.06;
        if c > plausible {
            let excess = c - plausible;
            c = plausible + excess * (1.0 - 0.5 * d.chroma_decay);
        }
    }

    // 2. Sky constraint: cap chroma in the sky-blue hue band to kill the
    //    "neon blue sky" artifact. Excess above the cap is soft-compressed.
    if d.fix_sky && (210.0..=240.0).contains(&h) {
        const SKY_CAP: f32 = 0.13;
        if c > SKY_CAP {
            c = SKY_CAP + (c - SKY_CAP) * 0.3;
        }
    }

    // 3. Skin protection: cap chroma in the skin hue band to avoid magenta /
    //    waxy over-saturation.
    if d.protect_skin && (20.0..=45.0).contains(&h) {
        const SKIN_CAP: f32 = 0.11;
        if c > SKIN_CAP {
            c = SKIN_CAP + (c - SKIN_CAP) * 0.4;
        }
    }

    oklch.chroma = c.max(0.0);
}

/// Apply white balance as linear-RGB channel gains (temp -> R/B, tint -> G).
/// Called on the linear-light triple BEFORE tone mapping — the physically
/// correct location for WB (it is a sensor/illuminant adaptation, not a look).
#[inline]
fn apply_white_balance(lr: &mut f32, lg: &mut f32, lb: &mut f32, wb: &WhiteBalance) {
    if wb.temp == 0.0 && wb.tint == 0.0 {
        return;
    }
    // temp>0 (warm): R up, B down.  temp<0 (cool): R down, B up.
    let tr = 1.0 + wb.temp * 0.2;
    let tb = 1.0 - wb.temp * 0.2;
    // tint>0 (magenta): G down.  tint<0 (green): G up.
    let tg = 1.0 - wb.tint * 0.15;
    *lr *= tr;
    *lg *= tg;
    *lb *= tb;
}

/// Apply the creative color grade in OKLCH. Mutates chroma and/or hue only —
/// lightness is never touched, so this cannot introduce fake color. Order:
/// perceptual saturation -> smart vibrance -> hue rotate + split-tone.
///
/// Saturation and vibrance are zone-aware: the slider's effect is strongest in
/// midtones and smoothly tapers in shadows and highlights, mimicking the way
/// film holds colour rather than the uniform, digital-looking push/pull of a
/// plain multiplier. Identity is preserved at saturation == 1.0 and vibrance == 0.0.
#[inline]
fn apply_color_grade(oklch: &mut Oklch<f32>, cg: &ColorGrade) {
    let l = oklch.l;

    // Zone weight: 1.0 at midtones, ~0.5 at pure shadows/highlights, smooth.
    // This makes the saturation/vibrance controls feel "human" and prevents
    // digital-looking colour in the extremes.
    let zone = 0.5 + 0.5 * (1.0 - (2.0 * l - 1.0).abs().powf(1.5));

    // 1. Perceptual saturation: identity at 1.0, stronger in mids, softer in extremes.
    let sat = cg.saturation;
    let mult = if sat >= 1.0 {
        1.0 + (sat - 1.0) * zone
    } else {
        1.0 - (1.0 - sat) * zone
    };
    let mut c = oklch.chroma * mult;

    // 2. Vibrance: boost low-chroma pixels more, also tapered by zone.
    if cg.vibrance != 0.0 {
        let low = 1.0 - (c / 0.3).clamp(0.0, 1.0); // 1 at c=0, 0 at c>=0.3
        c *= 1.0 + cg.vibrance * low * zone;
    }

    // 3. Hue rotate + split-tone. Only meaningful where chroma exists; neutrals
    //    (which have no hue to speak of) are left alone, preserving grays.
    if c > 1e-3 {
        let sh_w = 1.0 - smoothstep(0.0, 0.5, l); // shadows weight
        let hi_w = smoothstep(0.5, 1.0, l); // highlights weight
        let mut h = oklch.hue.into_positive_degrees();
        h += cg.hue_rotate;
        h += cg.split_shadow * sh_w + cg.split_highlight * hi_w;
        oklch.hue = OklabHue::from_degrees(h.rem_euclid(360.0));
    }

    oklch.chroma = c.max(0.0);
}

/// Apply the lightness-only grade in OKLCH. Mutates `oklch.l` only; hue and
/// chroma are untouched (that is what keeps this fake-color-free). Order:
/// soft brightness lift -> global contrast -> mid-tone dehaze/clarity.
///
/// `pivot` is the image's mean lightness, used as the axis for contrast and
/// dehaze so those operations are mean-preserving (they add 层次感 without
/// shifting overall brightness — the user's explicit requirement).
#[inline]
fn apply_grade(oklch: &mut Oklch<f32>, g: &Grade, pivot: f32) {
    let mut l = oklch.l;

    // 1. Soft brightness lift: L' = 1 - (1-L)·2^(-k). Highlights (L→1) are
    //    preserved, shadows/mids are raised gently. Never blows out.
    if g.brightness_lift > 0.0 {
        let k = 2.0f32.powf(-g.brightness_lift);
        l = 1.0 - (1.0 - l) * k;
    }

    // 2. Global contrast around the image's mean lightness (mean-preserving).
    //    ASYMMETRIC: full strength BELOW the mean (builds midtone separation /
    //    立体感, where detail lives) but a mild NEGATIVE strength ABOVE it —
    //    i.e. highlights are gently *compressed* toward the mean instead of
    //    expanded. This is what keeps the default safe on bright / high-key /
    //    backlit images: no over-fit to dark shots, and highlights never clip.
    if g.contrast != 0.0 {
        let k = if l >= pivot {
            -g.contrast * 0.15
        } else {
            g.contrast
        };
        l = pivot + (l - pivot) * (1.0 + k);
    }

    // 3. Mid-tone dehaze / clarity: boost separation around the mean lightness
    //    without shifting the mean brightness. Same asymmetric rule as contrast
    //    — full on shadows/mids, gentle compression on highlights — to protect
    //    highlight detail (the user explicitly liked the highlight handling).
    if g.dehaze > 0.0 {
        let sign = if l > pivot { 1.0 } else { -1.0 };
        let max_d = if pivot > 0.5 { pivot } else { 1.0 - pivot };
        let d = (l - pivot).abs();
        let k = if l >= pivot {
            -g.dehaze * 0.15
        } else {
            g.dehaze
        };
        let new_d = d + k * d * (1.0 - d) * 2.0;
        l = pivot + sign * new_d.clamp(0.0, max_d);
    }

    // 3b. 胶片感过渡 (film-like S-curve). Cubic sigmoid that keeps
    //     f(0)=0, f(0.5)=0.5, f(1)=1 and the derivative positive for all sane
    //     strengths, so there is no tonal reversal / weird oscillation.
    //     f(x) = x + s * (x-0.5) * (1 - 4*(x-0.5)^2)
    //     >0 deepens shadows & lifts highlights; <0 flattens (dreamy).
    if g.film_curve != 0.0 {
        let d = l - 0.5;
        l = (l + g.film_curve * d * (1.0 - 4.0 * d * d)).clamp(0.0, 1.0);
    }

    // 3c. 光比控制融合 (light-ratio fusion). ONE smooth, mean-preserving
    //     multi-knee curve that fuses shadow-lift + midtone-separation +
    //     highlight-compression into a single perceptually-uniform control.
    //     At the pivot the slope stays 1 (no mean shift); away from it the
    //     factor eases, so extremes never blow out — the "加权衰减" behavior.
    if g.light_ratio != 0.0 {
        let d = l - pivot;
        let xc = (d / 0.5).clamp(-1.0, 1.0);
        l = (pivot + d * (1.0 + g.light_ratio * (1.0 - xc * xc))).clamp(0.0, 1.0);
    }

    // 4. Deep-shadow (Blacks) recovery FIRST: an extra, narrower lift confined
    //    to the very darkest end (L < 0.20) so the deepest blacks keep a touch
    //    more detail (toward a stronger 0.25-style lift). Applied before the
    //    broad shadow lift so it actually reaches the lowest pixels.
    if g.deep_shadow_lift > 0.0 {
        const T: f32 = 0.20;
        // True-black floor: pure blacks (intentional backlit silhouette /
        // crushed-to-zero) are NOT lifted — lifting true black to gray is
        // unnatural and wrecks silhouettes. Only crushed-but-present darks
        // (L above ~0.03) get relieved, ramping to full by L≈0.10. This keeps
        // the silhouette look the user wants while still rescuing real crush.
        let floor = smoothstep(0.03, 0.10, l);
        let w = if l < T { (1.0 - l / T).powi(2) } else { 0.0 };
        l += g.deep_shadow_lift * w * floor;
    }

    // 5. Shadow recovery (Shadows): lift the broader dark end (L < 0.30) so
    //    contrast/dehaze crush (step 2/3 push sub-pivot pixels down) doesn't
    //    kill mid-dark detail. Peaks at L=0.30's edge, rolls off to 0 by L=0.30
    //    — midtones and highlights (which the user is happy with) are never
    //    touched. Shares the same true-black floor as step 4.
    if g.shadow_lift > 0.0 {
        const T: f32 = 0.30;
        let floor = smoothstep(0.03, 0.10, l);
        let w = if l < T { (1.0 - l / T).powi(2) } else { 0.0 };
        l += g.shadow_lift * w * floor;
    }

    // 6. Highlight headroom safety net (final): soft-roll anything above 0.90
    //    toward 0.98 so the default can NEVER reintroduce blown highlights,
    //    no matter how bright the source. Quadratic roll-off (no hard knee /
    //    banding). Only affects already-bright pixels. GUARDED by grade_active
    //    so an identity (all-zero) grade stays a strict no-op — this preserves
    //    the M0 round-trip property (render(identity) == input).
    let grade_active = g.brightness_lift > 0.0
        || g.contrast != 0.0
        || g.dehaze > 0.0
        || g.shadow_lift > 0.0
        || g.deep_shadow_lift > 0.0
        || g.film_curve != 0.0
        || g.light_ratio != 0.0;
    if grade_active && l > 0.90 {
        let t = (l - 0.90) / 0.10;
        l = 0.90 + 0.08 * (1.0 - (1.0 - t) * (1.0 - t));
    }

    oklch.l = l.clamp(0.0, 1.0);
}

/// True if a linear-sRGB triple sits inside the [0,1] cube (with epsilon).
#[inline]
fn in_gamut(lin: LinSrgb<f32>) -> bool {
    const EPS: f32 = 1e-4;
    let (r, g, b) = lin.into_components();
    (-EPS..=1.0 + EPS).contains(&r)
        && (-EPS..=1.0 + EPS).contains(&g)
        && (-EPS..=1.0 + EPS).contains(&b)
}

/// Self-developed gamut soft-clip: if the OKLCH color maps outside sRGB,
/// bisect chroma down (keeping L and H fixed) until it fits. This replaces the
/// hard per-channel clip that shifts hue and causes fake color. Returns the
/// in-gamut linear-sRGB color.
#[inline]
fn gamut_softclip(oklch: Oklch<f32>) -> LinSrgb<f32> {
    let lin: LinSrgb<f32> = oklch.into_color();
    if in_gamut(lin) {
        return lin;
    }
    // Bisection on chroma in [0, current]. 16 iters => < 1e-4 chroma precision.
    let mut lo = 0.0f32;
    let mut hi = oklch.chroma;
    let mut probe = oklch;
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        probe.chroma = mid;
        let test: LinSrgb<f32> = probe.into_color();
        if in_gamut(test) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    probe.chroma = lo;
    probe.into_color()
}

/// Render an image through the OKLCH pipeline (M0 + M1 + M2a).
pub fn render(img: &DynamicImage, adj: &Adjustments) -> RgbImage {
    use rayon::prelude::*;

    // 0. 几何预处理：解码后、线性化前（裁剪 / 旋转 / 翻转 / 透视）。
    //    仅当非 identity 才做 resample，避免无谓的整图拷贝。
    let transformed = if adj.geometry.is_identity() {
        None
    } else {
        Some(apply_geometry(img.clone(), &adj.geometry))
    };
    let work: &DynamicImage = match &transformed {
        Some(t) => t,
        None => img,
    };

    let rgb = work.to_rgb8();
    let (w, h) = rgb.dimensions();
    let src = rgb.into_raw(); // Vec<u8>, len = w*h*3, no clone
    let mut dst = vec![0u8; src.len()];

    let exposure_mult = 2.0f32.powf(adj.exposure_ev);
    let tone_curve = adj.tone_map.curve();
    let defake = adj.defake;
    let grade = adj.grade;
    let wb = adj.white_balance;
    let cg = adj.color;
    let hsl = adj.hsl;
    let skin = adj.skin;
    let zones = adj.zones;
    // 整体效果：修后图与原图在 sRGB 域按比例混合（1.0=完整效果，0.0=原图）。
    let mix = adj.mix.clamp(0.0, 1.0);

    // Pre-pass: mean OKLCH lightness, used as the mean-preserving pivot for
    // contrast / dehaze so those never shift overall brightness.
    let n = src.len() / 3;
    let mean_l: f32 = if grade.contrast != 0.0 || grade.dehaze > 0.0 {
        let sum: f64 = src
            .par_chunks(3)
            .map(|inp| {
                let lin = LinSrgb::new(
                    srgb_to_linear(inp[0]),
                    srgb_to_linear(inp[1]),
                    srgb_to_linear(inp[2]),
                );
                let ok: Oklch<f32> = lin.into_color();
                ok.l as f64
            })
            .sum();
        (sum / n as f64) as f32
    } else {
        0.5
    };

    // 星TAP Rust 基座: rayon 并行替代逐像素 for 循环，多核 + 自动 SIMD。
    dst.par_chunks_mut(3)
        .zip(src.par_chunks(3))
        .for_each(|(out, inp)| {
            // 1. decode sRGB -> linear f32
            let mut lr = srgb_to_linear(inp[0]);
            let mut lg = srgb_to_linear(inp[1]);
            let mut lb = srgb_to_linear(inp[2]);

            // 2. exposure (linear multiplier)
            lr *= exposure_mult;
            lg *= exposure_mult;
            lb *= exposure_mult;

            // 2.5 white balance (linear-RGB channel gains). Physically correct
            // location: before the non-linear tone map, so it acts as a sensor
            // / illuminant adaptation, not an artistic look. Identity when temp
            // and tint are both 0 (default).
            apply_white_balance(&mut lr, &mut lg, &mut lb, &wb);

            // 3. tone map (AgX/Filmic shoulder compression); no-op if None
            if let Some(curve) = tone_curve {
                let mapped = curve.map_rgb([lr, lg, lb]);
                lr = mapped[0];
                lg = mapped[1];
                lb = mapped[2];
            } else if exposure_mult > 1.0 {
                // Gentle highlight rolloff even without a dedicated tone-map
                // curve.  Prevents abrupt hard-clipping when the user pushes
                // exposure while "无" is selected.
                //
                // Two properties are essential and both were wrong before:
                //
                // 1) LUMINANCE, NOT PER-CHANNEL. A per-channel Reinhard
                //    (lr/(1+lr*lr*k) on R,G,B independently) compresses the
                //    brightest channel more than the others, rotating hue and
                //    desaturating highlights — the "灰色伪色块" the user saw.
                //    We compute ONE rolloff ratio from luminance and scale all
                //    three channels by it, so R:G:B ratios (hue + saturation)
                //    are preserved and highlights roll toward a *clean* white.
                //
                // 2) MONOTONIC SHOULDER. The old v/(1+v*v*k) is non-monotonic:
                //    it peaks near v=1/sqrt(k) then DECREASES, so very bright
                //    pixels became darker/gray than merely-bright ones. We use
                //    a tanh knee that is strictly increasing and asymptotes to
                //    exactly 1.0 — highlights compress to smooth pure white,
                //    never invert, never gray out.
                let luma = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
                const KNEE: f32 = 0.70; // below this: perfectly linear
                if luma > KNEE {
                    let range = 1.0 - KNEE; // headroom to white
                    let rolled = KNEE + range * ((luma - KNEE) / range).tanh();
                    let ratio = rolled / luma;
                    lr *= ratio;
                    lg *= ratio;
                    lb *= ratio;
                }
            }

            // 4. rotate into OKLCH (perceptually uniform, decoupled L/C/H)
            let lin = LinSrgb::new(lr, lg, lb);
            let mut oklch: Oklch<f32> = lin.into_color();
            // 保存原图像素的 OKLCH 全量：所有后续护栏/皮肤/伪色检测以此为基准。
            let orig_oklch = oklch;

            // Save original chroma for the saturation guard below.
            let orig_c = orig_oklch.chroma;

            // 5. de-fake-color grade (M2a); no-op if disabled
            if defake.enabled {
                apply_defake(&mut oklch, &defake);
            }
            // 5b. creative color grade (saturation/vibrance/hue/split-tone):
            //     chroma/hue only, neutral-safe, all-off = identity.
            apply_color_grade(&mut oklch, &cg);
            // 5c. lightness-only grade (M2b preview); all-zero = identity
            apply_grade(&mut oklch, &grade, mean_l);
            // 5d. per-hue-region HSL (M2c): seamless ACR-style split, the
            //     final creative touch on H/S/L before gamut. Identity no-op.
            apply_hsl_regions(&mut oklch, &hsl);
            // 5f. 粉嫩肤色 (M5): detect + beautify skin, leave non-skin intact.
            //     传入原图 OKLCH 供阴影检测（深影调皮肤 → 轻微淡化，自然柔和不发红）。
            apply_skin_tone(&mut oklch, &skin, &orig_oklch);

            // 5g. 色彩引擎像素级修正（天空/草木/数码黄/暗部彩度等）
            if let Some(ref cp) = adj.color_plan {
                apply_color_correction(&mut oklch, cp, orig_c);
            }

            // 5h. 多分区亮度融合（OKLCH L 域）：按像素明度分配到 4 区，只调 L，
            //     绝不动 C 或 H → 消除旧版在 sRGB 域操作的伪色问题。
            if zones.lift != [0.0; 4] {
                let zd = zone_delta(oklch.l, &zones);
                oklch.l = (oklch.l + zd).clamp(0.0, 1.0);
            }

            // 5h. 全局伪色护栏：多模块叠加后彩度飙升 > 3× 原图 → 视为伪色，拉回到 2×。
            if oklch.chroma > orig_c * 3.0 && oklch.chroma > 0.12 {
                oklch.chroma = orig_c * 2.0;
            }

            // Saturation guard: only clamp when the pipeline actively raises
            // chroma (non-identity grades).  At identity this is a no-op so the
            // m1_identity_matches_m0 test stays bit-exact.  Cap at 2.5× the
            // original chroma or 0.30 absolute, whichever is tighter, preventing
            // the "99 % saturation" the user flagged.
            if oklch.chroma > orig_c * 1.05 {
                let max_c = (orig_c * 2.5).min(0.30);
                oklch.chroma = oklch.chroma.min(max_c);
            }
            // 6. gamut soft-clip (reduces C, not L/H, if out of sRGB). Always
            //    run: cheap (returns early when in-gamut) and safe.
            let lin2: LinSrgb<f32> = gamut_softclip(oklch);

            let (lr2, lg2, lb2) = lin2.into_components();

            // 6. encode linear -> sRGB u8, then blend with the original by `mix`.
            let pr = linear_to_srgb(lr2);
            let pg = linear_to_srgb(lg2);
            let pb = linear_to_srgb(lb2);
            out[0] = (mix * pr as f32 + (1.0 - mix) * inp[0] as f32).round() as u8;
            out[1] = (mix * pg as f32 + (1.0 - mix) * inp[1] as f32).round() as u8;
            out[2] = (mix * pb as f32 + (1.0 - mix) * inp[2] as f32).round() as u8;
        });

    let mut out_img = RgbImage::from_raw(w, h, dst).expect("dst length matches w*h*3");

    // 后处理：细节 (M5) 与高级修图 (原 M6) 在 sRGB 域完成，均恒等短路。
    if !adj.detail.is_identity() {
        out_img = apply_detail(out_img, &adj.detail);
    }
    if !adj.advanced.is_identity() {
        out_img = apply_advanced(out_img, &adj.advanced);
    }
    out_img
}

/// 智能美肤 A 预设（v0.6，零模型）：`SkinTone::pink()` 去黄+加粉 的粉嫩肤色，
/// 叠加温和 `FreqSepSkin` 频谱磨皮。护眼唇/背景来自 `skin_prob` 对眼唇低概率的
/// 天然区域级保护（无需额外代码）。UI 调用后只把 `skin` + `advanced.freqsep`
/// 两个字段并入当前参数，其余修图不动。
pub fn smart_beauty_preset() -> Adjustments {
    let mut a = Adjustments::default();
    a.skin = SkinTone::pink(); // enabled=true, 粉嫩
    a.advanced.freqsep = FreqSepSkin {
        enabled: true,
        strength: 0.3,
        texture_keep: 0.8,
        smoothness: 0.3,
        mask_feather: 0.5,
    };
    a
}

/// 自动保存会话：把「当前源路径 + 当前调色参数」序列化为 JSON（崩溃/意外退出后恢复用）。
/// 以 `Preset` 作为 `Adjustments` 的可序列化桥梁（Adjustments 本身未 derive Serialize）。
pub fn save_session_json(
    path: &std::path::Path,
    src: Option<&std::path::Path>,
    adj: &Adjustments,
) -> std::io::Result<()> {
    use serde_json::json;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let preset = adj.to_preset();
    let obj = json!({
        "src": src.map(|p| p.to_string_lossy().to_string()),
        "adj": preset,
    });
    let s = serde_json::to_string(&obj)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, s)
}

/// 读取自动保存会话。源文件已不存在则返回 None（无法恢复）。
pub fn load_session_json(
    path: &std::path::Path,
) -> Option<(std::path::PathBuf, Adjustments)> {
    use serde_json::Value;
    let data = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&data).ok()?;
    let src = v.get("src")?.as_str()?;
    let p = std::path::PathBuf::from(src);
    if !p.exists() {
        return None;
    }
    let preset: crate::preset::Preset =
        serde_json::from_value(v.get("adj")?.clone()).ok()?;
    Some((p, preset.to_adjustments()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// OKLCH round-trip must not introduce perceivable color shift when no
    /// adjustments are applied (M0 property must hold through M1/M2).
    #[test]
    fn oklch_roundtrip_no_color_shift() {
        let mut img = RgbImage::new(128, 128);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 7 + y * 13) % 256) as u8;
            *px = Rgb([v, v.wrapping_mul(3), v.wrapping_mul(5)]);
        }
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render(&dyn_img, &Adjustments::identity());

        let mut max_diff = 0i32;
        for (p, q) in img.pixels().zip(out.pixels()) {
            for i in 0..3 {
                let d = (p.0[i] as i32 - q.0[i] as i32).abs();
                if d > max_diff {
                    max_diff = d;
                }
            }
        }
        assert!(
            max_diff <= 2,
            "OKLCH roundtrip max diff too high: {}",
            max_diff
        );
    }

    /// Identity adjustments must be bit-for-bit equivalent to M0 path.
    #[test]
    fn m1_identity_matches_m0() {
        let mut img = RgbImage::new(64, 64);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 11 + y * 17) % 256) as u8;
            *px = Rgb([v, (v / 2), (255 - v)]);
        }
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render(&dyn_img, &Adjustments::identity());
        for (p, q) in img.pixels().zip(out.pixels()) {
            for i in 0..3 {
                assert_eq!(p.0[i], q.0[i], "identity path changed pixel");
            }
        }
    }

    // ── 导入零修改回归测试 ────────────────────────────────────────────────
    // 商业软件标准：打开一张图默认就是原图，绝不能自动套"照片默认"调味。
    // 这些测试锁死 import_baseline_adj 的语义，谁把自动调味塞回导入都会让它们变红。

    #[test]
    fn import_first_image_is_identity() {
        // 首张导入（相册为空）必须是零修改，与用户当前参数无关。
        let cur = Adjustments::photo_default();
        let a = Adjustments::import_baseline_adj(true, &cur);
        // 关键字段必须全部为零（恒等）。
        assert_eq!(a.exposure_ev, 0.0, "首图曝光应=0");
        assert_eq!(a.grade.contrast, 0.0, "首图对比度应=0");
        assert_eq!(a.grade.dehaze, 0.0, "首图去雾应=0");
        assert_eq!(a.grade.shadow_lift, 0.0, "首图阴影应=0");
        assert_eq!(a.grade.deep_shadow_lift, 0.0, "首图黑色应=0");
        assert_eq!(a.mix, 1.0, "首图 mix 应=1(全效果=原图)");
        // 整体等于 default（恒等渲染即原图）。
        assert_eq!(
            a.grade.contrast,
            Adjustments::default().grade.contrast
        );
    }

    #[test]
    fn import_subsequent_keeps_current() {
        // 后续图（相册非空）沿用当前工作参数。
        let cur = Adjustments::photo_default();
        let a = Adjustments::import_baseline_adj(false, &cur);
        assert_eq!(a.grade.contrast, 0.15, "后续图应沿用当前对比度");
        assert_eq!(a.grade.dehaze, 0.25, "后续图应沿用当前去雾");
    }

    #[test]
    fn photo_default_is_not_identity() {
        // 钉死：photo_default 确实带调味（≠恒等），所以它绝不能用于"导入默认"。
        let d = Adjustments::default();
        let p = Adjustments::photo_default();
        assert_ne!(d.grade.contrast, p.grade.contrast, "photo_default 必须带对比度调味");
        assert_ne!(d.grade.dehaze, p.grade.dehaze, "photo_default 必须带去雾调味");
    }

    #[test]
    fn render_identity_is_original_image() {
        // 端到端确认：用 identity 渲染的结果，与原图逐像素一致（与 import 零修改呼应）。
        let mut img = RgbImage::new(96, 96);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 5 + y * 19) % 256) as u8;
            *px = Rgb([v, v.wrapping_mul(2), v.wrapping_mul(7)]);
        }
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render(&dyn_img, &Adjustments::import_baseline_adj(true, &Adjustments::photo_default()));
        let mut max_diff = 0i32;
        for (p, q) in img.pixels().zip(out.pixels()) {
            for i in 0..3 {
                let d = (p.0[i] as i32 - q.0[i] as i32).abs();
                if d > max_diff {
                    max_diff = d;
                }
            }
        }
        assert!(max_diff <= 2, "首图导入(identity)渲染与原图差异过大: {}", max_diff);
    }

    /// Exposure must brighten the image.
    #[test]
    fn exposure_brightens() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([100u8, 100, 100])));
        let id = render(&img, &Adjustments::identity());
        let lit = render(
            &img,
            &Adjustments {
                exposure_ev: 2.0,
                ..Default::default()
            },
        );
        let id_y = id.get_pixel(0, 0).0[0] as f32;
        let lit_y = lit.get_pixel(0, 0).0[0] as f32;
        assert!(
            lit_y > id_y + 50.0,
            "exposure +2EV should brighten a lot ({} vs {})",
            lit_y,
            id_y
        );
    }

    /// Tone map must compress highlights (anti fake-color).
    #[test]
    fn tone_map_compresses_highlights() {
        let curve = ToneMapMode::Agx.curve().expect("Agx curve");
        let over = curve.map_rgb([8.0, 8.0, 8.0]);
        for c in over {
            assert!(c.is_finite(), "tone map produced non-finite value");
            assert!(c >= 0.0, "tone map produced negative value: {}", c);
            assert!(c <= 1.0 + 1e-4, "tone map exceeded SDR range: {}", c);
        }
        let red = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([255u8, 40, 40])));
        let no_tm = render(&red, &Adjustments::identity());
        let agx = render(
            &red,
            &Adjustments {
                exposure_ev: 2.0,
                tone_map: ToneMapMode::Agx,
                ..Default::default()
            },
        );
        let nt = no_tm.get_pixel(0, 0).0;
        let ax = agx.get_pixel(0, 0).0;
        let nt_sat = (nt[0] as i32 - nt[1] as i32).abs() + (nt[0] as i32 - nt[2] as i32).abs();
        let ax_sat = (ax[0] as i32 - ax[1] as i32).abs() + (ax[0] as i32 - ax[2] as i32).abs();
        assert!(
            ax_sat <= nt_sat,
            "AgX should not increase highlight saturation ({} vs {})",
            ax_sat,
            nt_sat
        );
    }

    /// Filmic mode must also produce finite, in-range SDR output.
    #[test]
    fn filmic_mode_valid() {
        let curve = ToneMapMode::Filmic.curve().expect("Filmic curve");
        let out = curve.map_rgb([4.0, 1.0, 0.25]);
        for c in out {
            assert!(
                c.is_finite() && c >= 0.0 && c <= 1.0 + 1e-4,
                "filmic out of range: {}",
                c
            );
        }
    }

    // ---------- M2a de-fake-color tests ----------

    /// De-fake-color OFF must be bit-for-bit identity (product safety).
    #[test]
    fn defake_off_is_identity() {
        let mut img = RgbImage::new(32, 32);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 5 + y * 9) % 256) as u8;
            *px = Rgb([v, 255 - v, v / 2]);
        }
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render(&dyn_img, &Adjustments::identity());
        for (p, q) in img.pixels().zip(out.pixels()) {
            for i in 0..3 {
                assert_eq!(p.0[i], q.0[i], "defake-off changed pixel");
            }
        }
    }

    /// Chroma decay must desaturate a bright saturated pixel, never invert or
    /// shift its hue toward another color (the fake-color failure mode).
    #[test]
    fn chroma_decay_desaturates_highlight_no_hue_shift() {
        // A bright, saturated blue-ish highlight.
        let px = Rgb([180u8, 200, 250]);
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, px));
        let mut adj = Adjustments::identity();
        adj.defake = DefakeColor {
            enabled: true,
            chroma_decay: 0.5,
            fix_sky: false,
            protect_skin: false,
            gamut_softclip: true,
        };
        let out = render(&img, &adj);
        let o = out.get_pixel(0, 0).0;

        // Measure chroma via OKLCH on both.
        let c_in = {
            let lin = LinSrgb::new(
                srgb_to_linear(px.0[0]),
                srgb_to_linear(px.0[1]),
                srgb_to_linear(px.0[2]),
            );
            let ok: Oklch<f32> = lin.into_color();
            ok.chroma
        };
        let (c_out, h_in, h_out) = {
            let lin_out = LinSrgb::new(
                srgb_to_linear(o[0]),
                srgb_to_linear(o[1]),
                srgb_to_linear(o[2]),
            );
            let ok_out: Oklch<f32> = lin_out.into_color();
            let lin_in = LinSrgb::new(
                srgb_to_linear(px.0[0]),
                srgb_to_linear(px.0[1]),
                srgb_to_linear(px.0[2]),
            );
            let ok_in: Oklch<f32> = lin_in.into_color();
            (
                ok_out.chroma,
                ok_in.hue.into_positive_degrees(),
                ok_out.hue.into_positive_degrees(),
            )
        };
        assert!(
            c_out < c_in,
            "chroma decay should reduce chroma ({} -> {})",
            c_in,
            c_out
        );
        // Hue must be essentially unchanged (no fake color). Allow small
        // quantization wobble (< 5 deg).
        let dh = (h_in - h_out).abs().min(360.0 - (h_in - h_out).abs());
        assert!(
            dh < 5.0,
            "hue must not drift under chroma decay ({} vs {})",
            h_in,
            h_out
        );
    }

    /// Gamut soft-clip must always land inside sRGB and keep hue stable.
    #[test]
    fn gamut_softclip_stays_in_gamut() {
        // Construct an OKLCH color that is deliberately out of sRGB gamut
        // (very high chroma at mid lightness).
        let oob = Oklch::new(0.7f32, 0.4, 150.0);
        let lin = gamut_softclip(oob);
        assert!(in_gamut(lin), "soft-clip result must be in sRGB gamut");
        // Round-trip its hue: it should stay near 150 deg (green-ish).
        let back: Oklch<f32> = lin.into_color();
        let h = back.hue.into_positive_degrees();
        let dh = (h - 150.0).abs().min(360.0 - (h - 150.0).abs());
        assert!(dh < 8.0, "soft-clip should preserve hue (~150), got {}", h);
    }

    // ---------- M2b color-grade tests ----------

    /// White balance warm (temp>0) must raise R and lower B on a neutral pixel,
    /// i.e. produce a warmer cast, without touching G or shifting lightness much.
    #[test]
    fn white_balance_warm_raises_r_lowers_b() {
        // Neutral mid-gray so the only asymmetry comes from the temp gain.
        let px = Rgb([128u8, 128, 128]);
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, px));
        let adj = Adjustments {
            white_balance: WhiteBalance {
                temp: 0.5,
                tint: 0.0,
            },
            ..Default::default()
        };
        let out = render(&img, &adj).get_pixel(0, 0).0;
        // temp>0 => R up, B down. So out R must exceed out B.
        assert!(
            out[0] > out[2],
            "warm WB should make R > B (got R={}, B={})",
            out[0],
            out[2]
        );
    }

    /// White balance cool (temp<0) must do the opposite (R down, B up).
    #[test]
    fn white_balance_cool_raises_b_lowers_r() {
        let px = Rgb([128u8, 128, 128]);
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, px));
        let adj = Adjustments {
            white_balance: WhiteBalance {
                temp: -0.5,
                tint: 0.0,
            },
            ..Default::default()
        };
        let out = render(&img, &adj).get_pixel(0, 0).0;
        assert!(
            out[2] > out[0],
            "cool WB should make B > R (got R={}, B={})",
            out[0],
            out[2]
        );
    }

    /// Saturation > 1 must increase chroma WITHOUT shifting hue (no fake color),
    /// and the color-grade default must be a strict identity.
    #[test]
    fn saturation_boosts_chroma_no_hue_shift() {
        let px = Rgb([120u8, 160, 200]); // mild blue, well within sRGB
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, px));

        // identity color grade
        let id = render(&img, &Adjustments::default()).get_pixel(0, 0).0;

        // saturation x2
        let adj = Adjustments {
            color: ColorGrade {
                saturation: 2.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let out = render(&img, &adj).get_pixel(0, 0).0;

        let c_in = oklch_chroma(px.0);
        let (c_id, c_out, h_id, h_out) = {
            let ci = oklch_chroma(id);
            let co = oklch_chroma(out);
            let hi = oklch_hue(px.0);
            let ho = oklch_hue(out);
            (ci, co, hi, ho)
        };
        // identity color grade leaves chroma unchanged (within quant noise)
        assert!(
            (c_id - c_in).abs() < 0.004,
            "color-grade default should be identity (C {} -> {})",
            c_in,
            c_id
        );
        // saturation x2 boosts chroma
        assert!(
            c_out > c_in + 0.004,
            "saturation x2 should boost chroma ({} -> {})",
            c_in,
            c_out
        );
        // hue must be stable
        let dh = (h_id - h_out).abs().min(360.0 - (h_id - h_out).abs());
        assert!(
            dh < 5.0,
            "saturation must not shift hue ({} vs {})",
            h_id,
            h_out
        );
    }

    /// Split-tone must shift shadow hue toward split_shadow, but leave the
    /// highlight essentially untouched. We assert the *relative* behavior
    /// (shadow moves clearly more than highlight) rather than an exact angle,
    /// because the shadow weight `1 - smoothstep(0,0.5,l)` intentionally tapers
    /// with lightness and the exact value is a tuning detail.
    #[test]
    fn split_tone_shifts_shadows_only() {
        // Dark pixel (shadow) and a bright pixel (highlight), same hue family.
        let dark = Rgb([40u8, 10, 80]);
        let bright = Rgb([220u8, 220, 240]);
        let adj = Adjustments {
            color: ColorGrade {
                split_shadow: 40.0,
                split_highlight: 0.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let d_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, dark)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        let b_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, bright)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        let h_d = oklch_hue(d_out);
        let h_b = oklch_hue(b_out);
        let h_din = oklch_hue(dark.0);
        // dark pixel hue should move toward split_shadow (positive, clearly > 0)
        let dh_d = (h_d - h_din).rem_euclid(360.0);
        let dh_d_signed = if dh_d > 180.0 { dh_d - 360.0 } else { dh_d };
        assert!(
            dh_d_signed > 8.0,
            "shadow hue should clearly shift toward split_shadow (got {})",
            dh_d_signed
        );
        // bright pixel (highlight) hue should be essentially untouched
        let h_bin = oklch_hue(bright.0);
        let dh_b = (h_b - h_bin).abs().min(360.0 - (h_b - h_bin).abs());
        assert!(
            dh_b < 8.0,
            "highlight hue should be untouched by shadow split (got {})",
            dh_b
        );
        // and shadows must shift MORE than highlights
        assert!(
            dh_d_signed > dh_b + 5.0,
            "shadow should shift more than highlight ({} vs {})",
            dh_d_signed,
            dh_b
        );
    }

    #[inline]
    fn oklch_chroma(px: [u8; 3]) -> f32 {
        let lin = LinSrgb::new(
            srgb_to_linear(px[0]),
            srgb_to_linear(px[1]),
            srgb_to_linear(px[2]),
        );
        let ok: Oklch<f32> = lin.into_color();
        ok.chroma
    }
    #[inline]
    fn oklch_hue(px: [u8; 3]) -> f32 {
        let lin = LinSrgb::new(
            srgb_to_linear(px[0]),
            srgb_to_linear(px[1]),
            srgb_to_linear(px[2]),
        );
        let ok: Oklch<f32> = lin.into_color();
        ok.hue.into_positive_degrees()
    }

    // ---------- M2c per-hue-region HSL tests ----------

    /// Blue-band saturation boost must increase the chroma of a blue pixel and
    /// keep its hue stable (no fake color). Default HSL must be strict identity.
    #[test]
    fn hsl_blue_band_boosts_sky_saturation() {
        let px = Rgb([150u8, 185, 230]); // light blue, has gamut headroom
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, px));

        // default (identity) HSL leaves chroma untouched within quant noise
        let id = render(&img, &Adjustments::default()).get_pixel(0, 0).0;
        let c_in = oklch_chroma(px.0);
        assert!(
            (oklch_chroma(id) - c_in).abs() < 0.004,
            "HSL default must be identity (C {} -> {})",
            c_in,
            oklch_chroma(id)
        );

        // blue band (index 5) saturation x1.4
        let mut hsl = HslRegions::default();
        hsl.sat_mult[5] = 1.4;
        let adj = Adjustments {
            hsl,
            ..Default::default()
        };
        let out = render(&img, &adj).get_pixel(0, 0).0;

        let c_out = oklch_chroma(out);
        assert!(
            c_out > c_in + 0.004,
            "blue-band sat x1.4 should boost chroma ({} -> {})",
            c_in,
            c_out
        );
        // hue must be stable
        let h_in = oklch_hue(px.0);
        let h_out = oklch_hue(out);
        let dh = (h_in - h_out).abs().min(360.0 - (h_in - h_out).abs());
        assert!(
            dh < 5.0,
            "blue-band sat must not shift hue ({} vs {})",
            h_in,
            h_out
        );
    }

    /// Red-band hue rotation must rotate a RED pixel's hue but leave a GREEN
    /// pixel untouched — proving the per-band blend isolates hues (seamless,
    /// no spill into other bands).
    #[test]
    fn hsl_red_band_rotates_red_only() {
        // Bright, light red (high L) so rotating toward orange stays in-gamut
        // and the output hue is measurable (a dark red would get gamut-crushed
        // to near-gray and its re-read hue would be noise).
        let red_px = Rgb([255u8, 110, 110]);
        let green_px = Rgb([40u8, 200, 60]);
        let mut hsl = HslRegions::default();
        hsl.hue_shift[0] = 30.0; // red band rotates +30 deg
        let adj = Adjustments {
            hsl,
            ..Default::default()
        };

        let r_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, red_px)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        let g_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, green_px)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;

        // red pixel should rotate clearly toward orange (+30 within the band)
        let h_rin = oklch_hue(red_px.0);
        let h_rout = oklch_hue(r_out);
        let dh_r = (h_rout - h_rin).rem_euclid(360.0);
        let dh_r_s = if dh_r > 180.0 { dh_r - 360.0 } else { dh_r };
        assert!(
            dh_r_s > 10.0 && dh_r_s < 50.0,
            "red-band +30 should rotate red clearly (got {})",
            dh_r_s
        );

        // green pixel (band 3) must be essentially untouched
        let h_gin = oklch_hue(green_px.0);
        let h_gout = oklch_hue(g_out);
        let dh_g = (h_gin - h_gout).abs().min(360.0 - (h_gin - h_gout).abs());
        assert!(
            dh_g < 6.0,
            "green hue must be untouched by red-band ({} vs {})",
            h_gin,
            h_gout
        );
        // and red must rotate MORE than green
        assert!(
            dh_r_s > dh_g + 8.0,
            "red should rotate more than green ({} vs {})",
            dh_r_s,
            dh_g
        );
    }

    /// Identity HSL must not change the image at all (M0 round-trip preserved
    /// through M2c).
    #[test]
    fn hsl_identity_is_noop() {
        let mut img = RgbImage::new(48, 48);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 13 + y * 7) % 256) as u8;
            *px = Rgb([v, v.wrapping_mul(3), v.wrapping_mul(5)]);
        }
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render(&dyn_img, &Adjustments::default());
        for (p, q) in img.pixels().zip(out.pixels()) {
            for i in 0..3 {
                assert_eq!(p.0[i], q.0[i], "HSL identity changed pixel");
            }
        }
    }

    // ---------- M5 skin-tone tests ----------

    /// A skin-toned pixel must move its hue TOWARD the 粉嫩 target (25°) and its
    /// chroma toward the healthy target, while a clearly non-skin (blue) pixel
    /// must be left essentially untouched (protect_non_skin).
    #[test]
    fn skin_tone_pinkifies_skin_leaves_blue() {
        let skin_px = Rgb([205u8, 150, 120]); // warm light skin
        let blue_px = Rgb([150u8, 185, 230]); // clearly non-skin blue
        let st = SkinTone::pink();
        let adj = Adjustments {
            skin: st,
            ..Default::default()
        };
        let s_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, skin_px)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        let b_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, blue_px)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;

        let h_sin = oklch_hue(skin_px.0);
        let h_sout = oklch_hue(s_out);
        let d_in = (h_sin - 25.0).abs().min(360.0 - (h_sin - 25.0).abs());
        let d_out = (h_sout - 25.0).abs().min(360.0 - (h_sout - 25.0).abs());
        assert!(
            d_out < d_in,
            "skin hue should move toward 粉嫩 target 25° ({} -> {})",
            h_sin,
            h_sout
        );

        // blue must be essentially untouched
        let h_bin = oklch_hue(blue_px.0);
        let h_bout = oklch_hue(b_out);
        let dh = (h_bin - h_bout).abs().min(360.0 - (h_bin - h_bout).abs());
        assert!(
            dh < 5.0,
            "non-skin blue hue must be untouched ({} vs {})",
            h_bin,
            h_bout
        );
        assert!(
            (oklch_chroma(blue_px.0) - oklch_chroma(b_out)).abs() < 0.01,
            "non-skin blue chroma must be untouched"
        );
    }

    /// Off skin module must be a strict identity for the pixel (no-op).
    #[test]
    fn skin_off_is_identity() {
        let px = Rgb([205u8, 150, 120]);
        let adj = Adjustments {
            skin: SkinTone::default(), // enabled = false
            ..Default::default()
        };
        let out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, px)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        assert_eq!(px.0, out, "disabled skin module must not change the pixel");
    }

    // ---------- M6 multi-zone fusion tests ----------

    /// A deep-shadow pixel (low L) must brighten when the shadow zone is lifted,
    /// and an all-zero zone grade must be a strict identity.
    #[test]
    fn zone_lift_brightens_shadows() {
        let dark = Rgb([25u8, 20, 30]);
        let mut adj = Adjustments::default();
        adj.zones.lift[0] = 0.3; // lift 暗部
        let out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, dark)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        assert!(
            out[0] as u32 + out[1] as u32 + out[2] as u32
                > dark.0[0] as u32 + dark.0[1] as u32 + dark.0[2] as u32,
            "shadow-zone lift should brighten a dark pixel"
        );

        // identity zone grade
        let id = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, dark)),
            &Adjustments::default(),
        )
        .get_pixel(0, 0)
        .0;
        assert_eq!(dark.0, id, "zero zone grade must be identity");
    }

    /// 多分区融合必须平滑过渡：输入一条线性灰阶，施加分区提亮/压暗后，输出
    /// 亮度曲线仍应是 C¹ 连续（离散二阶差分有界），不出现硬边/台阶。这是
    /// "边界羽化更自然" 的质量门槛。
    #[test]
    fn zones_fusion_is_smooth() {
        let w = 128u32;
        let h = 4u32;
        let mut img = RgbImage::new(w, h);
        for x in 0..w {
            let v = (x as f32 / (w - 1) as f32 * 255.0).round() as u8;
            for y in 0..h {
                *img.get_pixel_mut(x, y) = Rgb([v, v, v]);
            }
        }
        let mut adj = Adjustments::default();
        adj.zones.lift = [0.2, 0.0, 0.0, -0.2]; // 暗部提亮、高光压暗：制造分区差异
        let out = render(&DynamicImage::ImageRgb8(img), &adj);

        // 每列平均亮度，检查沿 x 方向的平滑性（离散拉普拉斯峰值有界）。
        let mut brightness = vec![0.0f32; w as usize];
        for x in 0..w {
            let mut s = 0.0f32;
            for y in 0..h {
                let p = out.get_pixel(x, y).0;
                s += (p[0] as f32 + p[1] as f32 + p[2] as f32) / 3.0;
            }
            brightness[x as usize] = s / h as f32;
        }
        let mut max_lap = 0.0f32;
        for x in 1..(w as usize - 1) {
            let lap = (brightness[x + 1] - 2.0 * brightness[x] + brightness[x - 1]).abs();
            max_lap = max_lap.max(lap);
        }
        assert!(
            max_lap < 15.0,
            "zone fusion produced a hard seam (laplacian peak {:.2} > 15)",
            max_lap
        );
    }

    /// 胶片感 S-curve: a mid-shadow pixel darkens and a mid-highlight brightens
    /// under a positive film_curve (more film contrast), while the midpoint and
    /// the overall identity (film_curve=0) is preserved.
    #[test]
    fn film_curve_adds_contrast() {
        let lo = Rgb([90u8, 90, 90]); // ~0.35 L
        let hi = Rgb([190u8, 190, 190]); // ~0.72 L
        let mut adj = Adjustments::default();
        adj.grade.film_curve = 0.25;
        let lo_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, lo)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        let hi_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, hi)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        // low pixel (below 0.5) should get darker, high pixel brighter
        assert!(
            (lo_out[0] as i32) < (lo.0[0] as i32),
            "film S-curve should darken shadows ({} -> {})",
            lo.0[0],
            lo_out[0]
        );
        assert!(
            (hi_out[0] as i32) > (hi.0[0] as i32),
            "film S-curve should brighten highlights ({} -> {})",
            hi.0[0],
            hi_out[0]
        );

        // identity: film_curve = 0 must be no-op
        let id = Adjustments::default();
        let lo_id = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, lo)),
            &id,
        )
        .get_pixel(0, 0)
        .0;
        assert_eq!(lo.0, lo_id, "film_curve=0 must be identity");
    }

    /// 光比融合 (light_ratio) must open the light ratio: a shadow pixel darkens
    /// and a highlight brightens under positive light_ratio, pivot-preserving.
    #[test]
    fn light_ratio_fusion_works() {
        let lo = Rgb([80u8, 80, 80]);
        let hi = Rgb([200u8, 200, 200]);
        let mut adj = Adjustments::default();
        adj.grade.light_ratio = 0.5;
        let lo_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, lo)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        let hi_out = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, hi)),
            &adj,
        )
        .get_pixel(0, 0)
        .0;
        assert!(
            (lo_out[0] as i32) < (lo.0[0] as i32),
            "light_ratio should deepen shadows"
        );
        assert!(
            (hi_out[0] as i32) > (hi.0[0] as i32),
            "light_ratio should lift highlights"
        );
        let id = Adjustments::default();
        let lo_id = render(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, lo)),
            &id,
        )
        .get_pixel(0, 0)
        .0;
        assert_eq!(lo.0, lo_id, "light_ratio=0 must be identity");
    }

    /// 90° 旋转必须正常产出、不崩溃（特别在 release panic=abort 模式）。
    #[test]
    fn quarter_turns_render_succeeds() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 200, Rgb([128u8; 3])));
        for q in [0u8, 1, 2, 3] {
            let mut adj = Adjustments::default();
            adj.geometry.quarter_turns = q;
            let out = render(&img, &adj);
            if q % 2 == 0 {
                assert_eq!(out.dimensions(), (100, 200));
            } else {
                assert_eq!(out.dimensions(), (200, 100));
            }
        }
    }
}
