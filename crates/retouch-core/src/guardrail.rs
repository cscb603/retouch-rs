//! Safety guardrails for AI-driven correction.
//!
//! Two layers, both ported in spirit from retouch_app's `guardrail.py`:
//!
//! 1. **Hard clamp** — every field is bounded to its slider range. An LLM can
//!    only ever nudge what a human could nudge; it can never emit a physically
//!    absurd value (e.g. exposure +5 EV) that would obliterate the image.
//!
//! 2. **Metric guardrail** — after rendering a candidate, compare its metrics
//!    against the *original* baseline. If the correction irreversibly damages
//!    skin saturation, adds color cast, or introduces new clipping, reject it.
//!    This is what lets an autonomous loop "know" it went too far even though
//!    it cannot *see* the picture.

use crate::analyze::ImageMetrics;
use crate::params::registry;
use crate::pipeline::Adjustments;
use crate::tonemap::{Key, Tonality};

/// Clamp every adjustable field of `adj` into its slider-allowed range.
/// Cheap (just `registry()` iteration + get/set). Safe to call on every
/// candidate before rendering.
pub fn clamp(adj: &mut Adjustments) {
    for spec in registry() {
        let v = spec.field.get(adj);
        let lo = spec.to_raw(-1.0);
        let hi = spec.to_raw(1.0);
        let nv = v.clamp(lo, hi);
        spec.field.set(adj, nv);
    }
}

/// Result of the metric guardrail.
#[derive(Debug, Clone)]
pub struct GuardrailStatus {
    /// True if the candidate is safe to keep.
    pub passed: bool,
    /// Human-readable reasons (empty when passed).
    pub reasons: Vec<String>,
}

/// Compare a candidate's metrics against the original baseline. Returns a
/// rejection if the correction caused irreversible damage.
///
/// Thresholds mirror retouch_app's `guardrail_status`: skin chroma drift
/// ±7, over-saturation growth, cast growth +2, new highlight/shadow clipping,
/// plus brightness / skin-brightness ceilings to stop over-exposure / grayed-out skin.
pub fn check(m: &ImageMetrics, base: &ImageMetrics) -> GuardrailStatus {
    let mut reasons: Vec<String> = Vec::new();

    // 整体亮度上限：中/低调 mean_l > 0.62 即明显发白/过曝（惨白毁图）；
    // 高调图（基线中位数已 >0.58，本就明亮/乳白）放宽到 0.70，否则把雪景/日系
    // 等健康高调照误拦成"毁图"→ 自动修图失效。
    let bright_cap = if base.tone.median_l > 0.58 { 0.70 } else { 0.62 };
    if m.tone.mean_l > bright_cap {
        reasons.push(format!(
            "整体亮度过高/发白 mean_l {:.3} > {:.2}",
            m.tone.mean_l, bright_cap
        ));
    }

    // Skin: only judge if the original actually had meaningful skin area.
    if base.skin.ratio > 0.03 && m.skin.ratio > 0.03 {
        let mc = m.skin.mean_c;
        let bmc = base.skin.mean_c;
        if mc > bmc + 7.0 {
            reasons.push(format!("肤色过饱和 mean_c {:.2} > 基线 {:.2}+7", mc, bmc));
        }
        if mc < bmc - 6.0 {
            reasons.push(format!("肤色洗白 mean_c {:.2} < 基线 {:.2}-6", mc, bmc));
        }
        // 肤色亮度绝对上限：> 0.82 即洗白（惨白脸）。同样用绝对阈值，
        // 避免合法地把暗脸提亮到健康值被误判为「毁图」。
        if m.skin.mean_l > 0.82 {
            reasons.push(format!("肤色过亮/洗白 mean_l {:.3} > 0.82", m.skin.mean_l));
        }
    }

    // Color cast growth (dominant hue chroma). We judge cast strength as the
    // overall chroma of the dominant direction — if it ballooned, the image
    // developed a synthetic tint.
    let cast = m.cast.chroma;
    let bcast = base.cast.chroma;
    if cast > bcast + 0.05 {
        reasons.push(format!("偏色增长 chroma {:.3} > 基线 {:.3}+0.05", cast, bcast));
    }

    // Global saturation should not jump wildly.
    if m.color.mean_c > base.color.mean_c + 0.03 {
        reasons.push(format!(
            "全局饱和度增长 mean_c {:.3} > 基线 {:.3}+0.03",
            m.color.mean_c, base.color.mean_c
        ));
    }

    // New clipping (lost highlights / blocked shadows) beyond baseline.
    // Use a tighter threshold for highlights: 0.3% is already visible.
    // 自适应阈值：低调图本就有大片暗部/死黑，允许更多暗部位移（否则任何反差/暗部
    // 找回都被误判为"毁图"→ 自动修图对夜景/剪影直接失效退回原图）；高调图同理允许
    // 更多高光位移。中调图仍严格保护。
    let low_key = base.tone.median_l < 0.42;
    let high_key = base.tone.median_l > 0.58;

    let cw = m.exposure.highlight_clip_pct;
    let bcw = base.exposure.highlight_clip_pct;
    let hi_thresh = if high_key {
        (bcw + 3.0).max(1.0)
    } else {
        (bcw + 0.3).max(0.3)
    };
    if cw > hi_thresh {
        reasons.push(format!("新增过曝 {:.1}%", cw));
    }
    let cb = m.exposure.shadow_clip_pct;
    let bcb = base.exposure.shadow_clip_pct;
    let sh_thresh = if low_key {
        (bcb + 10.0).max(5.0)
    } else {
        (bcb + 1.0).max(1.0)
    };
    if cb > sh_thresh {
        reasons.push(format!("新增死黑 {:.1}%", cb));
    }

    GuardrailStatus {
        passed: reasons.is_empty(),
        reasons,
    }
}

/// Score a candidate for *sweet-spot* selection (higher = better). Rewards
/// lifting a flat / dark image toward a healthy mid-tone with real contrast,
/// penalizes clipping and cast. Used to pick the best of several candidates.
pub fn score(m: &ImageMetrics, base: &ImageMetrics) -> f32 {
    let mut s = 0.0f32;
    // contrast: reward higher std_l (up to a point)
    s += (m.tone.std_l - base.tone.std_l).min(0.2) * 2.0;
    // brightness: reward moving mean_l toward ~0.5
    let target = 0.5f32;
    let l_err = (m.tone.mean_l - target).abs();
    let b_err = (base.tone.mean_l - target).abs();
    s += (b_err - l_err) * 0.5;
    // color: a little chroma is good; too much is not
    if m.color.mean_c < base.color.mean_c {
        s += (base.color.mean_c - m.color.mean_c) * 1.0;
    } else {
        s -= (m.color.mean_c - base.color.mean_c.min(0.18)).max(0.0) * 0.5;
    }
    // penalties
    s -= (m.exposure.highlight_clip_pct + m.exposure.shadow_clip_pct) * 1.5;
    s -= (m.cast.chroma - base.cast.chroma).max(0.0) * 2.0;
    s
}

/// 好照片的「目标量化带」（按影调分类，源自 RESEARCH §3/§6.3/§10.1 + 实测健康区间）。
///
/// 这是**目标函数**，不是「别偏离原图」：一键修图要「修到这个带里=好看」，
/// 高调图允许亮、低调图允许暗、剪影主体允许纯黑——都不再拿原图当尺子拦。
pub struct ToneTarget {
    pub med_lo: f32,
    pub med_hi: f32,
    pub std_lo: f32,
    pub std_hi: f32,
    pub c_lo: f32,
    pub c_hi: f32,
}

/// 按影调给出目标带（剪影特例：主体纯黑，整体 median 可低，但不强制提亮）。
pub fn target_for(t: &Tonality) -> ToneTarget {
    if t.silhouette {
        return ToneTarget {
            med_lo: 0.22,
            med_hi: 0.50,
            std_lo: 0.14,
            std_hi: 0.24,
            c_lo: 0.08,
            c_hi: 0.18,
        };
    }
    match t.key {
        Key::Low => ToneTarget {
            med_lo: 0.38,
            med_hi: 0.46,
            std_lo: 0.15,
            std_hi: 0.24,
            c_lo: 0.09,
            c_hi: 0.18,
        },
        Key::Mid => ToneTarget {
            med_lo: 0.46,
            med_hi: 0.54,
            std_lo: 0.17,
            std_hi: 0.26,
            c_lo: 0.09,
            c_hi: 0.18,
        },
        Key::High => ToneTarget {
            med_lo: 0.55,
            med_hi: 0.66,
            std_lo: 0.16,
            std_hi: 0.26,
            c_lo: 0.09,
            c_hi: 0.18,
        },
    }
}

/// 带评分：x 落在 [lo,hi] 内=1.0；区间外线性衰减到 0（容差 0.06）。
#[inline]
fn band_score(x: f32, lo: f32, hi: f32) -> f32 {
    if x >= lo && x <= hi {
        1.0
    } else if x < lo {
        (1.0 - (lo - x) / 0.06).max(0.0)
    } else {
        (1.0 - (x - hi) / 0.06).max(0.0)
    }
}

/// 好照片评分（越高=越接近目标带=越好看）。这是自动修图的**目标函数**，
/// 替代旧的「别偏离原图」式护栏——驱动闭环去够「好看」，而不是只求安全。
pub fn goodness(m: &ImageMetrics, t: &ToneTarget) -> f32 {
    let mut s = 0.0;
    s += band_score(m.tone.median_l, t.med_lo, t.med_hi) * 3.0;
    s += band_score(m.tone.std_l, t.std_lo, t.std_hi) * 2.0;
    s += band_score(m.color.mean_c, t.c_lo, t.c_hi) * 1.5;
    // 偏色越小越好（chroma 0.03 以下近乎中性）
    s -= (m.cast.chroma - 0.03).max(0.0) * 4.0;
    s
}

/// 真·毁图检测——**只拦出现伪影的候选**，绝不拦「偏离原图」。
/// 返回 `Some(原因)` 表示应拒绝。
pub fn is_artifact(m: &ImageMetrics, base: &ImageMetrics, t: &Tonality) -> Option<String> {
    // 合成偏色：出现原图没有的明显色罩 = 伪影（毁固有色/出怪色）
    if m.cast.chroma > base.cast.chroma + 0.08 {
        return Some(format!("新增偏色 chroma {:.3}", m.cast.chroma));
    }
    // 冲脸 / 肤色洗白 / 肤色过饱和
    if base.skin.ratio > 0.03 && m.skin.ratio > 0.03 {
        if m.skin.mean_l > 0.82 {
            return Some("肤色过亮/洗白".into());
        }
        if m.skin.mean_c > base.skin.mean_c + 7.0 {
            return Some("肤色过饱和".into());
        }
    }
    // 新增高光死白：仅中/低调图算伪影（高调/剪影允许均匀过曝，是健康意图）
    let hi_intent = t.key == Key::High || t.silhouette;
    if !hi_intent {
        if m.exposure.highlight_clip_pct > base.exposure.highlight_clip_pct + 1.0 {
            return Some(format!("新增过曝 {:.1}%", m.exposure.highlight_clip_pct));
        }
    }
    None
}
