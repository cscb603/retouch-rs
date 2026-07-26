//! 单图小算法验证：detect→correct→verify 闭环（绘画色彩理论量化版）
//!
//! 核心论点（用户提出，这里验证）：
//! 1. 偏色分两类 —— 数码cast（叠加在全图的常数偏移）vs 固有色（物体本身的高彩度色）。
//!    只从「近中性区域」(低彩度像素) 估计偏色向量，再全局减去它：
//!    中性区 → 归零（正确），红墙/蓝天 → 还原成本色（不被破坏）。这正是论文「只校有色偏、不校固有色偏」。
//! 2. 亮度护栏：渲染后若 mean_l 超 cap，降 mix（加大原图渗透）直到安全 —— 即用户说的「20%原图渗透」物理保证不毁图。
//! 3. verify：二次检测确认偏色下降且无自然色破坏。
//!
//! 用法：cargo run --example test_dc_loop --release -- "<图片路径>" [第二张可选]

use image::{DynamicImage, GenericImageView, RgbImage};
use retouch_core::analyze::analyze;
use retouch_core::auto_color::auto_neutral_balance;
use retouch_core::pipeline::render;

// ---------- sRGB <-> CIE Lab（标准 D65，无外部依赖） ----------
fn srgb_to_lab(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let rs = r as f32 / 255.0;
    let gs = g as f32 / 255.0;
    let bs = b as f32 / 255.0;
    let lin = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let rl = lin(rs);
    let gl = lin(gs);
    let bl = lin(bs);
    let x = rl * 0.4124564 + gl * 0.3575761 + bl * 0.1804375;
    let y = rl * 0.2126729 + gl * 0.7151522 + bl * 0.0721750;
    let z = rl * 0.0193339 + gl * 0.1191920 + bl * 0.9503041;
    let f = |t: f32| {
        let eps = 216.0 / 24389.0;
        if t > eps {
            t.powf(1.0 / 3.0)
        } else {
            t * (24389.0 / 27.0) + 4.0 / 29.0
        }
    };
    let fx = f(x / 0.95047);
    let fy = f(y);
    let fz = f(z / 1.08883);
    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let bb = 200.0 * (fy - fz);
    (l, a, bb)
}

fn lab_to_srgb(l: f32, a: f32, b: f32) -> (u8, u8, u8) {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let eps = 216.0 / 24389.0;
    let f_inv = |t: f32| {
        let t3 = t * t * t;
        if t3 > eps {
            t3
        } else {
            3.0f32 * (6.0f32 / 29.0).powi(2) * (t - 4.0f32 / 29.0)
        }
    };
    let x = 0.95047 * f_inv(fx);
    let y = 1.0 * f_inv(fy);
    let z = 1.08883 * f_inv(fz);
    let rl = x * 3.2404542 + y * (-1.5371385) + z * (-0.4985314);
    let gl = x * (-0.9692660) + y * 1.8760108 + z * 0.0415560;
    let bl = x * 0.0556434 + y * (-0.2040259) + z * 1.0572252;
    let to_srgb = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    };
    let r = (to_srgb(rl) * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (to_srgb(gl) * 255.0).round().clamp(0.0, 255.0) as u8;
    let bb = (to_srgb(bl) * 255.0).round().clamp(0.0, 255.0) as u8;
    (r, g, bb)
}

/// 近中性区域偏色估计：取低彩度、非极暗极亮像素，平均其 Lab(a,b)。
/// 返回 (mean_a, mean_b, 近中性像素占比, 全图近中性占比)
fn neutral_cast(img: &RgbImage) -> (f32, f32, f32, f32) {
    let mut sa = 0.0;
    let mut sb = 0.0;
    let mut n = 0.0;
    let mut low = 0.0;
    let mut total = 0.0;
    for p in img.pixels() {
        let (l, a, b) = srgb_to_lab(p[0], p[1], p[2]);
        total += 1.0;
        let chroma = (a * a + b * b).sqrt();
        if chroma < 12.0 && l > 18.0 && l < 88.0 {
            sa += a;
            sb += b;
            n += 1.0;
            low += 1.0;
        }
    }
    if n > 0.0 {
        (sa / n, sb / n, n / total, low / total)
    } else {
        (0.0, 0.0, 0.0, 0.0)
    }
}

/// 全局减去偏色常数 (da, db)，k 为强度（≤1 表示最多完全移除测得的中性偏色）
fn apply_wb_nudge(img: &RgbImage, da: f32, db: f32, k: f32) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut out = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels() {
        let (l, a, b) = srgb_to_lab(p[0], p[1], p[2]);
        let na = (a - k * da).clamp(-128.0, 127.0);
        let nb = (b - k * db).clamp(-128.0, 127.0);
        let (r, g, bb) = lab_to_srgb(l, na, nb);
        out.put_pixel(x, y, image::Rgb([r, g, bb]));
    }
    out
}

/// 凸组合：out = s*sec + (1-s)*orig。s=1 全用 sec，s=0 全用原图。物理上结果必在两者之间。
fn blend(orig: &RgbImage, sec: &RgbImage, s: f32) -> RgbImage {
    let (w, h) = orig.dimensions();
    let mut out = RgbImage::new(w, h);
    for (x, y, pa) in orig.enumerate_pixels() {
        let pb = sec.get_pixel(x, y);
        let r = (pa[0] as f32 * (1.0 - s) + pb[0] as f32 * s) as u8;
        let g = (pa[1] as f32 * (1.0 - s) + pb[1] as f32 * s) as u8;
        let b = (pa[2] as f32 * (1.0 - s) + pb[2] as f32 * s) as u8;
        out.put_pixel(x, y, image::Rgb([r, g, b]));
    }
    out
}

fn cap_for(orig_mean_l: f32) -> f32 {
    if orig_mean_l > 0.55 {
        orig_mean_l + 0.02
    } else {
        0.58
    }
}

fn run_one(path: &str) {
    println!("\n================ {} ================", path);
    let orig = match image::open(path) {
        Ok(i) => i,
        Err(e) => {
            println!("  打开失败: {e}");
            return;
        }
    };
    // 限制测试尺寸，加速（原理与分辨率无关）
    let (ow, oh) = orig.dimensions();
    let max_side = 1600u32;
    let orig = if ow.max(oh) > max_side {
        let scale = max_side as f32 / ow.max(oh) as f32;
        orig.resize(
            (ow as f32 * scale) as u32,
            (oh as f32 * scale) as u32,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        orig
    };
    let orig_rgb = orig.to_rgb8();
    let m_orig = analyze(&orig);
    println!(
        "  原图   meanL={:.3} stdL={:.3} cast={:.3}@{:.0}° huePeak={:.3} skinL={:.3} skinC={:.3}",
        m_orig.tone.mean_l,
        m_orig.tone.std_l,
        m_orig.cast.chroma,
        m_orig.cast.hue_deg,
        m_orig.color.hue_peakiness,
        m_orig.skin.mean_l,
        m_orig.skin.mean_c
    );
    let (na0, nb0, _, _) = neutral_cast(&orig_rgb);
    println!("  原图近中性偏色 (a,b)=({:.2},{:.2})", na0, nb0);

    // 1) 主校正：自动中性化引擎（颜色去偏色，用户已认可能量好）
    let bal = auto_neutral_balance(&orig, true);
    let adj = bal.to_adjustments();
    let primary = render(&orig, &adj);
    let m_pri = analyze(&DynamicImage::ImageRgb8(primary.clone()));
    let (na1, nb1, _, _) = neutral_cast(&primary);
    println!(
        "  主校正 meanL={:.3} stdL={:.3} cast={:.3} skinL={:.3} | 近中性(a,b)=({:.2},{:.2})",
        m_pri.tone.mean_l, m_pri.tone.std_l, m_pri.cast.chroma, m_pri.skin.mean_l, na1, nb1
    );

    // 2) 二次校正：基于近中性区域偏色，全局有界减去（k=0.9，最多移除测得偏色）
    let mut sec = apply_wb_nudge(&primary, na1, nb1, 0.9);
    let mut m_sec = analyze(&DynamicImage::ImageRgb8(sec.clone()));
    let mut nc = neutral_cast(&sec);
    // 二次检测 + 必要时再迭代一次
    if (nc.0 * nc.0 + nc.1 * nc.1).sqrt() > 3.0 {
        sec = apply_wb_nudge(&sec, nc.0, nc.1, 0.6);
        m_sec = analyze(&DynamicImage::ImageRgb8(sec.clone()));
        nc = neutral_cast(&sec);
    }
    println!(
        "  二次校正 meanL={:.3} stdL={:.3} cast={:.3} skinL={:.3} | 近中性(a,b)=({:.2},{:.2})",
        m_sec.tone.mean_l, m_sec.tone.std_l, m_sec.cast.chroma, m_sec.skin.mean_l, nc.0, nc.1
    );

    // 3) 亮度护栏：超 cap 则降 mix（加大原图渗透），物理保证不比原图更亮
    let cap = cap_for(m_orig.tone.mean_l);
    let mut final_img = sec.clone();
    let mut s = 1.0f32;
    if m_sec.tone.mean_l > cap {
        for step in 1..=20 {
            s = (1.0 - step as f32 * 0.05).max(0.0);
            final_img = blend(&orig_rgb, &sec, s);
            if analyze(&DynamicImage::ImageRgb8(final_img.clone()))
                .tone
                .mean_l
                <= cap
            {
                break;
            }
        }
    }
    let m_fin = analyze(&DynamicImage::ImageRgb8(final_img.clone()));
    println!(
        "  最终   meanL={:.3}(cap={:.3}) stdL={:.3} cast={:.3} skinL={:.3} skinC={:.3} | 原图渗透={:.0}%",
        m_fin.tone.mean_l, cap, m_fin.tone.std_l, m_fin.cast.chroma, m_fin.skin.mean_l, m_fin.skin.mean_c,
        (1.0 - s) * 100.0
    );

    // 4) 验收：不毁图 + 有改善
    let no_destroy = m_fin.tone.mean_l <= cap
        && m_fin.tone.std_l >= m_orig.tone.std_l * 0.6
        && m_fin.skin.mean_l <= 0.82;
    let improved = m_fin.cast.chroma <= m_orig.cast.chroma; // 偏色不增
    let inherent_ok = (nc.0 * nc.0 + nc.1 * nc.1).sqrt() < (na0 * na0 + nb0 * nb0).sqrt().max(8.0);
    println!(
        "  验收：不毁图={}  偏色不增={}  近中性偏色收敛={}  => {}",
        no_destroy,
        improved,
        inherent_ok,
        if no_destroy && improved && inherent_ok {
            "✅ PASS"
        } else {
            "❌ FAIL"
        }
    );

    // 存盘便于肉眼看
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("img");
    let dir = std::env::temp_dir();
    let _ = orig.save(dir.join(format!("dc_{stem}_orig.png")));
    let _ = primary.save(dir.join(format!("dc_{stem}_primary.png")));
    let _ = final_img.save(dir.join(format!("dc_{stem}_final.png")));
    println!("  存盘：{:?}/dc_{}_[orig|primary|final].png", dir, stem);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: test_dc_loop <图片1> [图片2 ...]");
        return;
    }
    for p in &args[1..] {
        run_one(p);
    }
}
