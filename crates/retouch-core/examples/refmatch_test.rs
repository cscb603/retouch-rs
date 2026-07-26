//! 无头测试参考图匹配：把 src 图的影调朝 ref 图靠拢，输出前后指标对比。
//! 用法: cargo run --example refmatch_test --release <src> <ref> <out>

use retouch_core::analyze::analyze;
use retouch_core::reference::run_reference_match;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: refmatch_test <src> <ref> <out>");
        std::process::exit(2);
    }
    let src = PathBuf::from(&args[1]);
    let rref = PathBuf::from(&args[2]);
    let out = PathBuf::from(&args[3]);

    let src_img = image::open(&src).expect("open src");
    let ref_img = image::open(&rref).expect("open ref");
    let ref_m = analyze(&ref_img);

    let (out_img, result) = run_reference_match(&src_img, &ref_m, 1024, 8, 1.0);
    out_img.save(&out).expect("save out");

    println!("=== 参考图匹配测试 ===");
    println!("源图: {}", src.display());
    println!("参考图: {}", rref.display());
    println!("输出: {}", out.display());
    println!("\n-- 指标对比 --");
    println!(
        "mean_l   : 源 {:.3} -> 后 {:.3} | 参考 {:.3}  (目标)",
        result.metrics_before.tone.mean_l,
        result.metrics_after.tone.mean_l,
        result.ref_metrics.tone.mean_l
    );
    println!(
        "std_l   : 源 {:.3} -> 后 {:.3} | 参考 {:.3}",
        result.metrics_before.tone.std_l,
        result.metrics_after.tone.std_l,
        result.ref_metrics.tone.std_l
    );
    println!(
        "mean_c  : 源 {:.3} -> 后 {:.3} | 参考 {:.3}",
        result.metrics_before.color.mean_c,
        result.metrics_after.color.mean_c,
        result.ref_metrics.color.mean_c
    );
    println!(
        "过曝%   : 源 {:.2} -> 后 {:.2} | 参考 {:.2}",
        result.metrics_before.exposure.highlight_clip_pct,
        result.metrics_after.exposure.highlight_clip_pct,
        result.ref_metrics.exposure.highlight_clip_pct
    );
    println!(
        "肤色L   : 源 {:.3} -> 后 {:.3} | 参考 {:.3}",
        result.metrics_before.skin.mean_l,
        result.metrics_after.skin.mean_l,
        result.ref_metrics.skin.mean_l
    );
    println!("\n-- 判定 --");
    let after = &result.metrics_after;
    let over_exposed = after.tone.mean_l > 0.62
        || after.exposure.highlight_clip_pct
            > result.metrics_before.exposure.highlight_clip_pct + 0.5;
    let skin_blown = after.skin.mean_l > 0.82;
    println!("护栏通过: {}", result.guardrail_passed);
    println!(
        "过曝/油光: {}",
        if over_exposed { "❌ FAIL" } else { "✅ OK" }
    );
    println!("肤色过亮: {}", if skin_blown { "❌ FAIL" } else { "✅ OK" });
    println!("\n-- 迭代日志 --");
    for l in &result.log {
        println!("  {}", l);
    }
}
