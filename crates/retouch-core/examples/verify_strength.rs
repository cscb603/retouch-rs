//! 一键中性三档力度验证：对每张图跑 弱(0.5)/中(1.0)/强(1.8)，
//! 确认「增强幅度随档位单调递增」且「强档不毁图（无伪影/不过曝）」。
//! 用法：cargo run -p retouch-core --example verify_strength -- <图1> [图2 ...]

use image::DynamicImage;
use retouch_core::analyze::analyze;
use retouch_core::auto::run_auto;
use retouch_core::guardrail::is_artifact;
use retouch_core::tonemap::classify_tonality;

fn delta_rms(a: &DynamicImage, b: &DynamicImage) -> f32 {
    let a = a.to_rgb8();
    let b = b.to_rgb8();
    let (w, h) = (a.dimensions().0.min(b.dimensions().0), a.dimensions().1.min(b.dimensions().1));
    let mut s = 0.0f64;
    let mut n = 0.0f64;
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let pa = a.get_pixel(x, y).0;
            let pb = b.get_pixel(x, y).0;
            for c in 0..3 {
                let d = pa[c] as f64 - pb[c] as f64;
                s += d * d;
            }
            n += 3.0;
        }
    }
    (s / n.max(1.0)).sqrt() as f32
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("用法: verify_strength <图1> [图2 ...]");
        std::process::exit(1);
    }
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
        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        println!(
            "\n图 {:<22} | 影调 {} | 原 median={:.3} std={:.3} mc={:.3}",
            name, t.label, base.tone.median_l, base.tone.std_l, base.color.mean_c
        );
        let mut prev_rms = -1.0f32;
        for (lbl, s) in [("弱", 0.5f32), ("中", 1.0f32), ("强", 1.8f32)] {
            let (out, _res) = run_auto(&img, 1024, 4, s);
            let out_d = DynamicImage::ImageRgb8(out.clone());
            let m = analyze(&out_d);
            let rms = delta_rms(&img, &out_d);
            let art = is_artifact(&m, &base, &t);
            let mono = if rms > prev_rms { "✓单调递增" } else { "✗非递增" };
            prev_rms = rms;
            println!(
                "  {} (s={:.1}) RMS={:6.2} {} | median={:.3} std={:.3} mc={:.3} hiClip={:5.2} | 伪影={}",
                lbl,
                s,
                rms,
                mono,
                m.tone.median_l,
                m.tone.std_l,
                m.color.mean_c,
                m.exposure.highlight_clip_pct,
                art.map_or_else(|| "无".to_string(), |r| r)
            );
            let a = &_res.adjustments;
            println!(
                "         adj: ev={:.2} contrast={:.3} film={:.3} lr={:.3} shLift={:.3} vib={:.3} mix={:.2}",
                a.exposure_ev,
                a.grade.contrast,
                a.grade.film_curve,
                a.grade.light_ratio,
                a.grade.shadow_lift,
                a.color.vibrance,
                a.mix
            );
        }
    }
}
