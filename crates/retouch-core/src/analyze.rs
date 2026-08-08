//! AI-facing image analysis.
//!
//! Turns a photo into perceptual, *AI-readable* metrics expressed in our native
//! OKLCH space (not Lab). This is the structured data an agent consumes to
//! decide corrections — the machine equivalent of "looking at the picture".
//!
//! Design note: retouch_app (darktable-based) emitted Lab metrics
//! (`meanL/stdL/mean_a/mean_b/mean_C/...`). We deliberately switch to OKLCH
//! because (a) it is the space our pipeline actually reasons in, (b) hue is a
//! plain 0–360° angle (easy for an LLM to reason about "too green" / "too
//! warm"), and (c) lightness/chroma are perceptually uniform, so a given delta
//! means the same thing everywhere. Same *purpose*, better *basis*.

use image::DynamicImage;
use palette::{IntoColor, LinSrgb, Oklch, Srgb};
use serde::Serialize;

/// Full metric bundle for one image. All angles in degrees, all L/C in 0–1.
#[derive(Serialize, Debug, Clone)]
pub struct ImageMetrics {
    pub width: u32,
    pub height: u32,
    /// Luminance distribution (OKLCH L, 0=black .. 1=white).
    pub tone: ToneMetrics,
    /// Chroma / hue distribution.
    pub color: ColorMetrics,
    /// Skin-region statistics (only meaningful if `ratio` > ~0.03).
    pub skin: SkinMetrics,
    /// Clipping health (lost highlights / blocked shadows).
    pub exposure: ExposureMetrics,
    /// sRGB gamut overflow (our soft-clip prevents hard loss, but this tells
    /// an agent how much chroma had to be pulled back).
    pub gamut: GamutMetrics,
    /// Overall color cast (dominant hue + its chroma).
    pub cast: CastMetrics,
    /// `max_l - min_l`: true dynamic range, independent of mid-tone placement.
    pub dynamic_range: f32,
}

#[derive(Serialize, Debug, Clone)]
pub struct ToneMetrics {
    pub mean_l: f32,
    pub std_l: f32,
    pub min_l: f32,
    pub max_l: f32,
    /// 直方图中位数 L（50th 百分位）。比 mean_l 更代表图的"真实基调"——
    /// 高 DR 图（夜景/逆光）有少量极亮像素把均值抬高，但大面积仍是暗的，
    /// 中位数能认出它本该是低调，避免误分类。
    pub median_l: f32,
    /// 25th 百分位 L（暗部代表）。
    pub p25_l: f32,
    /// 75th 百分位 L（亮部代表）。
    pub p75_l: f32,
    /// 亮部面积占比（% 像素 L>0.6）。影调判断用：高调=亮部面积大、暗部面积小。
    /// 比 median_l 更贴合摄影定义（"画面中明暗像素的比例"），避免单点中位数误判。
    pub bright_area_pct: f32,
    /// 暗部面积占比（% 像素 L<0.2）。低调=暗部面积大、亮部面积小。
    pub dark_area_pct: f32,
}

#[derive(Serialize, Debug, Clone)]
pub struct ColorMetrics {
    /// Mean chroma (0..~0.4). Very low => pale / washed-out.
    pub mean_c: f32,
    /// Circular-mean dominant hue in degrees (0=R, 90=Y, 180=G, 270=B).
    pub mean_h_deg: f32,
    /// 0..1 — how concentrated hue is. Low => colorful-but-varied; high => one
    /// dominant color (e.g. a blue sky). Helps detect "monochrome" shots.
    pub hue_peakiness: f32,
    /// Mean chroma per 45° hue bin (indices: 0=R,1=Y/R,2=Y,3=G/Y,4=G,5=C,6=B,7=M).
    pub per_hue_chroma: [f32; 8],
}

#[derive(Serialize, Debug, Clone)]
pub struct SkinMetrics {
    /// Fraction of pixels classified as skin (0..1).
    pub ratio: f32,
    /// Mean chroma of skin pixels (high => ruddy / over-saturated skin).
    pub mean_c: f32,
    /// Mean hue of skin pixels (degrees).
    pub mean_h_deg: f32,
    /// Mean lightness of skin pixels (guardrail uses this to prevent the
    /// "washed-out pale face" look from over-brightening).
    pub mean_l: f32,
}

#[derive(Serialize, Debug, Clone)]
pub struct ExposureMetrics {
    /// % pixels at sRGB 255 (blown highlights).
    pub highlight_clip_pct: f32,
    /// % pixels at sRGB 0 (crushed shadows).
    pub shadow_clip_pct: f32,
}

#[derive(Serialize, Debug, Clone)]
pub struct GamutMetrics {
    /// % pixels whose OKLCH color falls outside sRGB (soft-clipped).
    pub clip_pct: f32,
    /// Highest chroma seen (before clipping).
    pub max_c: f32,
}

#[derive(Serialize, Debug, Clone)]
pub struct CastMetrics {
    /// Dominant hue (degrees) of the overall image.
    pub hue_deg: f32,
    /// Chroma of that dominant direction (cast strength).
    pub chroma: f32,
}

#[inline]
fn ok_of(px: [u8; 3]) -> Oklch<f32> {
    let lin = Srgb::new(
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    )
    .into_linear();
    lin.into_color()
}

#[inline]
fn in_gamut(ok: Oklch<f32>) -> bool {
    let lin: LinSrgb<f32> = ok.into_color();
    lin.red >= -1e-4
        && lin.red <= 1.0 + 1e-4
        && lin.green >= -1e-4
        && lin.green <= 1.0 + 1e-4
        && lin.blue >= -1e-4
        && lin.blue <= 1.0 + 1e-4
}

/// Cheap OKLCH skin probability (0..1): hue band ~[15,45]°, modest chroma,
/// mid luminance. Mirrors `pipeline::skin_probability` intent without coupling
/// to its tuning constants.
fn skin_prob(ok: &Oklch<f32>) -> f32 {
    let h = ok.hue.into_degrees().rem_euclid(360.0);
    let dh = (h - 30.0).abs().min(360.0 - (h - 30.0).abs());
    let mut p = (-(dh * dh) / (2.0 * 18.0 * 18.0)).exp();
    if ok.l < 0.18 || ok.l > 0.85 {
        p *= 0.2;
    }
    if ok.chroma < 0.02 || ok.chroma > 0.20 {
        p *= 0.3;
    }
    p
}

/// Quantize `img` into [`ImageMetrics`]. Pure decode + OKLCH stats, no
/// adjustment applied (the agent reasons about the *original* first).
pub fn analyze(img: &DynamicImage) -> ImageMetrics {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let mut n: u64 = 0;
    let mut sum_l = 0.0f64;
    let mut sum_l2 = 0.0f64;
    let mut min_l = 1.0f32;
    let mut max_l = 0.0f32;
    let mut hist = [0u64; 256]; // L 直方图（判中位数用）
    let mut bright_area: u64 = 0; // L>0.6 的像素数（亮部面积占比，影调判断用）
    let mut dark_area: u64 = 0;   // L<0.2 的像素数（暗部面积占比）
    let mut sum_c = 0.0f64;
    let mut cos_acc = 0.0f64; // hue circular mean accumulator (weighted by C)
    let mut sin_acc = 0.0f64;
    let mut hue_bin_c = [0.0f64; 8];
    let mut hue_bin_n = [0u64; 8];
    let mut skin_n = 0u64;
    let mut skin_sum_l = 0.0f64;
    let mut skin_sum_c = 0.0f64;
    let mut skin_cos = 0.0f64;
    let mut skin_sin = 0.0f64;
    let mut hi_clip = 0u64;
    let mut lo_clip = 0u64;
    let mut gamut_clip = 0u64;
    let mut max_c = 0.0f32;

    for (_x, _y, pixel) in rgb.enumerate_pixels() {
        let ok = ok_of(pixel.0);
        n += 1;
        let l = ok.l;
        let lb = (l * 255.0).clamp(0.0, 255.0) as usize;
        hist[lb] += 1;
        if l > 0.6 { bright_area += 1; }
        if l < 0.2 { dark_area += 1; }
        sum_l += l as f64;
        sum_l2 += (l as f64) * (l as f64);
        if l < min_l {
            min_l = l;
        }
        if l > max_l {
            max_l = l;
        }
        let c = ok.chroma;
        sum_c += c as f64;
        if c > max_c {
            max_c = c;
        }
        let hdeg = ok.hue.into_degrees().rem_euclid(360.0);
        let hr = hdeg.to_radians();
        cos_acc += (c as f64) * (hr.cos() as f64);
        sin_acc += (c as f64) * (hr.sin() as f64);
        let bin = ((hdeg / 45.0) as usize).min(7);
        hue_bin_c[bin] += c as f64;
        hue_bin_n[bin] += 1;

        let sp = skin_prob(&ok);
        if sp > 0.5 {
            skin_n += 1;
            skin_sum_l += l as f64;
            skin_sum_c += c as f64;
            skin_cos += (c as f64) * (hr.cos() as f64);
            skin_sin += (c as f64) * (hr.sin() as f64);
        }

        let mx = pixel.0[0].max(pixel.0[1]).max(pixel.0[2]);
        let mn = pixel.0[0].min(pixel.0[1]).min(pixel.0[2]);
        if mx == 255 {
            hi_clip += 1;
        }
        if mn == 0 {
            lo_clip += 1;
        }
        if !in_gamut(ok) {
            gamut_clip += 1;
        }
    }

    let nf = n as f64;
    let mean_l = (sum_l / nf) as f32;

    // 直方图百分位（用中位数判基调，避免高 DR 图被亮部抬均值）
    let mut cum = 0u64;
    let mut p25_l = 0.0f32;
    let mut median_l = 0.0f32;
    let mut p75_l = 0.0f32;
    let mut got_p25 = false;
    let mut got_med = false;
    for (b, &c) in hist.iter().enumerate() {
        cum += c;
        let frac = cum as f64 / nf;
        let lv = (b as f32 + 0.5) / 256.0;
        if !got_p25 && frac >= 0.25 {
            p25_l = lv;
            got_p25 = true;
        }
        if !got_med && frac >= 0.50 {
            median_l = lv;
            got_med = true;
        }
        if frac >= 0.75 {
            p75_l = lv;
            break;
        }
    }
    let var_l = (sum_l2 / nf) - (sum_l / nf) * (sum_l / nf);
    let std_l = var_l.max(0.0).sqrt() as f32;
    let mean_c = (sum_c / nf) as f32;

    let mean_h_deg = if cos_acc == 0.0 && sin_acc == 0.0 {
        0.0
    } else {
        let a = sin_acc.atan2(cos_acc);
        (a * 180.0 / std::f64::consts::PI).rem_euclid(360.0) as f32
    };
    // peakiness: concentration of chroma-weighted hue vs uniform spread.
    let r = (cos_acc * cos_acc + sin_acc * sin_acc).sqrt() / sum_c.max(1e-9);
    let hue_peakiness = r as f32;

    let mut per_hue = [0.0f32; 8];
    for i in 0..8 {
        per_hue[i] = if hue_bin_n[i] > 0 {
            (hue_bin_c[i] / hue_bin_n[i] as f64) as f32
        } else {
            0.0
        };
    }

    let skin_ratio = skin_n as f32 / n as f32;
    let skin_mean_c = if skin_n > 0 {
        (skin_sum_c / skin_n as f64) as f32
    } else {
        0.0
    };
    let skin_mean_h = if skin_n > 0 && (skin_cos != 0.0 || skin_sin != 0.0) {
        let a = skin_sin.atan2(skin_cos);
        (a * 180.0 / std::f64::consts::PI).rem_euclid(360.0) as f32
    } else {
        0.0
    };
    let skin_mean_l = if skin_n > 0 {
        (skin_sum_l / skin_n as f64) as f32
    } else {
        0.0
    };

    let bright_area_pct = (bright_area as f64 / nf * 100.0) as f32;
    let dark_area_pct = (dark_area as f64 / nf * 100.0) as f32;

    ImageMetrics {
        width: w,
        height: h,
        tone: ToneMetrics {
            mean_l,
            std_l,
            min_l,
            max_l,
            median_l,
            p25_l,
            p75_l,
            bright_area_pct,
            dark_area_pct,
        },
        color: ColorMetrics {
            mean_c,
            mean_h_deg,
            hue_peakiness,
            per_hue_chroma: per_hue,
        },
        skin: SkinMetrics {
            ratio: skin_ratio,
            mean_c: skin_mean_c,
            mean_h_deg: skin_mean_h,
            mean_l: skin_mean_l,
        },
        exposure: ExposureMetrics {
            highlight_clip_pct: (hi_clip as f64 / nf * 100.0) as f32,
            shadow_clip_pct: (lo_clip as f64 / nf * 100.0) as f32,
        },
        gamut: GamutMetrics {
            clip_pct: (gamut_clip as f64 / nf * 100.0) as f32,
            max_c,
        },
        cast: CastMetrics {
            hue_deg: mean_h_deg,
            chroma: mean_c,
        },
        dynamic_range: max_l - min_l,
    }
}
