//! 影调感知质量引擎（v0.3 核心交付物）
//!
//! 替代 `auto_color.rs` 里「无条件 `tone_map: Agx` + 按动态范围写死 `light_ratio`
//! 提亮」的 ad-hoc 路线——那条路线不看图是啥调子，一律套全局提亮，于是暗的夜景
//! /逆光被硬顶过曝毁图。
//!
//! 本模块做法（基于 RESEARCH-color-science-foundation.md 已证实方法）：
//! 1. `classify_tonality` —— 先用直方图把图认成十大影调之一（基调×调性×软硬 + 剪影）。
//! 2. `tonal_adjustments` —— 按影调设「正确的 tone_map + 曝光/对比目标」：
//!    - 低调（夜景/逆光/剪影）：**不整体提亮**，用保护暗部+高光的 rolloff 找回暗部细节；
//!    - 中调：平衡 S 曲线，两侧保护、自然衰变；
//!    - 高调（雪景/日系）：允许均匀乳白过曝/提亮。
//! 3. 颜色中和（白平衡 + 色彩补偿）**复用 `auto_neutral_balance` 的好部分**（用户实测"好"）。
//!
//! 护栏 `safe_neutral` 仅在引擎外层留一道薄地板，应几乎不触发。

use crate::advanced::Advanced;
use crate::analyze::ImageMetrics;
use crate::auto_color::auto_neutral_balance;
use crate::detail::Detail;
use crate::geometry::Geometry;
use crate::pipeline::{
    Adjustments, ColorGrade, DefakeColor, Grade, HslRegions, SkinTone, ToneMapMode, ZoneGrade,
};
use image::DynamicImage;

/// 基调（整体明暗，由直方图质心 mean_l 判）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Low,
    Mid,
    High,
}

/// 调性（明暗跨度，由真实动态范围 dynamic_range = max_l − min_l 判）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Short,
    Mid,
    Long,
}

/// 软硬（反差，由双侧死白占比判）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hardness {
    Soft,
    Hard,
}

/// 一张图的影调判定结果。
#[derive(Clone, Copy, Debug)]
pub struct Tonality {
    pub key: Key,
    pub span: Span,
    pub hard: Hardness,
    pub silhouette: bool,
    /// 中文影调名（如「低长调」「剪影」），便于 UI/日志展示。
    pub label: &'static str,
}

impl Tonality {
    /// 极端高反差：剪影 / 低调长调 / 高调长调（允许干净的近黑与乳白 shoulder）。
    pub fn is_extreme(&self) -> bool {
        self.silhouette
            || (self.key == Key::Low && self.span == Span::Long)
            || (self.key == Key::High && self.span == Span::Long)
    }
}

/// 十大影调分类（全由 `analyze()` 量化指标得出，无 AI、无阈值拍脑袋）。
///
/// - 基调（面积比例，摄影定义）：亮部面积 ≥55% 且死黑 <4% → 高调；暗部面积 ≥25% 且死白 <4% → 低调；其余中调。median_l 作兜底。
/// - 调性：`dynamic_range < 0.33 → 短`；`0.33–0.66 → 中`；`> 0.66 → 长`。
/// - 软硬：高光与阴影**两侧**都有显著死白 → 硬；否则软。
/// - 剪影特例：阴影死白占比高且最暗接近纯黑 → 主体纯黑轮廓。
pub fn classify_tonality(m: &ImageMetrics) -> Tonality {
    let dr = m.dynamic_range; // max_l − min_l，真实动态范围（与中点放置无关）

    // 基调用**面积比例**判（摄影标准：高调=暗部面积小、低调=亮部面积小，
    // 即"画面中明暗像素的比例"），而非单点中位数——中位数只代表中间那个像素，
    // 不反映"亮部占总面积多少"，易把"大面积亮+少量暗"误归中调、把"大面积暗+
    // 少量亮"误归中调。面积比例直接对应摄影定义（搜狗百科/SHUTTERCOACH/Fstoppers 一致）。
    //   高调：亮部面积 ≥55% 且 死黑面积 <4% → 画面大部分是亮的、暗部极少
    //   低调：暗部面积 ≥25% 且 死白面积 <4% → 画面大部分是暗的、亮部极少
    //   中间调：明暗比例适中（其余）
    // median_l 作为兜底：极端但面积判据不命中时（如亮背景+大暗主体）仍可归位。
    let t = &m.tone;
    let shadow_dead = m.exposure.shadow_clip_pct;
    let hi_dead = m.exposure.highlight_clip_pct;
    let is_high = (t.bright_area_pct >= 55.0 && shadow_dead < 4.0) || t.median_l > 0.66;
    let is_low = (t.dark_area_pct >= 25.0 && hi_dead < 4.0) || t.median_l < 0.36;
    let key = if is_high {
        Key::High
    } else if is_low {
        Key::Low
    } else {
        Key::Mid
    };

    let span = if dr < 0.33 {
        Span::Short
    } else if dr > 0.66 {
        Span::Long
    } else {
        Span::Mid
    };

    let hard = if m.exposure.highlight_clip_pct > 1.0 && m.exposure.shadow_clip_pct > 1.0 {
        Hardness::Hard
    } else {
        Hardness::Soft
    };

    // 剪影：大面积纯黑（阴影死白占比高 + 最暗接近 0）
    let silhouette = m.exposure.shadow_clip_pct > 6.0 && m.tone.min_l < 0.04;

    let label = match (key, span, hard, silhouette) {
        (_, _, _, true) => "剪影",
        (Key::Low, Span::Short, _, _) => "低短调",
        (Key::Low, Span::Mid, _, _) => "低中调",
        (Key::Low, Span::Long, _, _) => "低长调",
        (Key::Mid, Span::Short, _, _) => "中短调",
        (Key::Mid, Span::Mid, _, _) => "中中调",
        (Key::Mid, Span::Long, _, _) => "中长调",
        (Key::High, Span::Short, _, _) => "高短调",
        (Key::High, Span::Mid, _, _) => "高中调",
        (Key::High, Span::Long, _, _) => "高长调",
    };

    Tonality {
        key,
        span,
        hard,
        silhouette,
        label,
    }
}

/// 按影调产出「修好看」的调整参数。
///
/// 颜色中和（白平衡 + 色彩补偿）复用 `auto_neutral_balance` 的好部分；
/// 影调/曝光/对比由本函数按分类决定，替代无条件提亮。
///
/// `strength`（弱 0.5 / 中 1.0 / 强 1.8）只缩放「增强类」字段（对比 / 胶片 rolloff /
/// 暗部找回 / 鲜艳度 / 去雾），**保护类**（曝光位置 / 白平衡 / 色调映射 / mix）不缩放 —
/// 这正是用户要的「按影调类型决定哪里加强、哪里保护」：低调不加全局反差只找回暗部、
/// 高调保护高光这套按影调的逻辑对所有档位都成立，强档只是把针对该影调的修正放大。
pub fn tonal_adjustments(
    img: &DynamicImage,
    smart_compensation: bool,
    strength: f32,
) -> Adjustments {
    let base = crate::analyze::analyze(img);
    let bal = auto_neutral_balance(img, smart_compensation);
    let t = classify_tonality(&base);

    // —— 颜色中和：用户实测"好"的部分，原样保留 ——
    let wb = bal.wb;
    let mut color = ColorGrade::default();
    let mut hsl = HslRegions::default();
    if smart_compensation {
        color.vibrance = bal.comp_color.vibrance;
        color.saturation = bal.comp_color.saturation;
        hsl = bal.comp_hsl;
    }
    // 欠饱和（灰扑扑）照片补一点鲜艳度，让"修过"肉眼可见（受 guardrail 饱和度上限保护）。
    // 增量随强度缩放：弱档轻补、强档狠补，但「基础颜色中和」(bal.comp_color) 始终保留。
    if base.color.mean_c < 0.09 {
        color.vibrance = (color.vibrance + 0.18 * strength).min(0.5);
    } else if base.color.mean_c < 0.13 {
        color.vibrance = (color.vibrance + 0.08 * strength).min(0.4);
    }
    // 低反差/发灰照片加去雾通透感，提升"质感"
    let mut dehaze = 0.0f32;
    if base.tone.std_l < 0.14 {
        dehaze = 0.10 * strength;
    }

    // —— tone_map：用 None（identity）让亮度完全由 exposure_ev 精确控制，不偷挪；
    //    胶片感 / rolloff 交给 grade（film_curve / light_ratio / shadow_lift，均 mean-保持，
    //    只塑形不位移）。这是"治本"可控性的关键——之前 Agx 无条件把 mean_l 抬 0.2~0.3，
    //    任何 exposure_ev 都压不住，正是反复崩图的根。
    let tone_map = ToneMapMode::None;

    // —— 目标 median_l（用中位数判基调，避免高 DR 图被亮部抬均值误判）——
    // 低调：仅极轻找回暗部（shadow_lift），median 守住低调区间（≤0.46），**不整体提亮**，
    //       保夜景/逆光的暗氛围（用户铁律：低调类保护高光阴影、胶片衰变，不硬提亮）；
    // 中调：平衡到 ~0.50；高调：允许上探（乳白），封顶 0.66 防旧版过曝。
    // exposure_ev 把中位数拉向目标（实测：中位数约 0.10 median_l / EV）
    // 护栏：异常图（全黑/全白/退化）可能让 analyze 给出 NaN/Inf 的 median_l，
    // 不夹住会算成 NaN 污染整图。先落回合法区间再算。
    let median_l = if base.tone.median_l.is_finite() {
        base.tone.median_l
    } else {
        0.5
    };
    let target_med: f32 = match t.key {
        Key::Low => (median_l + 0.03).min(0.46),
        Key::Mid => 0.50,
        Key::High => (median_l + 0.04).min(0.66),
    };
    // 护栏：自动中性化绝不把图压暗（exposure_ev ≥ 0）。
    // 已足够亮的图（如白墙人像 median_l > target_med）应保持原亮度，
    // 只做颜色校正（白平衡/去黄/对比度）；用户想暗可手动拉曝光。
    let exposure_ev = ((target_med - median_l) / 0.10).clamp(0.0, 2.0);

    // —— 对比/光比/暗部：按调性 + 基调 ——
    // 低调：绝不整体提亮；只靠 shadow_lift/deep_shadow_lift 找回暗部细节 +
    //       film_curve/light_ratio 给胶片感自然 rolloff（护高光护阴影，不硬切）。
    // 中调：明确给反差与胶片感（之前 contrast=0 导致"几乎没修"）——这是最常见的照片
    // 高调：允许提升/乳白过曝。
    let (mut contrast, mut film_curve, mut light_ratio, mut shadow_lift, mut deep_shadow_lift) =
        match (t.key, t.span) {
            // 低调：保护暗部，但给足中长调的胶片反差与暗部细节（不整体提亮）
            (Key::Low, Span::Short) => (0.08, 0.10, 0.08, 0.12, 0.08),
            (Key::Low, Span::Mid) => (0.14, 0.16, 0.15, 0.10, 0.06),
            (Key::Low, Span::Long) => (0.20, 0.18, 0.20, 0.08, 0.05),
            // 中调：明显可感知的反差与胶片感
            (Key::Mid, Span::Short) => (0.18, 0.10, 0.08, 0.05, 0.03),
            (Key::Mid, Span::Mid) => (0.24, 0.14, 0.16, 0.0, 0.0),
            (Key::Mid, Span::Long) => (0.28, 0.14, 0.18, 0.0, 0.0),
            // 高调：允许乳白/提亮，给足反差
            (Key::High, Span::Short) => (0.10, 0.08, 0.12, 0.0, 0.0),
            (Key::High, Span::Mid) => (0.18, 0.12, 0.16, 0.0, 0.0),
            (Key::High, Span::Long) => (0.24, 0.14, 0.18, 0.0, 0.0),
        };

    // 按强度缩放「增强类」字段：弱档轻、强档狠（保护类曝光/白平衡/色调映射不缩放）。
    contrast *= strength;
    film_curve *= strength;
    light_ratio *= strength;
    shadow_lift *= strength;
    deep_shadow_lift *= strength;

    // 剪影：主体是纯黑轮廓，绝不加反差去加深它（否则大量暗部跌入死黑被护栏拦下、
    // 自动修图直接失效退回原图）。只保留极轻反差 + 暗部找回（仍受强度缩放，但基数极小）。
    if t.silhouette {
        contrast = 0.04 * strength;
    }

    Adjustments {
        exposure_ev,
        tone_map,
        defake: DefakeColor::on(),
        grade: Grade {
            contrast,
            film_curve,
            light_ratio,
            shadow_lift,
            deep_shadow_lift,
            dehaze,
            ..Default::default()
        },
        white_balance: wb,
        color,
        hsl,
        skin: SkinTone::default(),
        zones: ZoneGrade::default(),
        geometry: Geometry::default(),
        detail: Detail::default(),
        advanced: Advanced::default(),
        // 色彩引擎产物（场景规则 + 记忆色 + 数码偏色补偿）随影调引擎一起下发，
        // 否则 pipeline 不会逐像素套用色彩引擎——这正是「一键智能」有、「自动中性化」
        // 没有的色彩智能差异。ColorPlan 由 auto_neutral_balance 内部已算好（强度 0.8）。
        color_plan: bal.color_plan,
        // 商业审查项3（v0.6.9 修正）：mix 必须恒为 1.0（全效果）。
        // 历史上这里写 0.9，把最终结果与原图按 10% 混合以「防过曝」，但会让用户在
        // 自动中性化之后拖动的曝光/对比/亮度等手动调整被 10% 稀释，表现为「调了没反应」。
        // 过曝防护已由 run_auto 的 is_artifact 护栏闭环负责（越界轮次自动回退），无需此折扣。
        mix: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{
        CastMetrics, ColorMetrics, ExposureMetrics, GamutMetrics, ImageMetrics, SkinMetrics,
        ToneMetrics,
    };
    use image::{DynamicImage, RgbImage};

    /// 构造合成 metrics 直接验证 `classify_tonality` 的面积比例判据，
    /// 不触发真实的 analyze/auto_neutral_balance（后者依赖完整图像统计）。
    fn mk_metrics(
        bright_pct: f32,
        dark_pct: f32,
        shadow_dead: f32,
        hi_dead: f32,
        median: f32,
    ) -> ImageMetrics {
        ImageMetrics {
            width: 100,
            height: 100,
            tone: ToneMetrics {
                mean_l: median,
                std_l: 0.2,
                min_l: 0.0,
                max_l: 1.0,
                median_l: median,
                p25_l: 0.2,
                p75_l: 0.7,
                bright_area_pct: bright_pct,
                dark_area_pct: dark_pct,
            },
            color: ColorMetrics {
                mean_c: 0.1,
                mean_h_deg: 0.0,
                hue_peakiness: 0.0,
                per_hue_chroma: [0.0; 8],
            },
            skin: SkinMetrics {
                ratio: 0.0,
                mean_c: 0.0,
                mean_h_deg: 0.0,
                mean_l: 0.5,
            },
            exposure: ExposureMetrics {
                highlight_clip_pct: hi_dead,
                shadow_clip_pct: shadow_dead,
            },
            gamut: GamutMetrics {
                clip_pct: 0.0,
                max_c: 0.0,
            },
            cast: CastMetrics {
                hue_deg: 0.0,
                chroma: 0.0,
            },
            dynamic_range: 0.8,
        }
    }

    #[test]
    fn high_key_by_bright_area_ratio() {
        // 白墙人像：80% 亮、死黑 <4% → 应判高调（面积比例，非中位数）
        let m = mk_metrics(80.0, 2.0, 1.0, 1.0, 0.60);
        assert_eq!(classify_tonality(&m).key, Key::High);
    }

    #[test]
    fn low_key_by_dark_area_ratio() {
        // 夜景/逆光：暗部面积 45%、死白 <4% → 应判低调
        let m = mk_metrics(10.0, 45.0, 1.0, 1.0, 0.30);
        assert_eq!(classify_tonality(&m).key, Key::Low);
    }

    #[test]
    fn mid_key_when_balanced() {
        // 明暗比例适中 → 中间调
        let m = mk_metrics(40.0, 15.0, 1.0, 1.0, 0.50);
        assert_eq!(classify_tonality(&m).key, Key::Mid);
    }

    #[test]
    fn high_key_not_misclassified_mid_by_median() {
        // 回归：大面积亮但 median 中等（亮背景 + 中等亮度主体）也必须判高调，
        // 不能因 median 不高而误归中调。
        let m = mk_metrics(70.0, 5.0, 2.0, 1.0, 0.52);
        assert_eq!(classify_tonality(&m).key, Key::High);
    }

    #[test]
    fn high_key_image_exposure_never_negative() {
        // 回归：白墙人像（高调）点「自动中性化」不应被压暗（exposure_ev >= 0）。
        let mut img = RgbImage::new(100, 100);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            if x < 82 {
                *p = image::Rgb([242u8, 242, 242]); // 亮（白墙）
            } else {
                *p = image::Rgb([150u8, 125, 110]); // 中等肤色
            }
        }
        let adj = tonal_adjustments(&DynamicImage::ImageRgb8(img), true, 1.0);
        assert!(
            adj.exposure_ev >= 0.0,
            "高调图 exposure_ev 应为非负，实际 {}",
            adj.exposure_ev
        );
    }
}
