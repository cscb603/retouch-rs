//! 研究用探针（非实现）：验证 safe_neutral 护栏是否
//! (a) 在普通图上对「自动中性化」无影响，(b) 在亮图上收住过曝。
//! 用缩略图（亮度机制与分辨率无关），仅做设计验证。

use image::{DynamicImage, GenericImageView};
use retouch_core::analyze::{analyze, ImageMetrics};
use retouch_core::auto_color::auto_neutral_balance;
use retouch_core::pipeline::{render, Adjustments};

fn metrics_of(img: &DynamicImage, adj: &Adjustments) -> ImageMetrics {
    analyze(&DynamicImage::ImageRgb8(render(img, adj)))
}

/// 复刻设计中的 safe_neutral 护栏逻辑（仅 mix 扫降，用于验证设计可行性）。
fn safe_wrap(img: &DynamicImage, base: &ImageMetrics, mut adj: Adjustments) -> Adjustments {
    let cap = if base.tone.mean_l > 0.55 {
        base.tone.mean_l + 0.02
    } else {
        0.58
    };
    let std_floor = base.tone.std_l * 0.65;
    let skin_cap = 0.80;
    let m = metrics_of(img, &adj);
    let violates = m.tone.mean_l > cap || m.skin.mean_l > skin_cap || m.tone.std_l < std_floor;
    if !violates {
        return adj; // 普通图：护栏无操作，自动中性化原样
    }
    for mix in [0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0] {
        adj.mix = mix;
        let m = metrics_of(img, &adj);
        if m.tone.mean_l <= cap && m.skin.mean_l <= skin_cap && m.tone.std_l >= std_floor {
            return adj;
        }
    }
    adj.mix = 0.0; // 极端兜底：回到原图
    adj
}

fn main() {
    let real = [
        "/Users/xtap/Desktop/课件/街头人物篇/12月 逆光夕阳 情侣.jpg",
        "/Users/xtap/Desktop/课件/街头人物篇/3月 老报馆 夜景 _IMG_7112 拷贝.jpg",
        "/Users/xtap/Desktop/课件/街头人物篇/9月 罍街 玉兔小姐姐 (1).jpg",
        "/Users/xtap/Desktop/课件/街头人物篇/8月 国购 情侣 夜景_IMG_0147.JPG",
        "/Users/xtap/Desktop/课件/街头人物篇/合柴 逆光 剪影 路人甲IMG_0222.jpg",
        "/Users/xtap/Desktop/课件/街头人物篇/玫瑰花墙 洛洛 IMG_6973.JPG",
        "/Users/xtap/Desktop/课件/街头人物篇/旗袍 路人 IMG_3800.JPG",
        "/Users/xtap/Desktop/课件/街头人物篇/成都 街头IMG_5795.jpg",
    ];

    println!(
        "{:<40} | {:>8} {:>8} {:>8} | {:>8} {:>8} {:>8} | {:>6} {:>6} {:>6}",
        "图像", "原L", "裸L", "护L", "原std", "裸std", "护std", "原脸", "裸脸", "护脸"
    );
    println!("{}", "-".repeat(118));

    for p in real {
        let full = match image::open(p) {
            Ok(i) => i,
            Err(_) => {
                println!("{:<40} | 打开失败", p);
                continue;
            }
        };
        // 缩略图加速
        let (w, h) = full.dimensions();
        let scale = (w.max(h) as f32 / 1000.0).max(1.0);
        let tw = (w as f32 / scale) as u32;
        let th = (h as f32 / scale) as u32;
        let img = full.resize(tw, th, image::imageops::FilterType::Lanczos3);

        let base = analyze(&img);
        let raw = auto_neutral_balance(&img, true).to_adjustments();
        let raw_m = metrics_of(&img, &raw);
        let safe = safe_wrap(
            &img,
            &base,
            auto_neutral_balance(&img, true).to_adjustments(),
        );
        let safe_m = metrics_of(&img, &safe);

        let name = p.rsplit('/').next().unwrap_or(p);
        println!(
            "{:<40} | {:>6.3} {:>6.3} {:>6.3} | {:>6.3} {:>6.3} {:>6.3} | {:>6.3} {:>6.3} {:>6.3}",
            name,
            base.tone.mean_l,
            raw_m.tone.mean_l,
            safe_m.tone.mean_l,
            base.tone.std_l,
            raw_m.tone.std_l,
            safe_m.tone.std_l,
            base.skin.mean_l,
            raw_m.skin.mean_l,
            safe_m.skin.mean_l,
        );
        let raw_over = raw_m.tone.mean_l > 0.62;
        let safe_over = safe_m.tone.mean_l > 0.62;
        let tag = match (raw_over, safe_over) {
            (true, true) => "  ❌裸过曝&护栏也没压住",
            (true, false) => "  ✅裸过曝→护栏已收住",
            (false, false) => {
                if (safe_m.tone.mean_l - raw_m.tone.mean_l).abs() < 0.01 {
                    "  =护栏无影响(好功能不变)"
                } else {
                    "  ~护栏微调"
                }
            }
            (false, true) => "  ⚠裸OK但护栏误伤",
        };
        println!(
            "    {:>8.3}->{:>8.3} (cap={:.3}){}",
            raw_m.tone.mean_l,
            safe_m.tone.mean_l,
            if base.tone.mean_l > 0.55 {
                base.tone.mean_l + 0.02
            } else {
                0.58
            },
            tag
        );
    }
}
