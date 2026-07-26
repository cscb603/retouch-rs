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
/// - 基调：`mean_l < 0.38 → 低`；`0.38–0.62 → 中`；`> 0.62 → 高`。
/// - 调性：`dynamic_range < 0.33 → 短`；`0.33–0.66 → 中`；`> 0.66 → 长`。
/// - 软硬：高光与阴影**两侧**都有显著死白 → 硬；否则软。
/// - 剪影特例：阴影死白占比高且最暗接近纯黑 → 主体纯黑轮廓。
pub fn classify_tonality(m: &ImageMetrics) -> Tonality {
    let dr = m.dynamic_range; // max_l − min_l，真实动态范围（与中点放置无关）

    // 基调用**中位数**判：高 DR 图（夜景/逆光）有少量极亮像素把 mean_l 抬高，
    // 但大面积仍是暗的，中位数才能认出它本该是低调。
    // 阈值放宽：低调 < 0.45（夜景/逆光普遍 median 0.39–0.46，要守住暗氛围，不被提亮）；
    // 高调 > 0.58（雪景/日系/亮调人像）。
    let key = if m.tone.median_l < 0.45 {
        Key::Low
    } else if m.tone.median_l > 0.58 {
        Key::High
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
    let target_med: f32 = match t.key {
        Key::Low => (base.tone.median_l + 0.03).min(0.46),
        Key::Mid => 0.50,
        Key::High => (base.tone.median_l + 0.04).min(0.66),
    };
    // exposure_ev 把中位数拉向目标（实测：中位数约 0.10 median_l / EV）
    let exposure_ev = ((target_med - base.tone.median_l) / 0.10).clamp(-2.0, 2.0);

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
        color_plan: None,
        mix: 0.9,
    }
}
