//! 一键中性 · 闭环微调实测
//!
//! 验证 v0.3.1 的「影调基线 + 闭环微调」是否真的"修过"（改动幅度肉眼可见）
//! 且没毁图（护栏通过）。重点打印：
//! - 改动幅度 RMS(sRGB)（>3/255 即肉眼可见，~0 说明等于没修）
//! - before/after 的 mean_l / std_l / mean_c（曝光/反差/鲜艳度）
//! - 每轮 score/护栏日志（证明"检测→微调/回退"闭环在跑）

use image::DynamicImage;
use retouch_core::analyze::analyze;
use retouch_core::auto::run_auto;
use retouch_core::tonemap::classify_tonality;

/// 两张图之间的 RMS 逐像素差（sRGB 0-255，每通道），抽样步进 2 提速。
fn delta_rms(a: &DynamicImage, b: &DynamicImage) -> f32 {
    let a = a.to_rgb8();
    let b = b.to_rgb8();
    let (w, h) = a.dimensions();
    let (w2, h2) = b.dimensions();
    let (w, h) = (w.min(w2), h.min(h2));
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
        eprintln!("用法: verify_loop <图1> [图2 ...]");
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
        let (out, res) = run_auto(&img, 1024, 4, 1.0);
        let after = DynamicImage::ImageRgb8(out);
        let rms = delta_rms(&img, &after);

        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());

        println!("=== {} ===", name);
        println!(
            "  影调={} | 闭环轮数={} | 护栏通过={}",
            t.label, res.rounds, res.guardrail_passed
        );
        println!(
            "  mean_l : {:.3} -> {:.3}    std_l : {:.3} -> {:.3}    mean_c : {:.3} -> {:.3}",
            base.tone.mean_l,
            res.metrics_after.tone.mean_l,
            base.tone.std_l,
            res.metrics_after.tone.std_l,
            base.color.mean_c,
            res.metrics_after.color.mean_c
        );
        let visible = if rms < 3.0 {
            "⚠ 几乎没修(≈恒等)"
        } else if rms > 45.0 {
            "⚠ 改动过大(可能过)"
        } else {
            "✓ 肉眼可见且合理"
        };
        println!("  ★ 改动幅度 RMS(sRGB) = {:.2}/255  {}", rms, visible);
        for l in &res.log {
            println!("    {}", l);
        }
        println!();
    }
}
