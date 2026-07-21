//! 影调感知质量引擎 · 真实照片质量验证
//!
//! 与 verify_safe_real（只判"不毁"）不同，本例按"修好看"判定：
//! - 打印分类（十大影调）、选用的 tone_map / exposure_ev；
//! - 打印 before/after 的 mean_l / dynamic_range / 高光死白 / 阴影死白 / 肤色亮度；
//! - 对每类给出质量判定（低调是否守住低调且不新增死白、中调是否平衡、高调是否乳白）。

use image::DynamicImage;
use retouch_core::analyze::analyze;
use retouch_core::pipeline::render;
use retouch_core::tonemap::{classify_tonality, tonal_adjustments};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: verify_quality <图1> [图2 ...]");
        std::process::exit(1);
    }

    println!(
        "{:<20} | {:>6} {:>6} {:>6} {:>6} | {:>7} {:>7} {:>7} | {:>5} {:>5} | {:>6} {:>6} | 判定",
        "图", "bMed", "aMed", "tgt", "dr", "hiClip", "shClip", "skL", "key", "span", "tmap", "ev"
    );
    println!("{}", "-".repeat(130));

    for path in &args {
        let img = match image::open(path) {
            Ok(i) => DynamicImage::ImageRgb8(i.to_rgb8()),
            Err(e) => {
                eprintln!("无法打开 {}: {}", path, e);
                continue;
            }
        };
        let base = analyze(&img);
        let t = classify_tonality(&base);
        let adj = tonal_adjustments(&img, true, 1.0);

        let tmap = match adj.tone_map {
            retouch_core::pipeline::ToneMapMode::Agx => "Agx",
            retouch_core::pipeline::ToneMapMode::Filmic => "Film",
            retouch_core::pipeline::ToneMapMode::None => "None",
        };
        let target_l: f32 = match t.key {
            retouch_core::tonemap::Key::Low => (base.tone.median_l + 0.03).min(0.46),
            retouch_core::tonemap::Key::Mid => 0.50,
            retouch_core::tonemap::Key::High => (base.tone.median_l + 0.04).min(0.66),
        };

        let final_img = render(&img, &adj);
        let mfin = analyze(&DynamicImage::ImageRgb8(final_img));

        let key_i = match t.key {
            retouch_core::tonemap::Key::Low => 0,
            retouch_core::tonemap::Key::Mid => 1,
            retouch_core::tonemap::Key::High => 2,
        };
        let span_i = match t.span {
            retouch_core::tonemap::Span::Short => 0,
            retouch_core::tonemap::Span::Mid => 1,
            retouch_core::tonemap::Span::Long => 2,
        };

        // 质量判定（用中位数：基调是否守住/放对位置）
        let verdict = judge(t.key, base.tone.median_l, mfin.tone.median_l, target_l,
                            mfin.exposure.highlight_clip_pct, base.exposure.highlight_clip_pct,
                            mfin.exposure.shadow_clip_pct, base.exposure.shadow_clip_pct,
                            mfin.skin.mean_l);

        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        let chars: Vec<char> = name.chars().collect();
        let short: String = if chars.len() > 18 {
            chars.iter().skip(chars.len() - 18).collect()
        } else {
            name.clone()
        };

        println!(
            "{:<20} | {:6.3} {:6.3} {:6.3} {:6.3} | {:7.2} {:7.2} {:7.2} | {:5} {:5} | {:>6} {:6.2} | {}",
            short,
            base.tone.median_l,
            mfin.tone.median_l,
            target_l,
            base.dynamic_range,
            mfin.exposure.highlight_clip_pct,
            mfin.exposure.shadow_clip_pct,
            mfin.skin.mean_l,
            key_i,
            span_i,
            tmap,
            adj.exposure_ev,
            verdict,
        );
        println!("   影调={} 标签", t.label);
    }
}

fn judge(
    key: retouch_core::tonemap::Key,
    base_l: f32,
    after_l: f32,
    target_l: f32,
    after_hi: f32,
    base_hi: f32,
    after_sh: f32,
    base_sh: f32,
    skin_l: f32,
) -> &'static str {
    // 冲脸：任何皮肤区域被提得过亮
    if skin_l > 0.82 {
        return "✗ 冲脸";
    }
    // 高调允许均匀乳白过曝，不判死白；中/低调新增死白=毁
    let hi_ok = match key {
        retouch_core::tonemap::Key::High => after_hi <= base_hi + 3.0, // 高调允许略增
        _ => after_hi <= base_hi + 0.5, // 中/低调几乎不新增死白
    };
    if !hi_ok {
        return "✗ 高光死白新增";
    }
    // 阴影：低调/中调不新增大面积死黑（除非本就是剪影意图）
    match key {
        retouch_core::tonemap::Key::Low => {
            // 低调：守住低调（after 不应比 base 显著变亮），且暗部细节应略回（shClip 不暴增）
            if after_l > base_l + 0.06 {
                return "✗ 低调被提亮";
            }
            if after_sh > base_sh + 5.0 {
                return "✗ 死黑增多";
            }
            "✓ 低调守住+暗部细节"
        }
        retouch_core::tonemap::Key::Mid => {
            // 中调：平衡，after 接近 target
            if (after_l - target_l).abs() > 0.08 {
                return "✗ 偏离目标曝光";
            }
            "✓ 中调平衡"
        }
        retouch_core::tonemap::Key::High => {
            // 高调：允许亮（乳白）；只要仍明显高调（median ≥ 0.58）即合格。
            // 略低于原图也 OK——把过亮原图驯到健康高调是改进，不是毁图。
            if after_l < 0.58 {
                return "✗ 高调被压暗";
            }
            "✓ 高调乳白"
        }
    }
}
