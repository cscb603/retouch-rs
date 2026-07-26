//! Autonomous, zero-network ("smart default") correction.
//!
//! This is the local half of the retouch_app agent loop, reimplemented
//! natively on our pipeline (no darktable, no LibRaw, no Python). The flow is
//! identical to what we learned from `retouch_app`:
//!
//! ```text
//! baseline metrics ──▶ heuristic params ──▶ render proxy ──▶ measure
//!        │                                                        │
//!        │                                           guardrail (vs baseline)
//!        └───────────────────────────────────────────────────▶ reject if damaged
//!                                                    │ passed → keep / refine → next round
//! final: best candidate + detail/advanced at full res
//! ```
//!
//! `auto_correct` is the rule-based "brain" (mirrors retouch_app's
//! `LocalClient`); `run_auto` is the loop. An LLM-driven variant
//! (`--mode api`) can later replace `auto_correct` with a model call — the
//! *loop and guardrails stay the same*, which is exactly the reusable
//! "method vs parameters" principle from retouch_app's PLAN.md.

use crate::advanced::Advanced;
use crate::analyze::{analyze, ImageMetrics};
use crate::color_engine::{analyze_color, color_plan, scene_rules, ColorMetrics};
use crate::guardrail;
use crate::guardrail::{goodness, is_artifact, target_for, ToneTarget};
use crate::params::{registry, Field};
use crate::pipeline::{render, Adjustments};
use crate::tonemap::{classify_tonality, Key, Tonality};
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use serde::Serialize;
use serde_json::Map;

/// Rule-based initial correction, expressed as `(field, raw_value)` overrides.
/// Mirrors retouch_app's `LocalClient.analyze` but reasoned in OKLCH: dark ->
/// lift, flat -> contrast, pale -> vibrance, cast -> white balance, skin ->
/// gentle pinken / de-saturate.
pub fn auto_correct(m: &ImageMetrics) -> Vec<(Field, f32)> {
    let mut out: Vec<(Field, f32)> = Vec::new();
    let push = |v: &mut Vec<(Field, f32)>, f: Field, x: f32| v.push((f, x));

    // --- exposure: pull mean lightness toward a healthy ~0.52 (never blow past) ---
    if m.tone.mean_l < 0.46 {
        push(
            &mut out,
            Field::ExposureEv,
            ((0.52 - m.tone.mean_l) * 1.4).min(0.5),
        );
    } else if m.tone.mean_l > 0.60 {
        push(
            &mut out,
            Field::ExposureEv,
            -((m.tone.mean_l - 0.52) * 1.1).min(0.5),
        );
    }

    // --- flatness / low contrast -> tiny contrast + clarity bump (keep neutral, not stylized) ---
    if m.tone.std_l < 0.12 {
        push(&mut out, Field::Contrast, 0.08);
        push(&mut out, Field::Dehaze, 0.06);
    }

    // --- pale / washed-out color -> slight vibrance (avoid oversaturating) ---
    if m.color.mean_c < 0.06 {
        push(&mut out, Field::Vibrance, 0.15);
    }

    // --- color cast: neutralize the dominant hue direction (conservative amounts) ---
    if m.cast.chroma > 0.10 {
        let h = m.cast.hue_deg;
        if (0.0..=60.0).contains(&h) || h > 300.0 {
            // warm/orange/red cast -> cool it
            push(&mut out, Field::WBTemp, -0.08);
        } else if (180.0..=300.0).contains(&h) {
            // cool/blue cast -> warm it
            push(&mut out, Field::WBTemp, 0.10);
        } else if (90.0..=160.0).contains(&h) {
            // green cast -> magenta tint
            push(&mut out, Field::WBTint, 0.07);
        }
    }

    // --- skin: very gentle correction (strength scaled down so it doesn't look artificial) ---
    if m.skin.ratio > 0.04 {
        if m.skin.mean_c < 0.07 {
            // dull skin -> light pinken
            push(&mut out, Field::SkinStrength, 0.25);
            push(&mut out, Field::SkinPinken, 0.15);
        } else if m.skin.mean_c > 0.16 {
            // over-saturated / ruddy skin -> pull back
            push(&mut out, Field::SkinStrength, 0.25);
            push(&mut out, Field::SkinYellowReduce, 0.15);
        }
    }

    // --- crushed shadows -> slight recovery ---
    if m.exposure.shadow_clip_pct > 0.5 {
        push(&mut out, Field::DeepShadowLift, 0.12);
    }

    out
}

/// Full autonomous correction report.
#[derive(Serialize, Debug, Clone)]
pub struct AutoResult {
    /// Metrics of the original image.
    pub metrics_before: ImageMetrics,
    /// Metrics after the final (full-resolution) render.
    pub metrics_after: ImageMetrics,
    /// Guardrail status of the chosen candidate (passed / reasons).
    pub guardrail_passed: bool,
    /// Per-round log lines (diagnostic).
    pub log: Vec<String>,
    /// Chosen parameter overrides as `field_id -> value` (compact patch).
    pub applied_params: serde_json::Value,
    /// Number of iteration rounds actually run.
    pub rounds: usize,
    /// 参考图指标（仅参考匹配时有值；一键中性为 None）。
    pub ref_metrics: Option<ImageMetrics>,
    /// 完整参数集（含 tone_map / defake / mix 等非 registry 字段）。
    /// UI 必须用它整体替换 `self.adj`，绝不能只取 `applied_params` 的 registry
    /// 字段——否则 `mix`/`Agx`/`defake` 等保命字段会被丢成默认，导致满强度
    /// 校正 + 高光无保护 → 过曝油光（即 v0.2.0「一键中性」毁图根因）。
    #[serde(skip)]
    pub adjustments: Adjustments,
}

/// Shared autonomous loop — the *method* (analyze → decide → render →
/// guardrail → score → keep-best → full-res final) is fixed; the *decider*
/// (where the next candidate comes from) is pluggable. This is exactly the
/// "method vs parameters" principle from retouch_app's PLAN.md:
///
/// - `run_auto` (local) plugs in the rule-based `auto_correct` heuristic.
/// - `--mode api` plugs in a model call (DeepSeek text decision), keeping the
///   very same loop and guardrails — the model only ever *nudges* what a
///   human could, and the guardrail still rejects anything that damages the
///   original.
///
/// `decider` receives the current metrics, the current adjustments, the round
/// index, and a stage hint ("initial" / "refine"), and returns the **next**
/// candidate adjustments to evaluate.
pub fn run_auto_loop<F>(
    img: &DynamicImage,
    proxy_max: u32,
    rounds: usize,
    mut decider: F,
) -> (RgbImage, AutoResult)
where
    F: FnMut(&ImageMetrics, &Adjustments, usize, &str) -> Adjustments,
{
    let base = analyze(img);

    // proxy image for cheap iteration（提到最前，供 initial 量度使用）
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

    // first candidate from the decider (starts from product defaults)
    let mut adj = decider(&base, &Adjustments::photo_default(), 0, "initial");
    guardrail::clamp(&mut adj);

    // 以「带强度影调引擎的 initial」为最优基线：弱/中/强各档的出发点就是对应强度，
    // 闭环只做「微调改进」，绝不退回比基线更弱的 photo_default——
    // 否则强档因被 goodness 当成「过度」而整轮遭拒、退回原图近似（弱>中>强倒挂的根因）。
    let t = classify_tonality(&base);
    let target = target_for(&t);
    let init_m = analyze(&DynamicImage::ImageRgb8(render(&proxy, &adj)));
    let init_art = is_artifact(&init_m, &base, &t);
    let mut best_adj = if init_art.is_none() {
        adj.clone()
    } else {
        Adjustments::photo_default()
    };
    let mut best_score: f32 = if init_art.is_none() {
        goodness(&init_m, &target)
    } else {
        goodness(&base, &target)
    };
    let mut log: Vec<String> = Vec::new();

    for r in 0..rounds {
        let out = render(&proxy, &adj);
        let m = analyze(&DynamicImage::ImageRgb8(out));
        let art = is_artifact(&m, &base, &t);
        let sc = goodness(&m, &target);
        let verdict = match &art {
            Some(reason) => format!("伪影: {}", reason),
            None => "通过".to_string(),
        };
        log.push(format!(
            "[轮{}] goodness={:.2} {} | median_l={:.3} std_l={:.3} mean_c={:.3}",
            r, sc, verdict, m.tone.median_l, m.tone.std_l, m.color.mean_c
        ));
        if art.is_none() && sc > best_score {
            best_score = sc;
            best_adj = adj.clone();
        }
        // next candidate from the decider
        let mut next = decider(&m, &adj, r, "refine");
        guardrail::clamp(&mut next);
        adj = next;
    }

    // final full-resolution render: do NOT inject stylized detail (no diffuse glow,
    // no heavy denoise, no extra sharpening). One-click local correction is meant to
    // be a neutral, smart correction of exposure / white-balance / contrast only.
    // Detail effects (denoise, sharpen, diffuse glow) are left to the user.
    let final_adj = best_adj.clone();
    let final_img = render(img, &final_adj);
    let mfin = analyze(&DynamicImage::ImageRgb8(final_img.clone()));

    // compact applied patch = non-default fields
    let mut patch = Map::new();
    for spec in registry() {
        let v = spec.field.get(&final_adj);
        let d = spec.to_raw(0.0);
        if (v - d).abs() > 1e-4 {
            patch.insert(spec.field.id(), serde_json::Value::from(v));
        }
    }

    let result = AutoResult {
        metrics_before: base.clone(),
        metrics_after: mfin.clone(),
        guardrail_passed: guardrail::check(&mfin, &base).passed,
        log,
        applied_params: serde_json::Value::Object(patch),
        rounds,
        ref_metrics: None,
        adjustments: final_adj.clone(),
    };
    (final_img, result)
}

/// 单侧护栏（v0.2.1）：只把结果「往原图拉」（降 `mix`），绝不主动提亮、绝不动
/// 曝光 / 对比。仅在出现真·毁图信号时才介入；普通图 = 无操作（自动中性化已接受的
/// 「亮度变淡」观感零变化）。
///
/// **毁图信号（任一触发即降 mix，直到安全或 mix=0 退回原图）：**
///   - 整体过亮：`result.mean_l > cap`，其中 `cap = if base.mean_l>0.55 { base+0.02 } else { 0.58 }`
///     （普通图引擎输出本就 <0.58 → 不触发；亮原图允许比原图略亮 0.02，不兜死白）
///   - 高光死白新增：`result.highlight_clip_pct > base.highlight_clip_pct + 0.08`
///   - 冲脸：`base.skin.ratio>0.03` 且 `result.skin.mean_l > 0.82`
///   - 严重发灰：`result.std_l < base.std_l * 0.60`
///
/// **实现（O(1) 搜索）**：`pipeline` 的 `mix` 是 sRGB 逐像素线性混合
/// `out = mix·proc + (1-mix)·orig`（见 pipeline.rs:1144）。故只需渲一次 full-effect
/// 代理图 + 一次原图，按比例混合后量度，无需每档重渲整条管线。量度在缩略图上做，
/// 与分辨率无关（mean_l / std_l / clip% 均为比例量）。
pub fn safe_neutral(img: &DynamicImage, base: &ImageMetrics, mut adj: Adjustments) -> Adjustments {
    let start_mix = adj.mix.clamp(0.0, 1.0);

    // **全分辨率量度**（关键修复）：护栏指标（尤其 highlight_clip_pct）在缩略图代理上
    // 会被邻域平均掉——细小高光点（车灯/反光）在 1024px 代理里被抹平，导致代理图算出来
    // 「没过曝、安全」而放行，但全分辨率真出图时这些点全爆（成都街头 hi 0.02→1.81、
    // 老报馆 hi 0.70→3.37 即此坑）。故直接在原始分辨率渲一次 full-effect 图，按 mix 逐像素
    // 混合原图后量度，保证护栏判定指标 ≡ 最终出图指标。
    // 成本：unsafe 图多渲一次全图 + 16 次混合量度；普通图在 start_mix 即安全、仅 1 次量度即返回。
    // 安全底：mix=0 精确等于原图（blend(proc,orig,0)=orig），故扫描最差退回原图，绝不可能比原图更糟。
    let orig_full = img.to_rgb8();
    // full-effect 处理图（mix=1.0）——所有候选 mix 都由它与原图按比例混合得到
    let proc_adj = Adjustments {
        mix: 1.0,
        ..adj.clone()
    };
    let proc_full = render(img, &proc_adj);

    let cap = if base.tone.mean_l > 0.55 {
        base.tone.mean_l + 0.02
    } else {
        0.58
    };

    // 在 mix=m 处的量度（全分辨率）：混合 proc/orig 后分析
    let metrics_at = |m: f32| -> ImageMetrics {
        let blended = blend_rgb(&proc_full, &orig_full, m);
        analyze(&DynamicImage::ImageRgb8(blended))
    };

    // 真·毁图判定（单侧：只拦变糟，不拦「变淡」）
    let is_safe = |mt: &ImageMetrics| -> bool {
        if mt.tone.mean_l > cap {
            return false;
        }
        if mt.exposure.highlight_clip_pct > base.exposure.highlight_clip_pct + 0.08 {
            return false;
        }
        if base.skin.ratio > 0.03 && mt.skin.mean_l > 0.82 {
            return false;
        }
        if mt.tone.std_l < base.tone.std_l * 0.60 {
            return false;
        }
        true
    };

    // 普通图：起始 mix 已安全 → 无操作（保留自动中性化观感）
    let start_metrics = metrics_at(start_mix);
    if is_safe(&start_metrics) {
        return adj;
    }

    // 从 start_mix 向下扫描，取「最高（最接近满校正）且安全」的 mix；
    // 找不到则退回原图（mix=0）。单调可逆：mix 越小越靠近原图。
    for i in 1..=16 {
        let m = start_mix * (1.0 - i as f32 / 16.0);
        if is_safe(&metrics_at(m)) {
            adj.mix = m;
            return adj;
        }
    }
    adj.mix = 0.0;
    adj
}

/// sRGB 逐像素线性混合：`out = m·proc + (1-m)·orig`，与 pipeline 的 `mix` 语义一致。
#[inline]
fn blend_rgb(proc: &RgbImage, orig: &RgbImage, m: f32) -> RgbImage {
    let (w, h) = proc.dimensions();
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = proc.get_pixel(x, y).0;
            let o = orig.get_pixel(x, y).0;
            let r = (m * p[0] as f32 + (1.0 - m) * o[0] as f32).round() as u8;
            let g = (m * p[1] as f32 + (1.0 - m) * o[1] as f32).round() as u8;
            let b = (m * p[2] as f32 + (1.0 - m) * o[2] as f32).round() as u8;
            out.put_pixel(x, y, Rgb([r, g, b]));
        }
    }
    out
}

/// Local, zero-network autonomous correction (v0.3.2+).
///
/// 目标 = 「修到调研里的好照片量化带」(RESEARCH §3/§6.3/§10.1)，不是"别偏离原图"。
/// 流程：影调引擎出基线 → 闭环微调（每轮渲代理图→量指标→朝目标带补一档→
/// 只留更靠近目标带且无伪影的候选）→ 分区融合(多渲染+亮度遮罩软合成，给局部对比/质感)
/// → 全分辨率出图并复测。Rust 单图处理足够快，多渲染融合可接受。
pub fn run_auto(
    img: &DynamicImage,
    proxy_max: u32,
    rounds: usize,
    strength: f32,
) -> (RgbImage, AutoResult) {
    let base = analyze(img);
    let t = classify_tonality(&base);
    let target = target_for(&t);
    // 色彩引擎预分析
    let cm = analyze_color(img, &base);
    let rules = scene_rules(cm.scene);
    let cp = color_plan(&cm, &rules, strength);
    let pm = if proxy_max == 0 { 1024 } else { proxy_max };
    let rds = if rounds == 0 { 4 } else { rounds };

    // 闭环：initial=影调基线（带强度）；refine=朝目标带补缺口（修完复测→微调，步长按强度）
    let (loop_img, mut result) = run_auto_loop(img, pm, rds, |m, prev, _r, stage| {
        if stage == "initial" {
            crate::tonemap::tonal_adjustments(img, true, strength)
        } else {
            refine_correction(m, prev, &target, &t, strength)
        }
    });

    // 分区影调融合：暗部提亮 + 高光压缩 + 全局基线，按原图亮度遮罩软合成
    // （darktable Tone Equalizer 思路；用户要的"生成几份再融合"=给局部对比与质感，
    //  且低调/剪影主体不被整体提亮）。增量随强度放大。
    // 在最终渲染前注入色彩引擎：调性对齐+记忆色+数码补偿
    result.adjustments.color_plan = Some(cp.clone());
    result.adjustments.color.vibrance += cp.vibrance_target;
    result.adjustments.color.saturation *= cp.saturation_target;
    let final_img = zone_blend(img, &result.adjustments, &base, strength);
    let mfin = analyze(&DynamicImage::ImageRgb8(final_img.clone()));

    // 双保险：自动路径绝不开启磨皮/融合（手动可选功能，开了会糊图）
    result.adjustments.advanced = Advanced::default();
    result.metrics_after = mfin.clone();
    result.guardrail_passed = is_artifact(&mfin, &base, &t).is_none();
    result.log.insert(
        0,
        format!(
            "一键中性·影调引擎({}) + 色彩引擎({:?}) + 闭环{}轮 + 分区融合：色温 {:.2}/色调 {:.2}/曝光 {:.2}/tone_map {:?}",
            t.label,
            cm.scene,
            rds,
            result.adjustments.white_balance.temp,
            result.adjustments.white_balance.tint,
            result.adjustments.exposure_ev,
            result.adjustments.tone_map
        ),
    );
    let _ = loop_img;
    (final_img, result)
}

/// 闭环微调的「决策器」：基于当前渲染结果的客观指标，朝「好照片目标带」补缺口。
/// 每轮只做小步、针对性修正；最终由 `run_auto_loop` 的 goodness+伪影检测决定保留/回退，
/// 所以越修越偏离目标会被丢弃（不会比原图更差）。
///
/// `strength` 放大「风格增强」步长（反差/鲜艳），但**曝光位置步长不放大**——曝光是中性
/// 必须放对的位置，弱强档都该放对；只有风格类（反差/胶片/暗部/鲜艳）随档位强弱。
fn refine_correction(
    m: &ImageMetrics,
    prev: &Adjustments,
    target: &ToneTarget,
    tone: &Tonality,
    strength: f32,
) -> Adjustments {
    let mut a = prev.clone();

    // 曝光：把中位数拉向目标带中心（小步，避免跳变；**不乘 strength**，位置必须正确）
    let med_ctr = (target.med_lo + target.med_hi) / 2.0;
    let d = med_ctr - m.tone.median_l;
    if d.abs() > 0.01 {
        a.exposure_ev += (d * 0.6).clamp(-0.3, 0.3);
    }

    // 反差：把 std_l 推向目标带（低调/剪影不加全局反差，避免把暗部推入死黑/把主体灰化，
    // 交给分区融合的暗部提亮处理局部对比）。步长随强度放大。
    let std_ctr = (target.std_lo + target.std_hi) / 2.0;
    if tone.key != Key::Low {
        let ds = std_ctr - m.tone.std_l;
        if ds > 0.0 {
            a.grade.contrast += (ds * 0.8 * strength).clamp(0.0, 0.1);
        } else if ds < 0.0 {
            a.grade.contrast -= (-ds * 0.5 * strength).clamp(0.0, 0.08);
        }
    }

    // 鲜艳度：灰扑扑（mean_c 低）就补一点，朝目标带中心。步长随强度放大。
    let c_ctr = (target.c_lo + target.c_hi) / 2.0;
    let dc = c_ctr - m.color.mean_c;
    if dc > 0.0 {
        a.color.vibrance += (dc * 1.5 * strength).clamp(0.0, 0.15);
    }

    // 安全夹紧（不依赖全局 clamp 也保证不出界）
    a.grade.contrast = a.grade.contrast.clamp(-0.25, 0.55);
    a.exposure_ev = a.exposure_ev.clamp(-2.0, 2.0);
    a.color.vibrance = a.color.vibrance.clamp(0.0, 0.5);
    a
}

/// 平滑插值（smoothstep）。
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 按原图亮度分三区权重：暗部→提亮版、中间调→全局基线、高光→压缩版。
/// 最暗区(luma<0.06)权重给全局，确保剪影主体保持纯黑不被灰化。
#[inline]
fn zone_weights(l: f32) -> (f32, f32, f32) {
    let ws = if l < 0.06 {
        0.0
    } else if l < 0.35 {
        smoothstep(0.06, 0.35, l)
    } else {
        0.0
    };
    let wh = if l > 0.75 {
        smoothstep(0.75, 1.0, l)
    } else {
        0.0
    };
    let wm = (1.0 - ws - wh).max(0.0);
    (ws, wm, wh)
}

/// 分区影调融合（darktable Tone Equalizer 轻量版，用户要的"多渲染+融合"）：
/// 渲三份——全局基线 / 暗部提亮版 / 高光压缩版——按原图亮度遮罩软合成，
/// 实现 dodge&burn、保局部对比=「光比好、有质感」，且低调/剪影主体不被整体提亮。
fn zone_blend(
    img: &DynamicImage,
    base_adj: &Adjustments,
    _base: &ImageMetrics,
    strength: f32,
) -> RgbImage {
    let g = render(img, base_adj);

    // 暗部提亮版（增量随强度放大：强档局部对比更猛，弱档更克制）
    let mut a_sh = base_adj.clone();
    a_sh.grade.shadow_lift = (a_sh.grade.shadow_lift + 0.18 * strength).min(0.5);
    a_sh.grade.deep_shadow_lift = (a_sh.grade.deep_shadow_lift + 0.12 * strength).min(0.5);
    let sh = render(img, &a_sh);

    // 高光压缩版（shoulder 自然衰变，防死白；增量随强度放大）
    let mut a_hi = base_adj.clone();
    a_hi.grade.light_ratio = (a_hi.grade.light_ratio + 0.10 * strength).min(0.6);
    a_hi.grade.film_curve = (a_hi.grade.film_curve + 0.06 * strength).min(0.4);
    let hi = render(img, &a_hi);

    let (w, h) = g.dimensions();
    let orig = img.to_rgb8();
    let mut out = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let o = orig.get_pixel(x, y).0;
            let luma = (0.2126 * o[0] as f32 + 0.7152 * o[1] as f32 + 0.0722 * o[2] as f32) / 255.0;
            let (ws, wm, wh) = zone_weights(luma);
            let gp = g.get_pixel(x, y).0;
            let sp = sh.get_pixel(x, y).0;
            let hp = hi.get_pixel(x, y).0;
            let r = (ws * sp[0] as f32 + wm * gp[0] as f32 + wh * hp[0] as f32).round() as u8;
            let gg = (ws * sp[1] as f32 + wm * gp[1] as f32 + wh * hp[1] as f32).round() as u8;
            let b = (ws * sp[2] as f32 + wm * gp[2] as f32 + wh * hp[2] as f32).round() as u8;
            out.put_pixel(x, y, Rgb([r, gg, b]));
        }
    }
    out
}
