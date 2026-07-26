use image::{DynamicImage, Rgb, RgbImage};
use retouch_core::analyze::analyze;
use retouch_core::auto::run_auto;
use retouch_core::reference::run_reference_match;

fn solid(hue: f32, sat: f32, val: f32) -> DynamicImage {
    // HSV-ish to sRGB for quick solid color; hue in degrees
    let c = val * sat;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = val - c;
    let (r, g, b) = match (hue / 60.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let px = |v: f32| ((v + m) * 255.0).round() as u8;
    let img = RgbImage::from_pixel(256, 256, Rgb([px(r), px(g), px(b)]));
    DynamicImage::ImageRgb8(img)
}

fn main() {
    // ---- 1. 参考匹配色调迁移：紫花 -> 暖橙参考 ----
    let purple = solid(270.0, 0.55, 0.45); // 紫、中饱和、中暗
    let orange_ref = solid(30.0, 0.70, 0.55); // 暖橙参考
    let ref_m = analyze(&orange_ref);
    println!("=== 参考匹配色调迁移测试 ===");
    println!(
        "原图 mean_h={:.1}°  ref mean_h={:.1}°",
        analyze(&purple).color.mean_h_deg,
        ref_m.color.mean_h_deg
    );

    let (matched, res) = run_reference_match(&purple, &ref_m, 256, 5, 1.0);
    let m_out = analyze(&DynamicImage::ImageRgb8(matched));
    let diff_before = (270.0f32 - 30.0).abs().min(360.0 - (270.0f32 - 30.0).abs());
    let diff_after = (m_out.color.mean_h_deg - 30.0)
        .abs()
        .min(360.0 - (m_out.color.mean_h_deg - 30.0).abs());
    println!(
        "匹配后 mean_h={:.1}°  距离参考: 前={:.1}° 后={:.1}°  护栏={}  强度={:.1}",
        m_out.color.mean_h_deg, diff_before, diff_after, res.guardrail_passed, res.adjustments.mix
    );
    assert!(diff_after < diff_before * 0.75, "色调应向参考迁移至少 25%");
    println!("[通过] 色调迁移生效\n");

    // ---- 2. 一键中性力度：低反差灰图应被拉开反差 ----
    let gray = {
        let mut img = RgbImage::new(256, 256);
        for y in 0..256 {
            for x in 0..256 {
                let v = (80 + (x + y) / 8).min(150) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        DynamicImage::ImageRgb8(img)
    };
    let base = analyze(&gray);
    println!("=== 一键中性力度测试 ===");
    println!(
        "原图 mean_l={:.3} std_l={:.3} mean_c={:.3}",
        base.tone.mean_l, base.tone.std_l, base.color.mean_c
    );

    let (auto, _) = run_auto(&gray, 256, 4, 1.0);
    let m_auto = analyze(&DynamicImage::ImageRgb8(auto));
    println!(
        "自动后 mean_l={:.3} std_l={:.3} mean_c={:.3}",
        m_auto.tone.mean_l, m_auto.tone.std_l, m_auto.color.mean_c
    );
    assert!(
        m_auto.tone.std_l > base.tone.std_l * 1.20,
        "一键中性应明显提升反差"
    );
    println!("[通过] 一键中性力度足够\n");

    // ---- 3. 强度滑块 0.0 -> 0.5 -> 1.0 应产生渐变 ----
    println!("=== 匹配强度渐变测试 ===");
    for (label, strength) in [("0.0", 0.0f32), ("0.5", 0.5), ("1.0", 1.0)] {
        let (out, _) = run_reference_match(&purple, &ref_m, 256, 3, strength);
        let m = analyze(&DynamicImage::ImageRgb8(out));
        let dist = (m.color.mean_h_deg - ref_m.color.mean_h_deg)
            .abs()
            .min(360.0 - (m.color.mean_h_deg - ref_m.color.mean_h_deg).abs());
        println!(
            "强度 {} -> mix={:.2} mean_h={:.1}° 距参考={:.1}°",
            label, strength, m.color.mean_h_deg, dist
        );
    }
}
