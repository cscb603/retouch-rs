// 参考匹配真实验证：source 朝 reference 的影调/色彩迁移，且不毁图。
use image::DynamicImage;
use retouch_core::analyze::analyze;
use retouch_core::pipeline::render;
use retouch_core::reference::run_reference_match;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: verify_refmatch <source> <reference> [match_strength 0..1]");
        std::process::exit(1);
    }
    let src_path = &args[1];
    let ref_path = &args[2];
    let strength: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.6);

    let src = image::open(src_path).expect("open source");
    let r#ref = image::open(ref_path).expect("open reference");

    let m_src = analyze(&src);
    let m_ref = analyze(&r#ref);

    let ref_m = analyze(&r#ref);
    let (_out_img, res) = run_reference_match(&src, &ref_m, 1024, 12, strength);
    let adj = res.adjustments;
    let after = render(&src, &adj);
    let m_after = analyze(&DynamicImage::ImageRgb8(after));

    let verdict = judge(
        m_src.tone.median_l,
        m_ref.tone.median_l,
        m_after.tone.median_l,
        m_after.exposure.highlight_clip_pct,
        m_src.exposure.highlight_clip_pct,
    );

    println!("src  : {:<28} med={:.3} mean={:.3} hiClip={:.2}",
        short(src_path), m_src.tone.median_l, m_src.tone.mean_l, m_src.exposure.highlight_clip_pct);
    println!("ref  : {:<28} med={:.3} mean={:.3} hiClip={:.2}",
        short(ref_path), m_ref.tone.median_l, m_ref.tone.mean_l, m_ref.exposure.highlight_clip_pct);
    println!("after: med={:.3} mean={:.3} hiClip={:.2} | strength={:.2} ev={:.2} tmap={:?}",
        m_after.tone.median_l, m_after.tone.mean_l, m_after.exposure.highlight_clip_pct,
        strength, adj.exposure_ev, adj.tone_map);
    println!("{}", verdict);
}

fn judge(src_med: f32, ref_med: f32, after_med: f32, after_hi: f32, src_hi: f32) -> String {
    // 1) 不能冲过曝
    if after_hi > src_hi + 0.08 {
        return format!("✗ 高光冲过曝 (src {:.2} -> {:.2})", src_hi, after_hi);
    }
    // 2) 应朝 reference 靠拢（方向正确）
    let moved_toward = (after_med - src_med).signum() == (ref_med - src_med).signum();
    if !moved_toward {
        return format!("✗ 方向反了 (src {:.3} -> {:.3}, ref {:.3})", src_med, after_med, ref_med);
    }
    // 3) 幅度合理：不应一步到位或超过 reference
    let overshoot = (after_med - ref_med).abs() > (src_med - ref_med).abs();
    if overshoot {
        return format!("✗ 过冲超过参考 (after {:.3} > ref {:.3})", after_med, ref_med);
    }
    format!("✓ 朝参考靠拢且未过曝 (src {:.3} -> after {:.3} -> ref {:.3})", src_med, after_med, ref_med)
}

fn short(p: &str) -> String {
    let n = std::path::Path::new(p).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let chars: Vec<char> = n.chars().collect();
    if chars.len() > 22 { chars.iter().skip(chars.len()-22).collect() } else { n }
}
