//! 真机实效验收：生成一张带斜电线的真实感纹理图，对四档污点修复各跑一次，
//! 量化「电线区修复后 PSNR」与「背景守恒（非线区最大改动）」。
//! 目的：① 证明 PatchMatch 档真能修细线；② 证明三档旧功能不回归
//!        （都能跑、都只改线区、不破坏背景）。
//!
//! 运行：`cargo run --example acceptance -p retouch-core`
use image::{Rgb, RgbImage};
use retouch_core::heal::heal_image;
use retouch_core::spot::{HealMode, SpotFix};

fn main() {
    let out_dir = "/tmp/retouch_acceptance";
    std::fs::create_dir_all(out_dir).unwrap();

    let (w, h) = (512u32, 512u32);
    // 确定性纹理背景（clean，无线）：低频渐变 + 轻噪声，模拟真实照片纹理
    // （非纯色，才能体现 PatchMatch「抄邻近纹理」比 Telea 扩散更自然的优势）。
    let mut clean = RgbImage::new(w, h);
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let rnd = |s: &mut u64| -> u32 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        (*s >> 32) as u32
    };
    for y in 0..h {
        for x in 0..w {
            let n = (rnd(&mut seed) % 48) as i32 - 24; // ±24 噪声
            let grad = 90 + ((x + y * 2) / 6) as i32 % 110; // 低频渐变 90..200
            let v = (grad + n).clamp(0, 255) as u8;
            clean.put_pixel(
                x,
                y,
                Rgb([v, (v as i32 * 92 / 100) as u8, (v as i32 * 80 / 100) as u8]),
            );
        }
    }

    // 画一条斜的深色电线 -> dirty
    let mut dirty = clean.clone();
    let mut wire = vec![false; (w * h) as usize];
    let lw = 2i32; // 线半径（~2px 宽）
    for t in 0..w as i32 {
        let cx = t;
        let cy = (t as f32 * 0.65 + 30.0) as i32;
        for dy in -lw..=lw {
            for dx in -lw..=lw {
                if dx * dx + dy * dy <= lw * lw {
                    let px = cx + dx;
                    let py = cy + dy;
                    if px >= 0 && px < w as i32 && py >= 0 && py < h as i32 {
                        dirty.put_pixel(px as u32, py as u32, Rgb([18u8, 18, 18]));
                        wire[(py as u32 * w + px as u32) as usize] = true;
                    }
                }
            }
        }
    }
    dirty.save(format!("{}/wire_dirty.png", out_dir)).unwrap();
    clean.save(format!("{}/wire_clean.png", out_dir)).unwrap();

    // 构造覆盖整条电线的多笔刷（沿电线每隔 ~9px 一笔，半径占短边 2%）。
    let mut spot = SpotFix::new();
    for t in (0..w as i32).step_by(9) {
        let cx = t as f32 / w as f32;
        let cy = (t as f32 * 0.65 + 30.0) / h as f32;
        spot.add_stroke(cx, cy, 0.02);
    }

    // baseline：未修复（dirty）vs clean 在电线区 PSNR（应很低，说明线明显）
    let dirty_psnr = region_psnr(&dirty, &clean, &wire);
    println!(
        "[baseline] dirty 电线区 PSNR = {:.2} dB（线未修，越低说明线越明显）",
        dirty_psnr
    );

    // 四档各跑 + 量化
    for mode in [
        HealMode::Telea,
        HealMode::FreqSep,
        HealMode::Poisson,
        HealMode::PatchMatch,
    ] {
        spot.mode = mode;
        let out = heal_image(&dirty, &spot, true); // preview=true，与交互预览一致
        let tag = format!("{:?}", mode).to_lowercase();
        out.save(format!("{}/wire_{}.png", out_dir, tag)).unwrap();
        let wire_psnr = region_psnr(&out, &clean, &wire);
        let bg_max = bg_max_diff(&out, &clean, &wire);
        println!(
            "[{:?}] 电线区 PSNR = {:.2} dB | 背景最大改动 = {}（应≈0，证明不破坏原图）",
            mode, wire_psnr, bg_max
        );
    }
    println!("PNG 已写出到 {}", out_dir);
}

/// 在 mask 区域内，out vs truth 的 PSNR（dB）。越高 = 越接近真值（线被修复成背景）。
fn region_psnr(out: &RgbImage, truth: &RgbImage, mask: &[bool]) -> f32 {
    let (w, h) = out.dimensions();
    let mut sse = 0.0f64;
    let mut n = 0u64;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if mask[i] {
                let p = out.get_pixel(x, y);
                let t = truth.get_pixel(x, y);
                for c in 0..3 {
                    let d = p.0[c] as f64 - t.0[c] as f64;
                    sse += d * d;
                }
                n += 1;
            }
        }
    }
    if n == 0 || sse == 0.0 {
        return f32::INFINITY;
    }
    let mse = sse / (n * 3) as f64;
    (10.0 * (255.0_f64 * 255.0_f64 / mse).log10()) as f32
}

/// 非 mask 区域（背景）out vs truth 的最大通道差。应≈0，证明只改了线、没动背景。
fn bg_max_diff(out: &RgbImage, truth: &RgbImage, mask: &[bool]) -> u32 {
    let (w, h) = out.dimensions();
    let mut mx = 0u32;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if !mask[i] {
                let p = out.get_pixel(x, y);
                let t = truth.get_pixel(x, y);
                for c in 0..3 {
                    let d = (p.0[c] as i32 - t.0[c] as i32).unsigned_abs();
                    if d > mx {
                        mx = d;
                    }
                }
            }
        }
    }
    mx
}
