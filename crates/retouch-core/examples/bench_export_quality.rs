//! v0.6.6 导出画质验证：S444（无色度下采样）vs 4:2:0，在彩色边缘的保真度对比。
//!
//! 白皮书 A 方案宣称「S444 消除彩色边缘发虚」，此处用 PSNR 量化验证，
//! 避免空口结论。运行：
//!   cargo run --release -p retouch-core --example bench_export_quality

use image::{Rgb, RgbImage};
use mozjpeg_rs::{Encoder as MozEncoder, QuantTableIdx, Subsampling};
use std::time::Instant;

/// 合成高饱和彩色边缘测试图（色度下采样的最坏情况：红/蓝相邻细条）。
fn synth_color_edges(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // 竖向红蓝细条纹（周期 6px）+ 斜向洋红/青分界
            let stripe = (x / 3) % 2 == 0;
            let diag = (x + y) % 160 < 80;
            let c = match (stripe, diag) {
                (true, true) => [220u8, 20, 30],   // 红
                (false, true) => [20, 40, 210],    // 蓝
                (true, false) => [210, 20, 190],   // 洋红
                (false, false) => [20, 200, 190],  // 青
            };
            img.put_pixel(x, y, Rgb(c));
        }
    }
    img
}

/// 合成人像肤色渐变（验证平滑区不因 S444 变差）。
fn synth_skin(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / w as f32;
            let s = y as f32 / h as f32;
            let r = 205.0 - t * 35.0;
            let g = 160.0 - t * 30.0 - s * 10.0;
            let b = 140.0 - t * 28.0 - s * 14.0;
            img.put_pixel(x, y, Rgb([r as u8, g as u8, b as u8]));
        }
    }
    img
}

fn psnr(a: &RgbImage, b: &RgbImage) -> f64 {
    let (ra, rb) = (a.as_raw(), b.as_raw());
    let n = ra.len().min(rb.len());
    let mut se = 0f64;
    for i in 0..n {
        let d = ra[i] as f64 - rb[i] as f64;
        se += d * d;
    }
    let mse = se / n as f64;
    if mse <= f64::EPSILON {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / mse).log10()
}

fn encode(img: &RgbImage, q: u8, sub: Subsampling) -> Vec<u8> {
    let (w, h) = img.dimensions();
    MozEncoder::default()
        .quality(q)
        .progressive(true)
        .optimize_huffman(true)
        .subsampling(sub)
        .quant_tables(QuantTableIdx::MssimTuned)
        .encode_rgb(img.as_raw(), w, h)
        .expect("encode failed")
}

fn decode(bytes: &[u8]) -> RgbImage {
    image::load_from_memory(bytes).expect("decode failed").to_rgb8()
}

fn run_case(name: &str, img: &RgbImage) {
    let q = 95u8;
    let t0 = Instant::now();
    let d444 = encode(img, q, Subsampling::S444);
    let ms444 = t0.elapsed().as_millis();

    let t1 = Instant::now();
    let d420 = encode(img, q, Subsampling::S420);
    let ms420 = t1.elapsed().as_millis();

    let p444 = psnr(img, &decode(&d444));
    let p420 = psnr(img, &decode(&d420));

    println!("【{}】 quality={}", name, q);
    println!(
        "  S444 (v0.6.6 采用) : PSNR {:>6.2} dB   {:>7} KB   {:>4}ms",
        p444,
        d444.len() / 1024,
        ms444
    );
    println!(
        "  S420 (旧默认)      : PSNR {:>6.2} dB   {:>7} KB   {:>4}ms",
        p420,
        d420.len() / 1024,
        ms420
    );
    let gain = p444 - p420;
    let cost = (d444.len() as f64 / d420.len() as f64 - 1.0) * 100.0;
    println!("  → 画质增益 {:+.2} dB，体积代价 {:+.1}%\n", gain, cost);
}

fn main() {
    println!("=== v0.6.6 导出画质验证：S444 vs 4:2:0 ===\n");
    run_case("高饱和彩色边缘（合成，最坏情况）", &synth_color_edges(1200, 800));
    run_case("肤色平滑渐变（合成，人像常见）", &synth_skin(1200, 800));

    // 实拍照片：合成图是极端情况，真实体积代价必须用实拍数据说话
    for p in std::env::args().skip(1) {
        match image::open(&p) {
            Ok(im) => {
                let rgb = im.to_rgb8();
                let (w, h) = rgb.dimensions();
                run_case(&format!("实拍 {} ({}×{})", 
                    std::path::Path::new(&p).file_name().unwrap_or_default().to_string_lossy(),
                    w, h), &rgb);
            }
            Err(e) => println!("跳过 {}：{}\n", p, e),
        }
    }

    println!("结论判据：彩色边缘增益应 ≥ 3dB；平滑区不应劣化（增益 ≥ 0）。");
    println!("注：合成条纹图是色度下采样的极端最坏情况，体积代价远高于实拍照片。");
}
