//! Algorithmic one-click colour correction + film-style presets.
//!
//! No AI, no neural net, no extra dependencies: just OKLab/OKLCH statistics
//! and conservative hand-tuned looks. Designed to be safe enough to run
//! blindly and still produce a usable starting point that the user can
//! continue to tweak.

use crate::color_engine::{analyze_color, color_plan, scene_rules, ColorPlan};
use crate::pipeline::{
    Adjustments, ColorGrade, DefakeColor, Grade, HslRegions, SkinTone, ToneMapMode,
    WhiteBalance,
};
use crate::tonemap::classify_tonality;
use image::DynamicImage;
use palette::{IntoColor, LinSrgb, Oklab, Srgb};

/// Result of the one-click neutral-balance analysis.
#[derive(Clone, Debug, Default)]
pub struct AutoBalance {
    /// White-balance offset that neutralises the detected cast.
    pub wb: WhiteBalance,
    /// Recommended light-ratio correction (natural contrast roll-off).
    pub light_ratio: f32,
    /// Recommended film-curve amount.
    pub film_curve: f32,
    /// Recommended exposure nudge (EV).
    pub exposure_ev: f32,
    /// Blend strength: 1.0 = full correction, 0.0 = original only.
    /// The public function uses 0.8 so 20 % of the original seeps through.
    pub mix: f32,
    /// Human-readable summary (for the GUI status bar).
    pub summary: String,
    /// Whether the scene-aware colour/light-ratio compensation is applied.
    pub smart_compensation: bool,
    /// Scene analysis used for the compensation.
    pub scene: SceneStyle,
    /// Colour-grade compensation (added if smart_compensation is on).
    pub comp_color: ColorGrade,
    /// Grade compensation (added if smart_compensation is on).
    pub comp_grade: Grade,
    /// HSL compensation (added if smart_compensation is on).
    pub comp_hsl: HslRegions,
    /// 色彩引擎（去假色/数码颜色补偿/场景感知记忆色）
    pub color_plan: Option<ColorPlan>,
}

impl AutoBalance {
    /// Convert into a full `Adjustments` ready for rendering.
    pub fn to_adjustments(self) -> Adjustments {
        let mut color = ColorGrade::default();
        let mut grade = Grade {
            film_curve: self.film_curve,
            light_ratio: self.light_ratio,
            ..Default::default()
        };
        let mut hsl = HslRegions::default();
        if self.smart_compensation {
            color.vibrance += self.comp_color.vibrance;
            color.saturation += self.comp_color.saturation;
            grade.film_curve += self.comp_grade.film_curve;
            grade.light_ratio += self.comp_grade.light_ratio;
            for i in 0..8 {
                hsl.sat_mult[i] += self.comp_hsl.sat_mult[i];
            }
        }
        Adjustments {
            exposure_ev: self.exposure_ev,
            tone_map: ToneMapMode::Agx,
            defake: DefakeColor::on(),
            grade,
            white_balance: self.wb,
            color,
            hsl,
            skin: SkinTone::default(),
            zones: Default::default(),
            geometry: Default::default(),
            detail: Default::default(),
            advanced: Default::default(),
            color_plan: self.color_plan.clone(),
            mix: self.mix,
        }
    }
}

/// Rough classification of the image's tonal distribution.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum HistogramType {
    /// Bright, airy, mostly above mid-gray.
    HighKey,
    /// Dark, moody, mostly below mid-gray.
    LowKey,
    /// Large spread from deep shadows to bright highlights.
    HighContrast,
    /// Narrow tonal range, flat / low contrast.
    Flat,
    /// No strong bias.
    #[default]
    Normal,
}

/// Scene description derived from the source image.
#[derive(Clone, Debug, Default)]
pub struct SceneStyle {
    pub histogram: HistogramType,
    /// Dominant 8-bin hue band (0 = red, 1 = orange, ... 7 = magenta).
    pub dominant_hue: Option<usize>,
    /// Second strongest 8-bin hue band.
    pub secondary_hue: Option<usize>,
    /// Mean OKLCH lightness.
    pub mean_l: f32,
    /// Mean OKLCH chroma (overall colourfulness).
    pub mean_c: f32,
    /// Standard deviation of lightness.
    pub std_l: f32,
}

/// One-click auto correction.
///
/// Strategy (two-pass):
/// 1. Pass 1 — analyze the ORIGINAL image to detect the colour cast and
///    compute WB correction + a rough dynamic-range light_ratio.
/// 2. Apply the WB correction to a downscaled copy, then re-analyse the
///    NEUTRALISED image so scene-adaptive compensation (vibrance, film_curve,
///    HSL sat_mult) is based on correct data, not cast-corrupted data.
/// 3. Keep 20 % of the original image (mix = 0.8) so the correction never
///    fully overwrites the source character.
pub fn auto_neutral_balance(img: &DynamicImage, smart_compensation: bool) -> AutoBalance {
    let rgb = img.to_rgb8();
    let n_total = rgb.width() as usize * rgb.height() as usize;
    if n_total == 0 {
        return AutoBalance {
            summary: "无法分析空图像".into(),
            ..Default::default()
        };
    }

    let mut hue_bin_n = [0usize; 8];
    let mut samples: Vec<Oklab<f32>> = Vec::with_capacity(n_total.min(65536));

    // Downsample very large images for speed: sample on a grid.
    let (w, h) = rgb.dimensions();
    let step = ((w.max(h) as f32 / 512.0).max(1.0).ceil()) as u32;

    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let px = rgb.get_pixel(x, y).0;
            let lin = Srgb::new(
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            )
            .into_linear();
            let oklab: Oklab<f32> = lin.into_color();
            let hdeg = oklab.a.atan2(oklab.b).to_degrees().rem_euclid(360.0);
            let bin = ((hdeg / 45.0) as usize).min(7);
            hue_bin_n[bin] += 1;
            samples.push(oklab);
        }
    }

    let n_samples = samples.len().max(1);
    let mut dominant_bin = 0usize;
    let mut dominant_count = 0usize;
    for (i, &c) in hue_bin_n.iter().enumerate() {
        if c > dominant_count {
            dominant_count = c;
            dominant_bin = i;
        }
    }
    let dominant_ratio = dominant_count as f32 / n_samples as f32;
    let exclude_dominant = dominant_ratio > 0.40;

    // Second pass: accumulate a/b from accepted samples.
    let mut sum_a = 0.0f64;
    let mut sum_b = 0.0f64;
    let mut accepted = 0usize;
    let mut min_l = 1.0f32;
    let mut max_l = 0.0f32;

    for ok in &samples {
        let l = ok.l;
        let c = ok.a.hypot(ok.b);
        let hdeg = ok.a.atan2(ok.b).to_degrees().rem_euclid(360.0);
        let bin = ((hdeg / 45.0) as usize).min(7);

        if l < min_l {
            min_l = l;
        }
        if l > max_l {
            max_l = l;
        }

        // Filter rules:
        // - near clipping has no reliable colour info
        // - near-neutral pixels don't tell us about the illuminant
        // - optionally ignore the biggest solid-colour region
        if l < 0.18 || l > 0.92 || c < 0.01 {
            continue;
        }
        if exclude_dominant && bin == dominant_bin {
            continue;
        }

        sum_a += ok.a as f64;
        sum_b += ok.b as f64;
        accepted += 1;
    }

    if accepted < 64 {
        // Not enough reliable samples; fall back to using everything except
        // extreme clips.
        sum_a = 0.0;
        sum_b = 0.0;
        accepted = 0;
        for ok in &samples {
            let l = ok.l;
            let c = ok.a.hypot(ok.b);
            if l < 0.10 || l > 0.98 || c < 0.005 {
                continue;
            }
            sum_a += ok.a as f64;
            sum_b += ok.b as f64;
            accepted += 1;
        }
    }

    let mean_a = if accepted > 0 {
        (sum_a / accepted as f64) as f32
    } else {
        0.0
    };
    let mean_b = if accepted > 0 {
        (sum_b / accepted as f64) as f32
    } else {
        0.0
    };

    // Convert residual cast to WB correction.
    // OKLab: +a = magenta, +b = yellow.
    // Our WB: +temp = warm (yellow), +tint = magenta.
    // So neutralise by applying the opposite sign.
    let k_temp = 4.0f32;
    let k_tint = 4.0f32;
    let temp = (-k_temp * mean_b).clamp(-0.55, 0.55);
    let tint = (-k_tint * mean_a).clamp(-0.45, 0.45);

    // Natural light-ratio / film-curve based on dynamic range.
    let dynamic_range = (max_l - min_l).max(0.05);
    let (light_ratio, film_curve, exposure_ev) = if dynamic_range > 0.72 {
        // High DR scene: open the ratio a little, add gentle film roll-off.
        (0.12, 0.08, 0.0)
    } else if dynamic_range > 0.50 {
        // Moderate DR: very subtle depth.
        (0.06, 0.05, 0.0)
    } else {
        // Low DR / flat scene: flatten slightly to avoid harsh contrast.
        (-0.05, 0.03, 0.05)
    };

    // Scene-aware compensation: re-measure the NEUTRALISED image so we base
    // decisions on correct chroma/contrast/hue distribution, not the original
    // cast-corrupted data.
    let (comp_color, comp_grade, comp_hsl) = if smart_compensation {
        render_neutralized_and_reanalyze(img, &WhiteBalance { temp, tint })
    } else {
        (ColorGrade::default(), Grade::default(), HslRegions::default())
    };

    let excluded_text = if exclude_dominant {
        format!("，忽略第 {} 号主导色相({:.0}%像素)", dominant_bin, dominant_ratio * 100.0)
    } else {
        String::new()
    };
    let comp_text: String = if smart_compensation {
        "，智能补偿启用（基于中性化后重测）".into()
    } else {
        "，智能补偿关闭".into()
    };

    // 色彩引擎：场景分类 + 数码偏色补偿 + 记忆色校正
    let color_plan = {
        let base_metrics = crate::analyze::analyze(img);
        let cm = analyze_color(img, &base_metrics);
        let rules = scene_rules(cm.scene);
        Some(color_plan(&cm, &rules, 0.8))
    };

    AutoBalance {
        wb: WhiteBalance { temp, tint },
        light_ratio,
        film_curve,
        exposure_ev,
        mix: 0.8,
        summary: format!(
            "自动中性化：色温 {:.2} / 色调 {:.2}{}，保留 20% 原图{}",
            temp, tint, excluded_text, comp_text
        ),
        smart_compensation,
        scene: SceneStyle::default(),
        comp_color,
        comp_grade,
        comp_hsl,
        color_plan,
    }
}

/// Render a downscaled version of the image with just the WB correction applied,
/// then re-analyse the neutralised result to compute scene-adaptive compensation.
/// This avoids the previous bug where compensation was based on cast-corrupted
/// original metrics (e.g. mean_c pulled low by a warm cast).
fn render_neutralized_and_reanalyze(
    img: &DynamicImage,
    wb: &WhiteBalance,
) -> (ColorGrade, Grade, HslRegions) {
    use palette::IntoColor as _;

    fn srgb_to_lin(u: u8) -> f32 {
        let c = u as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let step = ((w.max(h) as f32 / 256.0).max(1.0).ceil()) as u32;

    let mut corr_a = Vec::new();
    let mut corr_b = Vec::new();

    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let px = rgb.get_pixel(x, y).0;
            let mut lr = srgb_to_lin(px[0]);
            let mut lg = srgb_to_lin(px[1]);
            let mut lb = srgb_to_lin(px[2]);
            // Apply WB as linear RGB gains (same formula as pipeline.rs)
            let tr = 1.0 + wb.temp * 0.2;
            let tb = 1.0 - wb.temp * 0.2;
            let tg = 1.0 - wb.tint * 0.15;
            lr *= tr;
            lg *= tg;
            lb *= tb;
            let lin = LinSrgb::new(lr, lg, lb);
            let ok: Oklab<f32> = lin.into_color();
            corr_a.push(ok.a);
            corr_b.push(ok.b);
        }
    }

    let n_corr = corr_a.len().max(1);
    let mut mean_c = 0.0f64;
    let mut hue_bin = [0usize; 8];

    for i in 0..n_corr {
        let c = corr_a[i].hypot(corr_b[i]);
        let hdeg = corr_a[i].atan2(corr_b[i]).to_degrees().rem_euclid(360.0);
        let bin = ((hdeg / 45.0) as usize).min(7);
        hue_bin[bin] += 1;
        mean_c += c as f64;
    }

    let mean_c = (mean_c / n_corr as f64) as f32;

    // --- Very conservative compensation based on neutralised chroma ---
    let mut color = ColorGrade::default();
    let grade = Grade::default();
    let mut hsl = HslRegions::default();

    // Gentler vibrance tiers (was 0.10/0.06/0.03, now 0.06/0.03/0.01)
    if mean_c < 0.06 {
        color.vibrance = 0.06;
    } else if mean_c < 0.10 {
        color.vibrance = 0.03;
    } else if mean_c < 0.14 {
        color.vibrance = 0.01;
    }

    // Subtle dominant-hue sat preservation (was +0.05/+0.03, now +0.03/+0.02)
    let mut indexed: Vec<(usize, usize)> = hue_bin.iter().cloned().enumerate().collect();
    indexed.sort_by(|a, b| b.1.cmp(&a.1));
    if let Some(i) = indexed.first().filter(|x| x.1 > 0) {
        hsl.sat_mult[i.0] += 0.03;
    }
    if let Some(j) = indexed.get(1).filter(|x| x.1 > 0) {
        hsl.sat_mult[j.0] += 0.02;
    }

    (color, grade, hsl)
}



/// A named film-style preset.
#[derive(Clone, Debug)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub adj: Adjustments,
    /// true = 先跑引擎基线（tonal_adjustments），再叠加本 adj 作为风格增量。
    pub is_engine_based: bool,
}

/// Conservative, stable film-style presets. All values are intentionally mild
/// so they work as a starting point rather than a finished look.
pub fn film_presets() -> Vec<Preset> {
    vec![
        Preset {
            is_engine_based: false,
            id: "neutral_warm",
            name: "中性胶片·暖",
            description: "轻微暖调 + 柔和胶片曲线，适合日常/人像",
            adj: Adjustments {
                tone_map: ToneMapMode::Filmic,
                defake: DefakeColor::on(),
                grade: Grade {
                    film_curve: 0.10,
                    light_ratio: 0.08,
                    ..Default::default()
                },
                white_balance: WhiteBalance {
                    temp: 0.10,
                    tint: 0.0,
                },
                color: ColorGrade {
                    saturation: 1.02,
                    ..Default::default()
                },
                ..Default::default()
            },
        },
        Preset {
            is_engine_based: false,
            id: "neutral_cool",
            name: "中性胶片·冷",
            description: "轻微冷调 + 柔和胶片曲线，适合风景/清爽感",
            adj: Adjustments {
                tone_map: ToneMapMode::Filmic,
                defake: DefakeColor::on(),
                grade: Grade {
                    film_curve: 0.08,
                    light_ratio: 0.05,
                    ..Default::default()
                },
                white_balance: WhiteBalance {
                    temp: -0.08,
                    tint: 0.0,
                },
                color: ColorGrade {
                    saturation: 0.98,
                    ..Default::default()
                },
                ..Default::default()
            },
        },
        Preset {
            is_engine_based: true,
            id: "fuji",
            name: "富士色调",
            description: "清冷阴影 + 柔和肤色，典型 Fuji 负片感",
            adj: Adjustments {
                tone_map: ToneMapMode::Filmic,
                defake: DefakeColor::on(),
                grade: Grade {
                    film_curve: 0.08,
                    light_ratio: 0.06,
                    ..Default::default()
                },
                white_balance: WhiteBalance {
                    temp: -0.04,
                    tint: -0.03,
                },
                color: ColorGrade {
                    saturation: 0.96,
                    vibrance: 0.06,
                    split_shadow: 195.0,
                    split_highlight: 55.0,
                    ..Default::default()
                },
                hsl: {
                    let mut h = HslRegions::default();
                    h.sat_mult[3] = 1.08; // green
                    h.sat_mult[5] = 1.08; // blue
                    h.sat_mult[1] = 0.95; // orange slightly desat
                    h
                },
                ..Default::default()
            },
        },
        Preset {
            is_engine_based: false,
            id: "portra",
            name: "Portra 人像",
            description: "暖调肤色 + 低饱和 + 柔和 roll-off",
            adj: Adjustments {
                tone_map: ToneMapMode::Filmic,
                defake: DefakeColor::on(),
                grade: Grade {
                    film_curve: 0.10,
                    light_ratio: 0.05,
                    ..Default::default()
                },
                white_balance: WhiteBalance {
                    temp: 0.08,
                    tint: 0.02,
                },
                color: ColorGrade {
                    saturation: 0.96,
                    ..Default::default()
                },
                skin: SkinTone {
                    strength: 0.35,
                    hue_target: 35.0,
                    chroma_target: 0.06,
                    light_lift: 0.02,
                    yellow_reduce: 0.15,
                    ..Default::default()
                },
                ..Default::default()
            },
        },
        Preset {
            is_engine_based: false,
            id: "kodak_gold",
            name: "Kodak Gold",
            description: "暖金黄调 + 略高饱和，复古旅行感",
            adj: Adjustments {
                tone_map: ToneMapMode::Filmic,
                defake: DefakeColor::on(),
                grade: Grade {
                    film_curve: 0.12,
                    light_ratio: 0.10,
                    ..Default::default()
                },
                white_balance: WhiteBalance {
                    temp: 0.16,
                    tint: 0.02,
                },
                color: ColorGrade {
                    saturation: 1.06,
                    vibrance: 0.05,
                    ..Default::default()
                },
                hsl: {
                    let mut h = HslRegions::default();
                    h.light_mult[2] = 1.05; // yellow slightly brighter
                    h.sat_mult[2] = 1.10; // yellow more saturated
                    h.sat_mult[0] = 1.05; // red pop
                    h
                },
                ..Default::default()
            },
        },
        Preset {
            is_engine_based: false,
            id: "morandi",
            name: "莫兰迪·低调",
            description: "低饱和灰调 + 柔和低反差，高级莫兰迪静物感",
            adj: Adjustments {
                tone_map: ToneMapMode::Filmic,
                defake: DefakeColor::on(),
                grade: Grade {
                    film_curve: 0.06,
                    light_ratio: 0.04,
                    // 略降反差、微抬暗部形成灰调（莫兰迪的"蒙灰"质感）
                    contrast: -0.10,
                    shadow_lift: 0.06,
                    ..Default::default()
                },
                white_balance: WhiteBalance {
                    // 极轻的暖灰，避免纯冷或纯暖
                    temp: 0.03,
                    tint: 0.02,
                },
                color: ColorGrade {
                    // 大幅降饱和是莫兰迪的核心；vibrance 微负进一步压掉艳色
                    saturation: 0.80,
                    vibrance: -0.05,
                    ..Default::default()
                },
                hsl: {
                    let mut h = HslRegions::default();
                    // 把最容易"跳"的红/橙/黄/蓝再压一档，统一到灰粉灰绿灰蓝
                    h.sat_mult[0] = 0.85; // red
                    h.sat_mult[1] = 0.82; // orange
                    h.sat_mult[2] = 0.85; // yellow
                    h.sat_mult[3] = 0.88; // green
                    h.sat_mult[5] = 0.88; // blue
                    h
                },
                ..Default::default()
            },
        },
    ]
}
