//! 参考图影调匹配（纯算法，零 AI）。
//!
//! 导入一张「喜欢的图」作为参考，把它的 OKLCH 指标当作目标签名，
//! 用与一键中性同一套调整模块（曝光 / 对比 / 饱和 / 白平衡）做坐标下降：
//! 每轮渲染代理图 → 测量 → 朝参考指标收敛。guardrail 负责「靠但不毁」。
//!
//! 这比让模型凭空报参稳健得多——目标可测 → 收敛确定 → 肉眼可见靠近参考，
//! 且零 token、零网络、零 ML 依赖。

use crate::advanced::Advanced;
use crate::analyze::{analyze, ImageMetrics};
use crate::guardrail;
use crate::guardrail::is_artifact;
use crate::params::Field;
use crate::pipeline::{render, Adjustments};
use crate::tonemap::classify_tonality;
use image::GenericImageView;
use image::{DynamicImage, RgbImage};
use serde::Serialize;
use serde_json::Map;

/// 参考图匹配结果（复用 `AutoResult` 结构，便于 UI 统一展示；
/// `ref_metrics` 非空即代表本次来自参考匹配）。
#[derive(Serialize, Debug, Clone)]
pub struct ReferenceResult {
    /// 原图指标。
    pub metrics_before: ImageMetrics,
    /// 匹配后指标。
    pub metrics_after: ImageMetrics,
    /// 参考图指标（目标签名）。
    pub ref_metrics: ImageMetrics,
    /// 护栏是否通过（最后一次候选）。
    pub guardrail_passed: bool,
    /// 每轮诊断日志。
    pub log: Vec<String>,
    /// 采用参数补丁 `field_id -> value`。
    pub applied_params: serde_json::Value,
    /// 实际迭代轮数。
    pub rounds: usize,
    /// 完整参数集（含 tone_map / defake / mix 等非 registry 字段）。
    /// UI 必须用它整体替换 `self.adj`，理由同 `AutoResult::adjustments`。
    #[serde(skip)]
    pub adjustments: Adjustments,
}

/// 参考距离打分：越接近参考图指标分越高（取负距离）。
/// 过曝 / 死黑作为硬伤强烈惩罚，确保「靠但不毁」。
fn ref_score(m: &ImageMetrics, r: &ImageMetrics) -> f32 {
    let mut d = 0.0f32;
    // 亮度均值：最直观的「影调明暗」。
    d += (m.tone.mean_l - r.tone.mean_l).abs();
    // 反差：层次感。
    d += (m.tone.std_l - r.tone.std_l).abs() * 0.5;
    // 色彩强度：浓淡。
    d += (m.color.mean_c - r.color.mean_c).abs() * 1.0;
    // 整体色调：参考图与主图平均色相的差异（取最短路径，避免 350° vs 10° 这种跳变）。
    let dh = hue_shortest_diff(r.color.mean_h_deg, m.color.mean_h_deg).abs();
    d += dh * 0.04; // 60° 色调差 ≈ 2.4 距离单位，足够影响选优
    // 色偏向量差值（a/b 平面，与 auto_wb 同思路）：让源色偏朝参考色偏靠拢。
    let sv = cast_vec(m);
    let rv = cast_vec(r);
    let da = sv.0 - rv.0;
    let db = sv.1 - rv.1;
    d += (da * da + db * db).sqrt() * 2.0;
    // 过曝 / 死黑是硬伤：只有比参考「更糟」才惩罚。
    d += (m.exposure.highlight_clip_pct - r.exposure.highlight_clip_pct).max(0.0) * 0.05;
    -d
}

/// 把色偏（chroma + hue）还原成 OKLab 风格的 (a, b) 二维向量。
#[inline]
fn cast_vec(m: &ImageMetrics) -> (f32, f32) {
    let hr = m.cast.hue_deg.to_radians();
    (m.cast.chroma * hr.cos(), m.cast.chroma * hr.sin())
}

/// 计算两色相角的最短有向差值（范围 -180..180°）。
#[inline]
fn hue_shortest_diff(target: f32, current: f32) -> f32 {
    let mut d = target - current;
    d = (d + 180.0).rem_euclid(360.0) - 180.0;
    d
}

/// 返回 8 色相带中 chroma 权重最大的索引（0=红,1=橙,2=黄,3=绿,4=青,5=蓝,6=紫,7=品红）。
fn dominant_hue_band(m: &ImageMetrics) -> usize {
    let mut best = 0usize;
    let mut best_c = -1.0f32;
    for (i, &c) in m.color.per_hue_chroma.iter().enumerate() {
        if c > best_c {
            best_c = c;
            best = i;
        }
    }
    best
}

/// 朝参考图指标推进一步：比例控制器（保守步长）。
/// 从已经中性化的源图出发，做影调/色彩/色调趋近，防止过曝、过锐、油光。
fn ref_step(cur: &Adjustments, m: &ImageMetrics, r: &ImageMetrics) -> Adjustments {
    let mut a = cur.clone();

    // 亮度均值 -> 曝光（小步长，上限 0.2EV/轮）。
    let dl = r.tone.mean_l - m.tone.mean_l;
    let ev = Field::ExposureEv.get(&a) + (dl * 1.0).clamp(-0.20, 0.20);
    Field::ExposureEv.set(&mut a, ev);

    // 反差 -> 对比（更保守，避免局部过锐/油光）。
    let ds = r.tone.std_l - m.tone.std_l;
    let c = Field::Contrast.get(&a) + (ds * 0.8).clamp(-0.15, 0.15);
    Field::Contrast.set(&mut a, c);

    // 色彩强度 -> 饱和度
    let dc = r.color.mean_c - m.color.mean_c;
    let sat = (Field::Saturation.get(&a) + (dc * 0.6).clamp(-0.15, 0.15)).clamp(0.0, 3.0);
    Field::Saturation.set(&mut a, sat);

    // 色温/色调 -> 白平衡（只小幅度修温度，大跨度色相交给 hue_rotate/HSL）
    let sv = cast_vec(m);
    let rv = cast_vec(r);
    let da = sv.0 - rv.0;
    let db = sv.1 - rv.1;
    let wb = Field::WBTemp.get(&a) + (-da * 0.3).clamp(-0.10, 0.10);
    let tint = Field::WBTint.get(&a) + (-db * 0.3).clamp(-0.10, 0.10);
    Field::WBTemp.set(&mut a, wb);
    Field::WBTint.set(&mut a, tint);

    // 整体色调迁移：让主图平均色相朝参考图平均色相旋转。
    let dh = hue_shortest_diff(r.color.mean_h_deg, m.color.mean_h_deg);
    let hr = Field::HueRotate.get(&a) + (dh * 0.25).clamp(-25.0, 25.0);
    Field::HueRotate.set(&mut a, hr);

    // HSL 分区精细迁移：把主图的主色相带往参考图的主色相带拉。
    // 例如紫花（主带 6）配暖橙参考（主带 1），把紫色往橙方向偏移。
    let src_band = dominant_hue_band(m);
    let ref_band = dominant_hue_band(r);
    if src_band != ref_band {
        let bin_center = |b: usize| -> f32 { (b as f32 + 0.5) * 45.0 };
        let band_dh = hue_shortest_diff(bin_center(ref_band), bin_center(src_band));
        a.hsl.hue_shift[src_band] += (band_dh * 0.35).clamp(-35.0, 35.0);
        // 同时适度压一下主图原主带的饱和度，避免 hue_shift 后局部色过于浓艳
        a.hsl.sat_mult[src_band] = (a.hsl.sat_mult[src_band] - 0.02).max(0.85);
    }

    // 参考图若有明显暖/冷调，给暗部/高光加 split-tone 以贴近其氛围。
    if r.cast.chroma > 0.06 {
        let ref_h = r.cast.hue_deg;
        a.color.split_shadow = (a.color.split_shadow + hue_shortest_diff(ref_h, a.color.split_shadow) * 0.10).clamp(-45.0, 45.0);
        a.color.split_highlight = (a.color.split_highlight + hue_shortest_diff(ref_h, a.color.split_highlight) * 0.08).clamp(-45.0, 45.0);
    }

    a
}

/// 主入口：把 `img` 的影调朝 `ref_m` 靠拢，返回（全分辨率结果图, 报告）。
///
/// 闭环：初始为照片默认参数 → 每轮渲染代理图 → 测量 → 用 `ref_step` 推进一步
/// → guardrail 拦截损坏候选 → `ref_score` 选最优。最终取全分辨率重渲。
pub fn run_reference_match(
    img: &DynamicImage,
    ref_m: &ImageMetrics,
    proxy_max: u32,
    rounds: usize,
    strength: f32,
) -> (RgbImage, ReferenceResult) {
    let base = analyze(img);
    let tone = classify_tonality(&base);
    // 起点用影调感知引擎（正确放置亮度，不无脑提亮）；再朝参考小步趋近颜色/影调/色调。
    let mut adj = crate::tonemap::tonal_adjustments(img, true, 1.0);
    guardrail::clamp(&mut adj);

    // 最优选初始为影调引擎基线；只有「无伪影且更靠近参考」的候选才被采纳。
    let mut best_adj = adj.clone();
    let mut best_score = f32::NEG_INFINITY;
    let mut log: Vec<String> = Vec::new();
    let mut last_passed = true;

    let (iw, ih) = img.dimensions();
    let proxy = if iw.max(ih) > proxy_max {
        img.resize(
            proxy_max,
            proxy_max * ih / iw,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        img.clone()
    };

    for r in 0..rounds {
        let out = render(&proxy, &adj);
        let m = analyze(&DynamicImage::ImageRgb8(out));
        let art = is_artifact(&m, &base, &tone);
        let sc = ref_score(&m, ref_m);
        last_passed = art.is_none();
        let verdict = match &art {
            None => "通过".to_string(),
            Some(reason) => format!("伪影: {}", reason),
        };
        log.push(format!(
            "[轮{}] 距离分={:.2} 护栏={} | mean_l={:.3} std_l={:.3} mean_c={:.3} hue={:.1}°",
            r, sc, verdict, m.tone.mean_l, m.tone.std_l, m.color.mean_c, m.color.mean_h_deg
        ));
        if art.is_none() && sc > best_score {
            best_score = sc;
            best_adj = adj.clone();
        }
        let mut next = ref_step(&adj, &m, ref_m);
        guardrail::clamp(&mut next);
        adj = next;
    }

    let mut final_adj = best_adj.clone();
    // 强度滑块真正生效：mix=0 原图，mix=1 完全贴合参考。
    final_adj.mix = strength.clamp(0.0, 1.0);
    // 双保险：自动路径绝不开启磨皮/融合（advanced 是手动可选功能，开了会糊图）。
    final_adj.advanced = Advanced::default();
    let final_img = render(img, &final_adj);
    let mfin = analyze(&DynamicImage::ImageRgb8(final_img.clone()));

    let mut patch = Map::new();
    for spec in crate::params::registry() {
        let v = spec.field.get(&final_adj);
        let d = spec.to_raw(0.0);
        if (v - d).abs() > 1e-4 {
            patch.insert(spec.field.id(), serde_json::Value::from(v));
        }
    }

    (
        final_img,
        ReferenceResult {
            metrics_before: base,
            metrics_after: mfin,
            ref_metrics: ref_m.clone(),
            guardrail_passed: last_passed,
            log,
            applied_params: serde_json::Value::Object(patch),
            rounds,
            adjustments: final_adj.clone(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto::run_auto;
    use image::{DynamicImage, Rgb, RgbImage};

    fn solid(w: u8, h: u8, v: u8) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w as u32, h as u32, Rgb([v, v, v])))
    }

    #[test]
    fn neutral_lifts_dark_without_over_exposing() {
        // 暗灰图：一键中性应把它提亮到健康区间，但绝不惨白过曝。
        let dark = solid(64, 64, 30);
        let base_m = analyze(&dark);
        assert!(base_m.tone.mean_l < 0.3, "前提：原图确实偏暗");
        let (img, res) = run_auto(&dark, 128, 3, 1.0);
        let m = analyze(&DynamicImage::ImageRgb8(img));
        assert!(m.tone.mean_l > base_m.tone.mean_l, "应被提亮");
        assert!(m.tone.mean_l <= 0.62, "绝不过曝/惨白（护栏生效）");
        // 护栏应判定为安全（暗图提亮到中性是改进而非损坏）。
        assert!(res.guardrail_passed, "中性校正应通过护栏");
    }

    #[test]
    fn reference_match_moves_toward_ref_without_over_exposing() {
        // 源=暗灰，参考=亮灰：匹配应把源往亮的方向靠，但受护栏钳制不过曝。
        let src = solid(64, 64, 40);
        let ref_img = solid(64, 64, 210);
        let ref_m = analyze(&ref_img);
        assert!(ref_m.tone.mean_l > 0.7, "前提：参考图确实偏亮");
        let (img, res) = run_reference_match(&src, &ref_m, 128, 4, 1.0);
        let m = analyze(&DynamicImage::ImageRgb8(img));
        let src_m = analyze(&src);
        assert!(
            m.tone.mean_l > src_m.tone.mean_l,
            "匹配应让源图朝参考变亮 mean_l={} > {}",
            m.tone.mean_l,
            src_m.tone.mean_l
        );
        assert!(m.tone.mean_l <= 0.62, "匹配也不许过曝/惨白");
        assert!(res.rounds >= 1);
    }
}
