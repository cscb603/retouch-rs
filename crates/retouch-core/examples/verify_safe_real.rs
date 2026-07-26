//! 端到端验证：对真实照片跑一键中性（run_auto），比对前后指标，确认「绝不毁图」。
//! 用法：cargo run -p retouch-core --example verify_safe_real -- <图1> [图2] ...

use image::DynamicImage;
use retouch_core::analyze::analyze;
use retouch_core::auto::run_auto;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: verify_safe_real <图1> [图2] ...");
        std::process::exit(1);
    }
    let mut all_pass = true;
    for path in &args[1..] {
        let img = match image::open(path) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("打开失败 {}: {}", path, e);
                continue;
            }
        };
        let before = analyze(&img);
        let (_out, res) = run_auto(&img, 1024, 2, 1.0);
        let after = &res.metrics_after;

        let cap = if before.tone.mean_l > 0.55 {
            before.tone.mean_l + 0.02
        } else {
            0.58
        };
        let over_bright = after.tone.mean_l > cap;
        let blow_hi = after.exposure.highlight_clip_pct > before.exposure.highlight_clip_pct + 0.08;
        let face = before.skin.ratio > 0.03 && after.skin.mean_l > 0.82;
        let gray = after.tone.std_l < before.tone.std_l * 0.60;
        let pass = !(over_bright || blow_hi || face || gray);
        if !pass {
            all_pass = false;
        }

        let name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        println!(
            "[{}] L {:.3}->{:.3}(cap {:.3}) | hi {:.2}->{:.2} | std {:.3}->{:.3} | skinL {:.3}->{:.3} | {}",
            name,
            before.tone.mean_l,
            after.tone.mean_l,
            cap,
            before.exposure.highlight_clip_pct,
            after.exposure.highlight_clip_pct,
            before.tone.std_l,
            after.tone.std_l,
            before.skin.mean_l,
            after.skin.mean_l,
            if pass { "PASS ✅" } else { "FAIL ❌" }
        );
    }
    println!(
        "\n总结: {}",
        if all_pass {
            "全部 PASS ✅"
        } else {
            "存在 FAIL ❌"
        }
    );
}
