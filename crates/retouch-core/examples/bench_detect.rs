//! v0.6.6 自动污点检测：参数扫描 + 性能基准。
//!
//! 选型判据（按重要性排序）：
//!   1. 误检 = 0（误检的笔触会被真实修复、破坏画面，比漏检危险得多）
//!   2. 召回尽量高（覆盖 3~13px 直径的常见传感器灰尘）
//!   3. 预览尺寸(1400px 长边) 耗时 < 300ms（否则不能同步跑在 UI 线程）
//!
//! 运行：cargo run --release -p retouch-core --example bench_detect

use image::{Rgb, RgbImage};
use retouch_core::detect_spots::{detect_spots, DetectParams};
use std::time::Instant;

const DUST_RADII: [i32; 6] = [1, 2, 3, 4, 5, 6];

/// 合成「天空渐变 + 局部高频纹理 + 已知灰尘」测试图，并返回灰尘真值。
/// 灰尘按固定网格摆放（每种半径 3 个），避免随机重叠干扰统计。
fn synth(w: u32, h: u32) -> (RgbImage, Vec<(u32, u32, i32)>) {
    let mut img = RgbImage::new(w, h);
    // 天空渐变
    for y in 0..h {
        for x in 0..w {
            let v = 150.0 + (y as f32 / h as f32) * 80.0;
            img.put_pixel(x, y, Rgb([v as u8, (v + 6.0) as u8, (v + 18.0) as u8]));
        }
    }
    // 下 1/3 高频纹理（模拟树叶）——检测器不应在此刷屏
    let mut seed = 0xC0FFEEu32;
    for y in (h * 2 / 3)..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = (seed >> 25) as u8;
            img.put_pixel(x, y, Rgb([40 + n, 70 + n, 30 + n]));
        }
    }
    // 灰尘：6 种半径 × 3 个，网格摆放在上 1/2 天空区
    let mut truth = Vec::new();
    let cols = 6u32;
    let rows = 3u32;
    for r_i in 0..cols {
        for row in 0..rows {
            let r = DUST_RADII[r_i as usize];
            let cx = w * (r_i + 1) / (cols + 1);
            let cy = h * (row + 1) / (rows * 3);
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx * dx + dy * dy <= r * r {
                        let px = (cx as i32 + dx).clamp(0, w as i32 - 1) as u32;
                        let py = (cy as i32 + dy).clamp(0, h as i32 - 1) as u32;
                        img.put_pixel(px, py, Rgb([30u8, 30, 32]));
                    }
                }
            }
            truth.push((cx, cy, r));
        }
    }
    (img, truth)
}

struct Score {
    hit: usize,
    total: usize,
    fp: usize,
    ms: u128,
    per_radius: Vec<(i32, usize, usize)>,
}

fn evaluate(img: &RgbImage, truth: &[(u32, u32, i32)], p: &DetectParams) -> Score {
    let (w, h) = img.dimensions();
    let _ = detect_spots(img, p); // 预热
    let t0 = Instant::now();
    let spots = detect_spots(img, p);
    let ms = t0.elapsed().as_millis();

    let mut matched = vec![false; spots.len()];
    let mut hit = 0usize;
    let mut per: std::collections::BTreeMap<i32, (usize, usize)> = Default::default();
    for (tx, ty, tr) in truth {
        let e = per.entry(*tr).or_insert((0, 0));
        e.1 += 1;
        let mut found = false;
        for (i, s) in spots.iter().enumerate() {
            if matched[i] {
                continue;
            }
            let sx = s.cx * w as f32;
            let sy = s.cy * h as f32;
            let d = ((sx - *tx as f32).powi(2) + (sy - *ty as f32).powi(2)).sqrt();
            if d <= *tr as f32 + 5.0 {
                matched[i] = true;
                found = true;
                break;
            }
        }
        if found {
            hit += 1;
            e.0 += 1;
        }
    }
    let fp = matched.iter().filter(|m| !**m).count();
    Score {
        hit,
        total: truth.len(),
        fp,
        ms,
        per_radius: per.into_iter().map(|(r, (a, b))| (r, a, b)).collect(),
    }
}

fn main() {
    println!("=== v0.6.6 自动污点检测：参数扫描 ===");
    println!("测试图 1400×933：天空渐变 + 下1/3高频纹理 + 18 个已知灰尘（r=1..6，各 3 个）\n");
    let (img, truth) = synth(1400, 933);

    println!(
        "{:<26} {:>7} {:>7} {:>7}   分半径命中",
        "配置", "召回", "误检", "耗时"
    );
    println!("{}", "─".repeat(88));

    let combos: Vec<(&str, u32, Vec<u32>)> = vec![
        ("ksize=5  单尺度", 5, vec![1]),
        ("ksize=9  单尺度", 9, vec![1]),
        ("ksize=13 单尺度", 13, vec![1]),
        ("ksize=17 单尺度", 17, vec![1]),
        ("ksize=5  多尺度[1,3,6]", 5, vec![1, 3, 6]),
        ("ksize=9  多尺度[1,3]", 9, vec![1, 3]),
    ];

    let mut best: Option<(String, usize, usize, u128)> = None;
    for (name, ks, scales) in combos {
        let mut p = DetectParams {
            median_ksize: ks,
            scales: scales.clone(),
            ..Default::default()
        };
        p.median_ksize = ks;
        let s = evaluate(&img, &truth, &p);
        let per: Vec<String> = s
            .per_radius
            .iter()
            .map(|(r, a, b)| format!("r{}:{}/{}", r, a, b))
            .collect();
        let flag = if s.fp > 0 {
            "❌误检"
        } else if s.ms > 300 {
            "⚠超时"
        } else {
            "✅"
        };
        println!(
            "{:<26} {:>5}/{:<2} {:>7} {:>5}ms   {}  {}",
            name,
            s.hit,
            s.total,
            s.fp,
            s.ms,
            per.join(" "),
            flag
        );
        if s.fp == 0 && s.ms <= 300 {
            let better = match &best {
                None => true,
                Some((_, bh, _, bms)) => s.hit > *bh || (s.hit == *bh && s.ms < *bms),
            };
            if better {
                best = Some((name.to_string(), s.hit, s.fp, s.ms));
            }
        }
    }

    println!("{}", "─".repeat(88));
    match best {
        Some((n, hit, _, ms)) => println!(
            "\n✅ 推荐配置：{}   （零误检、{}ms、召回 {}/18）",
            n, ms, hit
        ),
        None => println!("\n❌ 无配置同时满足「零误检 + <300ms」，需重新设计判据"),
    }

    // 大图耗时参考（只在最终配置下跑）
    println!("\n=== 不同分辨率耗时（默认配置）===");
    for (w, h) in [(1400u32, 933u32), (2400, 1600), (6000, 4000)] {
        let (im, _) = synth(w, h);
        let p = DetectParams::default();
        let _ = detect_spots(&im, &p);
        let t0 = Instant::now();
        let n = detect_spots(&im, &p).len();
        println!(
            "  {:>5}×{:<5} ({:>4.1}MPx) → {:>5}ms  检出 {} 处",
            w,
            h,
            (w as f64 * h as f64) / 1e6,
            t0.elapsed().as_millis(),
            n
        );
    }
    println!("\n注：UI 只在预览基图（长边 ≤1400px）上检测，全分辨率耗时仅供参考。");
}
