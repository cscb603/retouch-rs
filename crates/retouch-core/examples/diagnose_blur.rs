//! 诊断「一键中性」糊图根因：逐字段 A/B 测清晰度（拉普拉斯方差）。
//! 用法：cargo run -p retouch-core --example diagnose_blur --quiet -- <图路径>
use image::{GenericImageView, RgbImage};
use retouch_core::advanced::Advanced;
use retouch_core::auto::run_auto;
use retouch_core::auto_color::auto_neutral_balance;
use retouch_core::pipeline::{
    render, Adjustments, ColorGrade, DefakeColor, Grade, HslRegions, WhiteBalance,
};
use retouch_core::tonemap;

fn gray(img: &RgbImage) -> Vec<f64> {
    let (w, h) = img.dimensions();
    let mut g = vec![0.0f64; (w * h) as usize];
    for (i, p) in img.pixels().enumerate() {
        g[i] = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
    }
    g
}

/// 拉普拉斯方差 = 高频能量代理，越大越清晰。
fn sharpness(img: &RgbImage) -> f64 {
    let (w, h) = img.dimensions();
    let g = gray(img);
    let (wi, hi) = (w as usize, h as usize);
    let mut sum = 0.0f64;
    let mut sum2 = 0.0f64;
    let mut n = 0u64;
    for y in 1..hi - 1 {
        for x in 1..wi - 1 {
            let idx = y * wi + x;
            let lap = 4.0 * g[idx] - g[idx - 1] - g[idx + 1] - g[idx - wi] - g[idx + wi];
            sum += lap;
            sum2 += lap * lap;
            n += 1;
        }
    }
    let mean = sum / n as f64;
    sum2 / n as f64 - mean * mean
}

fn main() {
    let path = std::env::args().nth(1).expect("need image path");
    let img = image::open(&path).expect("open");
    let (w, h) = img.dimensions();
    // 缩到 1400 宽加速（相对清晰度不变）
    let small = img.resize(1400, 1400 * h / w, image::imageops::FilterType::Lanczos3);
    let (sw, sh) = small.dimensions();

    let (_out, res) = run_auto(&small, 1024, 1, 1.0);
    let adj = res.adjustments;
    println!("生成的完整参数 ADJ =\n{:#?}\n", adj);

    // 定位 advanced 真实来源
    let t_adj = tonemap::tonal_adjustments(&small, true, 1.0);
    let anb_adj = auto_neutral_balance(&small, true).to_adjustments();
    println!(
        "来源对比 | run_auto.advanced.enabled={} | tonal_adjustments.advanced.enabled={} | anb.advanced.enabled={}",
        adj.advanced.freqsep.enabled || adj.advanced.pyramid.enabled,
        t_adj.advanced.freqsep.enabled || t_adj.advanced.pyramid.enabled,
        anb_adj.advanced.freqsep.enabled || anb_adj.advanced.pyramid.enabled,
    );

    // 原图（pipeline 全 default：defake off、grade default 等）
    let base = render(&small, &Adjustments::default());
    let full = render(&small, &adj);
    println!(
        "尺寸 {}x{} | 原图基准清晰度={:.1} | 一键中性完整={:.1} (相对原图 {:.1}%)",
        sw,
        sh,
        sharpness(&base),
        sharpness(&full),
        100.0 * sharpness(&full) / sharpness(&base)
    );
    println!("{}\n", "-".repeat(90));

    // 逐个关掉「可能糊」的字段，看清晰度回升多少
    let mut variants: Vec<(&str, Adjustments)> = Vec::new();

    let mut a = adj.clone();
    a.defake = DefakeColor::default();
    variants.push(("关 defake", a));

    let mut a = adj.clone();
    a.grade = Grade::default();
    variants.push(("关 grade(对比/胶片/暗部)", a));

    let mut a = adj.clone();
    a.color = ColorGrade::default();
    variants.push(("关 color(振动/饱和)", a));

    let mut a = adj.clone();
    a.hsl = HslRegions::default();
    variants.push(("关 hsl", a));

    let mut a = adj.clone();
    a.white_balance = WhiteBalance::default();
    variants.push(("关 white_balance", a));

    let mut a = adj.clone();
    a.defake = DefakeColor::default();
    a.grade = Grade::default();
    a.color = ColorGrade::default();
    a.hsl = HslRegions::default();
    a.white_balance = WhiteBalance::default();
    variants.push(("关 defake+grade+color+hsl+wb(只留曝光/影调)", a));

    let mut a = adj.clone();
    a.advanced = Advanced::default();
    variants.push(("关 advanced(磨皮/融合)", a));

    println!(
        "{:<42} | {:>10} | {:>12}",
        "关掉该字段", "清晰度", "相对原图%"
    );
    println!("{}", "-".repeat(90));
    let base_s = sharpness(&base);
    for (name, vadj) in variants {
        let out = render(&small, &vadj);
        let s = sharpness(&out);
        println!("{:<42} | {:>10.1} | {:>11.1}%", name, s, 100.0 * s / base_s);
    }
}
