//! Color Engine — 色彩引擎（v0.4 核心新增）
//!
//! 与 `tonemap.rs`（影调引擎）对称，管所有颜色相关的自动校正：
//! - 读取原图指标 → 7 类场景分类 → 每类各自规则
//! - 记忆色拉正（皮肤/天空/草木/绿色）
//! - 数码传感器固有色校正（数码黄→橙、数码蓝→天蓝）
//! - 暗部彩度目标地板（消除伪色）
//! - 整体彩度/鲜艳度目标
//!
//! 全部**不新增 UI 按钮**，只提升一键中性 / 参考匹配的颜色质量。

use crate::analyze::ImageMetrics;
use crate::pipeline::{Adjustments, ColorGrade, WhiteBalance};
use crate::tonemap::{Key, Tonality};
use image::DynamicImage;
use palette::{IntoColor, LinSrgb, OklabHue, Oklch};

// ── 7 种场景类型 ──────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SceneType {
    /// 标准中调（median 0.45-0.55, 正常色饱和度）
    NormalMid,
    /// 暖调/日落（cast_hue 25-55°, 整体偏黄暖）
    GoldenHour,
    /// 阴天/冷调（cast_hue 190-270°, 低彩度）
    Overcast,
    /// 浓色/高饱和（mean_c > 0.18）
    Vivid,
    /// 平淡/低反差（std_l < 0.12, mean_c < 0.07）
    FlatLowContrast,
    /// 夜景/人工光（median < 0.35, multi-peak hue）
    Night,
    /// 极端高反差（DR > 0.60, 剪影/长调）
    Extreme,
}

// ── 场景规则（每类三个参数） ─────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct SceneRules {
    /// 曝光/影调大胆度 0..2（1=标准）
    pub exposure_factor: f32,
    /// 色彩/校正大胆度 0..2（1=标准）
    pub color_factor: f32,
    /// 护栏紧度 0=松 / 1=标准 / 2=严
    pub guard_level: u8,
}

/// 按场景类型返回规则表（不在循环内，一次查表）
pub const fn scene_rules(st: SceneType) -> SceneRules {
    match st {
        SceneType::NormalMid => SceneRules {
            exposure_factor: 1.0,
            color_factor: 1.0,
            guard_level: 1,
        },
        SceneType::GoldenHour => SceneRules {
            exposure_factor: 0.8,
            color_factor: 1.3,
            guard_level: 0,
        },
        SceneType::Overcast => SceneRules {
            exposure_factor: 1.1,
            color_factor: 0.9,
            guard_level: 1,
        },
        SceneType::Vivid => SceneRules {
            exposure_factor: 1.0,
            color_factor: 0.7,
            guard_level: 2,
        },
        SceneType::FlatLowContrast => SceneRules {
            exposure_factor: 1.6,
            color_factor: 1.7,
            guard_level: 2,
        },
        SceneType::Night => SceneRules {
            exposure_factor: 0.4,
            color_factor: 0.5,
            guard_level: 2,
        },
        SceneType::Extreme => SceneRules {
            exposure_factor: 1.4,
            color_factor: 0.8,
            guard_level: 2,
        },
    }
}

// ── 色彩指标（analyze_color 的输出） ──────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorMetrics {
    /// 整体平均彩度
    pub mean_c: f32,
    /// 暗部平均彩度（L < 0.25），高则可能有噪点/伪色
    pub shadow_c: f32,
    /// 偏色方向 (0-360) 与强度
    pub cast_hue: f32,
    pub cast_chroma: f32,
    /// 平均色相（整图加权）
    pub mean_h: f32,
    /// 天空在图中占比（0..1）
    pub sky_ratio: f32,
    /// 草木/绿色在图中占比（0..1）
    pub green_ratio: f32,
    /// 数码黄偏离量：非暖图 = 0；暖图 = mean_h - 调整目标
    pub digital_yellow_delta: f32,
    /// 数码蓝偏离量：有天空时 = mean_sky_h - 目标天蓝
    pub digital_blue_delta: f32,
    /// 场景类型
    pub scene: SceneType,
}

// ── 色彩计划（color_plan 的输出，像素循环消费） ───────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorPlan {
    /// 白平衡（复用 auto_neutral_balance 的成果）
    pub wb: WhiteBalance,
    /// 全局鲜艳度目标 (0..0.5)
    pub vibrance_target: f32,
    /// 全局饱和度目标 (0.5..2.0)
    pub saturation_target: f32,
    /// 暗部彩度上限（L < 0.25 像素的 C 不超过此值）
    pub shadow_chroma_cap: f32,
    /// 温暖补偿强度 (0..1)：数码黄→橙的校正量
    pub warm_boost: f32,
    /// 天空补偿强度 (0..1)：数码蓝→天蓝的校正量
    pub sky_boost: f32,
    /// 绿色补偿强度 (0..1)：草木绿色校正量
    pub green_boost: f32,
    /// 荧光灯/室内绿偏色修正 (0..1)
    pub fluorescent_fix: f32,
    /// 阴天冷蓝暖化补偿 (0..1)
    pub overcast_warm: f32,
    /// 色彩校正的整体强度（由 strength 缩放）
    pub strength: f32,
}

impl Default for ColorPlan {
    fn default() -> Self {
        Self {
            wb: WhiteBalance {
                temp: 0.0,
                tint: 0.0,
            },
            vibrance_target: 0.0,
            saturation_target: 1.0,
            shadow_chroma_cap: 0.04,
            warm_boost: 0.0,
            sky_boost: 0.0,
            green_boost: 0.0,
            fluorescent_fix: 0.0,
            overcast_warm: 0.0,
            strength: 1.0,
        }
    }
}

// ── 核心函数 ─────────────────────────────────────────────

/// 对一张图做色彩分析，输出 ColorMetrics（一次预计算，~0.1ms @ 1280px）
pub fn analyze_color(img: &DynamicImage, m: &ImageMetrics) -> ColorMetrics {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let raw = rgb.into_raw();
    let n = (raw.len() / 3) as usize;
    let low_c_thr: f32 = 0.02;

    let mut mean_c = 0.0f64;
    let mut count = 0u64;
    let mut shadow_c_sum = 0.0f64;
    let mut shadow_n = 0u64;
    let mut cast_a = 0.0f64;
    let mut cast_b = 0.0f64;
    let mut cast_n = 0u64;
    let mut h_sin = 0.0f64;
    let mut h_cos = 0.0f64;
    let mut sky_pixels = 0u64;
    let mut green_pixels = 0u64;
    let mut sky_h_sum = 0.0f64;
    let mut green_h_sum = 0.0f64;

    for i in 0..n {
        let r = raw[3 * i];
        let g = raw[3 * i + 1];
        let b = raw[3 * i + 2];
        let lin = LinSrgb::new(srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
        let ok: Oklch<f32> = lin.into_color();
        let l = ok.l;
        let c = ok.chroma;
        let h = ok.hue.into_positive_degrees();

        mean_c += c as f64;
        count += 1;

        // 暗部彩度
        if l < 0.25 {
            shadow_c_sum += c as f64;
            shadow_n += 1;
        }

        // 低彩度像素的 a/b → cast 方向
        if c < low_c_thr {
            let a = ok.hue.into_positive_degrees().to_radians().cos() * c;
            let b = ok.hue.into_positive_degrees().to_radians().sin() * c;
            cast_a += a as f64;
            cast_b += b as f64;
            cast_n += 1;
        }

        // 色相统计（circular mean using sin/cos）
        let h_r = h.to_radians();
        h_sin += h_r.sin() as f64;
        h_cos += h_r.cos() as f64;

        // 天空检测：hue 210-255°, chroma 0.02-0.14, L > 0.35
        if h >= 210.0 && h <= 255.0 && c > 0.02 && c < 0.14 && l > 0.35 {
            sky_pixels += 1;
            sky_h_sum += h as f64;
        }

        // 草木检测：hue 80-160°, chroma > 0.04, L > 0.25
        if h >= 80.0 && h <= 160.0 && c > 0.04 && l > 0.25 {
            green_pixels += 1;
            green_h_sum += h as f64;
        }
    }

    let nf = count as f32;
    let mean_c_val = (mean_c / nf as f64) as f32;
    let shadow_c_val = if shadow_n > 0 {
        (shadow_c_sum / shadow_n as f64) as f32
    } else {
        0.0
    };

    // cast 方向
    let (cast_hue, cast_chroma) = if cast_n > 0 {
        let ca = (cast_a / cast_n as f64) as f32;
        let cb = (cast_b / cast_n as f64) as f32;
        let cc = (ca * ca + cb * cb).sqrt();
        let ch = if cc > 0.001 {
            (-cb).atan2(-ca).to_degrees().rem_euclid(360.0)
        } else {
            0.0
        };
        (ch, cc)
    } else {
        (0.0, 0.0)
    };

    // 平均色相
    let mean_h_rad_val = h_sin.atan2(h_cos);
    let mean_h_val = mean_h_rad_val.to_degrees().rem_euclid(360.0) as f32;

    // 天空/草木比例
    let total = count as f32;
    let sky_ratio = sky_pixels as f32 / total.max(1.0);
    let green_ratio = green_pixels as f32 / total.max(1.0);
    let mean_sky_h = if sky_pixels > 0 {
        (sky_h_sum / sky_pixels as f64) as f32
    } else {
        0.0
    };
    let mean_green_h = if green_pixels > 0 {
        (green_h_sum / green_pixels as f64) as f32
    } else {
        0.0
    };

    // 场景分类
    let scene = classify_scene(m, mean_c_val, cast_hue, cast_chroma, sky_ratio, green_ratio);

    // 数码黄偏离
    let digital_yellow_delta = if scene == SceneType::GoldenHour {
        // 暖场景：目标橙 hue 约 35-45°，当前黄 hue 50-70° 则应补偿
        let warm_target = 40.0;
        let diff = mean_h_val - warm_target;
        if diff > 5.0 {
            diff * 0.3
        } else {
            0.0
        } // 只对显著黄的做补偿
    } else {
        0.0
    };

    // 数码蓝偏离
    let digital_blue_delta = if sky_pixels > 0 {
        let sky_target = 240.0; // 目标天蓝
        let diff = mean_sky_h - sky_target;
        if diff.abs() > 5.0 && diff < 0.0 {
            -diff * 0.25
        } else {
            0.0
        }
        // 只对显著偏冷的天空做补偿（向暖拉）
    } else {
        0.0
    };

    ColorMetrics {
        mean_c: mean_c_val,
        shadow_c: shadow_c_val,
        cast_hue,
        cast_chroma,
        mean_h: mean_h_val,
        sky_ratio,
        green_ratio,
        digital_yellow_delta,
        digital_blue_delta,
        scene,
    }
}

/// 7 类场景分类
fn classify_scene(
    m: &ImageMetrics,
    mean_c: f32,
    cast_hue: f32,
    cast_chroma: f32,
    sky_ratio: f32,
    green_ratio: f32,
) -> SceneType {
    let key = if m.tone.median_l < 0.38 {
        Key::Low
    } else if m.tone.median_l > 0.62 {
        Key::High
    } else {
        Key::Mid
    };

    // 极端高反差（剪影/全长调）
    if m.dynamic_range > 0.60 {
        return SceneType::Extreme;
    }

    // 夜景/人工光：低 median + 高 cast_chroma + 低 sky
    if key == Key::Low && m.tone.median_l < 0.35 && cast_chroma > 0.04 && sky_ratio < 0.05 {
        return SceneType::Night;
    }

    // 暖调/日落：cast_hue 在 25-55° 且 cast 有一定强度
    if cast_hue >= 25.0 && cast_hue <= 55.0 && cast_chroma > 0.03 {
        return SceneType::GoldenHour;
    }

    // 阴天/冷调：cast_hue 在 190-270° 或 mean_c 极低
    if (cast_hue >= 190.0 && cast_hue <= 270.0) || mean_c < 0.06 {
        return SceneType::Overcast;
    }

    // 浓色
    if mean_c > 0.18 {
        return SceneType::Vivid;
    }

    // 平淡低反差
    if m.tone.std_l < 0.12 && mean_c < 0.10 {
        return SceneType::FlatLowContrast;
    }

    SceneType::NormalMid
}

/// 生成色彩计划（被 auto.rs 调用，结果写入 Adjustments）
pub fn color_plan(cm: &ColorMetrics, rules: &SceneRules, strength: f32) -> ColorPlan {
    let cf = rules.color_factor * strength;

    let vibrance_target = match cm.scene {
        SceneType::FlatLowContrast => (0.15 * cf).min(0.40),
        SceneType::Vivid => (0.0 * cf).min(0.10), // 浓图不加
        SceneType::Night => (0.05 * cf).min(0.15),
        SceneType::Overcast => (0.08 * cf).min(0.30),
        SceneType::GoldenHour => (0.06 * cf).min(0.20),
        SceneType::Extreme => (0.04 * cf).min(0.15),
        SceneType::NormalMid => (0.08 * cf).min(0.25),
    };

    let saturation_target = match cm.scene {
        SceneType::Vivid => (1.0 - (0.08 * cf).min(0.20)), // 浓色降饱和
        SceneType::FlatLowContrast => (1.0 + (0.06 * cf).min(0.15)),
        _ => 1.0,
    };

    let shadow_chroma_cap = match cm.scene {
        SceneType::Extreme => 0.03, // 高反差：极严
        SceneType::Night => 0.04,
        SceneType::Vivid => 0.04,
        _ => 0.05,
    };

    let warm_boost = if cm.scene == SceneType::GoldenHour {
        (0.35 * cf).min(0.60)
    } else {
        0.0
    };

    let sky_boost = if cm.sky_ratio > 0.03 {
        (0.30 * cf).min(0.55)
    } else {
        0.0
    };

    let green_boost = if cm.green_ratio > 0.05 && cm.scene != SceneType::Night {
        (0.30 * cf).min(0.55) // ↑0.20→0.30
    } else {
        0.0
    };

    // 荧光灯检测：室内中灰绿偏(hue 140-175°, 低彩)
    let fluorescent_fix = if matches!(cm.scene, SceneType::NormalMid | SceneType::Overcast)
        && cm.cast_hue > 135.0
        && cm.cast_hue < 180.0
        && cm.cast_chroma > 0.01
        && cm.cast_chroma < 0.06
    {
        (0.40 * cf).min(0.70)
    } else {
        0.0
    };

    // 阴天冷蓝：偏蓝但饱和度低(logical: overcast场景或mean_c < 0.08且偏蓝)
    let overcast_warm = if cm.scene == SceneType::Overcast
        || (cm.cast_hue > 220.0
            && cm.cast_hue < 280.0
            && cm.cast_chroma > 0.008
            && cm.cast_chroma < 0.05)
    {
        (0.25 * cf).min(0.45)
    } else {
        0.0
    };

    ColorPlan {
        wb: WhiteBalance::default(),
        vibrance_target,
        saturation_target,
        shadow_chroma_cap,
        warm_boost,
        sky_boost,
        green_boost,
        fluorescent_fix,
        overcast_warm,
        strength,
    }
}

// ── 像素级应用函数（在 pipeline render() 循环内调用） ──────

/// 在 OKLCH 像素循环内调用：应用色彩引擎的单像素修正（v2 升级版）
#[inline]
pub fn apply_color_correction(oklch: &mut Oklch<f32>, plan: &ColorPlan, _orig_c: f32) {
    let h = oklch.hue.into_positive_degrees();
    let c = oklch.chroma;
    let l = oklch.l;

    // ═══ 1. 天空记忆色修正 ═══
    // 数码蓝→天蓝：传感器拍天空偏紫/偏冷蓝，拉向记忆中的青蓝(hue 238-250°)
    if plan.sky_boost > 0.0 && h >= 205.0 && h <= 265.0 && c > 0.015 && c < 0.18 && l > 0.30 {
        let sky_prob = sky_probability(h, c, l);
        if sky_prob > 0.0 {
            let w = plan.sky_boost * sky_prob;
            let target_h = 242.0;
            let mut dh = target_h - h;
            if dh > 180.0 {
                dh -= 360.0;
            } else if dh < -180.0 {
                dh += 360.0;
            }
            oklch.hue = palette::OklabHue::from_degrees(
                (h + dh * w * 0.65).rem_euclid(360.0), // ↑0.4→0.65
            );
            // 天空彩度：偏紫(purple cast)时降c去紫，正常的偏冷蓝则轻提
            if h < 215.0 || h > 255.0 {
                oklch.chroma = c * (1.0 - w * 0.15); // 偏色天空降纯
            } else {
                oklch.chroma = (c + w * 0.015).min(0.12); // 正常蓝天轻提
            }
        }
    }

    // ═══ 2. 草木绿色记忆色修正 ═══
    if plan.green_boost > 0.0 && h >= 80.0 && h <= 160.0 && c > 0.03 && l > 0.20 {
        let green_prob = green_probability(h, c, l);
        if green_prob > 0.0 {
            let w = plan.green_boost * green_prob;
            let target_green_h = 118.0;
            if (h - target_green_h).abs() > 12.0 {
                let mut dh = target_green_h - h;
                if dh > 180.0 {
                    dh -= 360.0;
                } else if dh < -180.0 {
                    dh += 360.0;
                }
                oklch.hue = palette::OklabHue::from_degrees(
                    (h + dh * w * 0.55).rem_euclid(360.0), // ↑0.35→0.55
                );
            }
            // 脏绿（偏高c + 偏色hue）→ 降c洗去数码黄绿脏感
            if c > 0.10 && (h < 95.0 || h > 145.0) {
                oklch.chroma = c * (1.0 - w * 0.25); // ↑0.15→0.25
            }
            // 暗绿(l<0.35)轻提亮度，透光感
            if l < 0.35 && c > 0.05 {
                oklch.l = (l + w * 0.03).min(0.40);
            }
        }
    }

    // ═══ 3. 数码黄→橙补偿 ═══
    // 日落/暖阳场景中传感器记录的黄(hue 48-72°)拉向眼见的橙(hue 35-45°)
    if plan.warm_boost > 0.0 && h >= 45.0 && h <= 78.0 && c > 0.03 {
        let target_warm_h = 38.0;
        let mut dh = target_warm_h - h;
        if dh.abs() > 1.5 {
            oklch.hue = palette::OklabHue::from_degrees(
                (h + dh * plan.warm_boost * 0.60).rem_euclid(360.0), // ↑0.3→0.6
            );
            // 提彩度+提亮度：让屎黄变成干净温暖的橙色
            oklch.chroma = (c + plan.warm_boost * 0.04).min(0.20);
            if l > 0.25 && l < 0.65 {
                oklch.l = (l + plan.warm_boost * 0.03).min(0.68);
            }
        }
    }

    // ═══ 4. NEW: 荧光灯/室内绿偏色修正 ═══
    // 数码相机在荧光灯下常把中灰色拍成黄绿色(hue 140-175°)
    if plan.fluorescent_fix > 0.0 && h >= 135.0 && h <= 180.0 && c < 0.06 && l > 0.25 {
        // 低彩绿偏→向中性灰拉回
        let w = plan.fluorescent_fix * (1.0 - c / 0.06);
        // 降chroma（去绿），微提升亮度（荧光灯下偏暗）
        oklch.chroma = (c * (1.0 - w * 0.5)).max(0.001);
        oklch.l = (l + w * 0.03).min(0.75);
    }

    // ═══ 5. NEW: 阴天冷蓝补偿 ═══
    // 阴天的中性灰/白偏蓝偏冷(hue 230-270°)，轻微暖化但不越界
    if plan.overcast_warm > 0.0 && h >= 225.0 && h <= 275.0 && c < 0.04 {
        if l > 0.20 && l < 0.85 {
            let w = plan.overcast_warm * (1.0 - c / 0.04);
            // 微暖化：蓝→蓝绿方向轻微偏移
            oklch.hue = palette::OklabHue::from_degrees((h - 8.0 * w).rem_euclid(360.0));
            // 轻提亮度和暖色调感觉
            oklch.l = (l + w * 0.02).min(0.88);
        }
    }

    // ═══ 6. 暗部彩度地板 ═══
    // 防暗部噪点/伪色——平滑过渡而非硬切
    if l < 0.28 && c > plan.shadow_chroma_cap {
        let t = (0.28 - l) / 0.28; // 越暗衰减越多
        oklch.chroma = c * (1.0 - t * 0.6) + plan.shadow_chroma_cap * t * 0.6;
    }
}

/// 天空概率 (0..1) —— hue gaussian centered on 240°, chroma gate, lightness gate
#[inline]
fn sky_probability(h: f32, c: f32, l: f32) -> f32 {
    // hue gaussian
    let dh = (h - 240.0).abs().min(360.0 - (h - 240.0).abs());
    if dh > 50.0 {
        return 0.0;
    }
    let hue_w = (-(dh * dh) / (2.0 * 20.0 * 20.0)).exp();

    // chroma gate: 蓝天彩度适中
    if c < 0.02 || c > 0.16 {
        return 0.0;
    }
    let chroma_w = if c < 0.04 { (c - 0.02) / 0.02 } else { 1.0 };

    // lightness gate: 天空亮度适中
    if l < 0.30 || l > 0.92 {
        return 0.0;
    }
    let l_w = ((l - 0.30) / 0.20).min(1.0) * (1.0 - ((l - 0.80) / 0.12).max(0.0));

    (hue_w * chroma_w * l_w).clamp(0.0, 1.0)
}

/// 草木概率 (0..1) —— hue centered on 120°, chroma gate, lightness gate
#[inline]
fn green_probability(h: f32, c: f32, l: f32) -> f32 {
    let dh = (h - 120.0).abs().min(360.0 - (h - 120.0).abs());
    if dh > 55.0 {
        return 0.0;
    }
    let hue_w = (-(dh * dh) / (2.0 * 22.0 * 22.0)).exp();

    if c < 0.03 || c > 0.25 {
        return 0.0;
    }
    let chroma_w = if c < 0.06 { (c - 0.03) / 0.03 } else { 1.0 };

    if l < 0.18 || l > 0.90 {
        return 0.0;
    }
    let l_w = ((l - 0.18) / 0.15).min(1.0) * (1.0 - ((l - 0.80) / 0.10).max(0.0));

    (hue_w * chroma_w * l_w).clamp(0.0, 1.0)
}

// Tiny helpers inlined from pipeline
fn srgb_to_linear(c: u8) -> f32 {
    let v = c as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
