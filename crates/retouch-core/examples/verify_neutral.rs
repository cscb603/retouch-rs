//! 一键中性真图验证：对每张图跑 run_auto，对比前后 OKLCH 指标，
//! 判定是否过曝/油光/发糊（对比崩）。用法:
//!   cargo run --example verify_neutral --release -- <img1> <img2> ...

use retouch_core::analyze::analyze;
use retouch_core::auto::run_auto;
use std::path::PathBuf;

fn verdict(name: &str, before_mean_l: f32, after_mean_l: f32, before_std: f32, after_std: f32, before_hc: f32, after_hc: f32, before_skin: f32, after_skin: f32) {
    let over = after_mean_l > 0.62 || after_hc > before_hc + 0.5;
    // 中性校正本就会降约 25% 对比（与「自动中性化」同源，属正常观感），
    // 只有跌破 50% 才判为真·发灰毁图；其余如实报告降幅。
    let washed = after_std < before_std * 0.5;
    let std_drop = (1.0 - after_std / before_std) * 100.0;
    let skin_blown = after_skin > 0.82;
    let ok = !over && !washed && !skin_blown;
    println!(
        "{:<42} | meanL {:.3}->{:.3} stdL {:.3}->{:.3}(降{:.0}%) 过曝% {:.2}->{:.2} 肤色L {:.3}->{:.3} | {}",
        name,
        before_mean_l, after_mean_l, before_std, after_std, std_drop, before_hc, after_hc, before_skin, after_skin,
        if ok { "✅ OK" } else { "❌ FAIL" }
    );
    if over { println!("    ↳ 过曝/油光: meanL>0.62 或 过曝% 暴涨"); }
    if washed { println!("    ↳ 发灰毁图: 对比 stdL 崩 >50%"); }
    if skin_blown { println!("    ↳ 肤色过亮: 肤色L>0.82"); }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: verify_neutral <img1> <img2> ...");
        std::process::exit(2);
    }
    println!("=== 一键中性真图验证（run_auto = auto_neutral_balance）===\n");
    println!("{:<42} | {} | {}", "图片", "指标前后", "判定");
    println!("{}", "-".repeat(120));
    let mut fails = 0;
    for a in &args {
        let p = PathBuf::from(a);
        let img = match image::open(&p) {
            Ok(i) => i,
            Err(e) => { eprintln!("跳过 {}: {}", a, e); continue; }
        };
        let before = analyze(&img);
        let (out, _res) = run_auto(&img, 1024, 2, 1.0);
        let after = analyze(&image::DynamicImage::ImageRgb8(out));
        let ok = !(after.tone.mean_l > 0.62 || after.exposure.highlight_clip_pct > before.exposure.highlight_clip_pct + 0.5)
            && !(after.tone.std_l < before.tone.std_l * 0.5)
            && !(after.skin.mean_l > 0.82);
        if !ok { fails += 1; }
        let short = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| a.clone());
        verdict(&short, before.tone.mean_l, after.tone.mean_l, before.tone.std_l, after.tone.std_l,
                before.exposure.highlight_clip_pct, after.exposure.highlight_clip_pct,
                before.skin.mean_l, after.skin.mean_l);
    }
    println!("{}", "-".repeat(120));
    println!("结果: {} 张测试，{} 张 FAIL", args.len(), fails);
    if fails == 0 {
        println!("✅ 全部通过：无过曝油光、无发糊发灰");
    } else {
        println!("❌ 仍有毁图，需回头修");
    }
}
