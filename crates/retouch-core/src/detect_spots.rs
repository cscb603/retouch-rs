//! 自动污点检测（v0.6.6）：纯信号处理，零权重、可离线。
//!
//! 路线：中值滤波估计无结构背景 → 残差(最大通道 abs 差) → 阈值二值化 →
//! 连通域分析 → **孤立性校验** → 生成归一化笔触。
//!
//! ## 为什么必须做孤立性校验
//! 只看「残差大」会在纹理区（树叶、织物、砂石）疯狂误检，而误检的笔触会被
//! 真实修复、破坏画面——误检比漏检危险得多。真实传感器灰尘的本质特征是
//! **局部异常**：斑点本身残差高，但紧邻的一圈背景必须是平滑的。纹理区则是
//! 「到处都高」，据此可干净地区分。
//!
//! **不做什么**（见白皮书 C2/C3）：语义级瑕疵（人、电线杆、皮肤痘印）不做，
//! 不引入任何 ML/ONNX 依赖。电线是结构化线条易误伤，交由用户手动圈选。

use crate::spot::{HealMode, SpotStroke};
use image::RgbImage;
use rayon::prelude::*;

/// 自动检测参数（均可机读覆盖）。
#[derive(Clone, Debug)]
pub struct DetectParams {
    /// 中值滤波核大小（奇数，默认 13）。
    ///
    /// **这是最关键的旋钮**：中值滤波只能发现「小于半个核」的结构——直径 ≥ ksize
    /// 的斑点其中心窗口整个落在斑点内部，中值 = 斑点自身 → 残差归零 → 完全隐形。
    /// 实测（1400×933，18 个 r=1..6 已知灰尘）：
    ///   ksize=5 → 召回 6/18；ksize=9 → 18/18 但 18 处误检；**ksize=13 → 18/18 且零误检**。
    /// 得益于 Huang 直方图滑窗中值，耗时几乎与核大小无关（13 与 5 仅差 14ms）。
    pub median_ksize: u32,
    /// 残差阈值（0-255，超过即判定为异常点，默认 25）。
    pub contrast_thr: f32,
    /// 最小污点半径(px)，小于此视为噪点丢弃（默认 1.5，即 3×3 斑点可被检出）。
    pub min_radius_px: f32,
    /// 最大污点半径(px)，大于此视为大块物体（不自动选，默认 40）。
    pub max_radius_px: f32,
    /// 最小连通域面积(px²)，去孤立噪点（默认 4）。
    pub min_area: u32,
    /// 孤立性比率：候选点外围一圈的平均残差必须低于 `contrast_thr * 此值`，
    /// 否则判定为「处在纹理区」而拒绝（默认 0.35）。调大=更宽松更易误检。
    pub isolation_ratio: f32,
    /// 笔触半径外扩系数：修复需要采样周边，笔触略大于斑点本身（默认 1.4）。
    pub radius_scale: f32,
    /// 最多返回多少个笔触（按对比度从高到低取，默认 200），防极端图刷屏。
    pub max_spots: usize,
    /// 多尺度降采样因子（默认 `[1]`，即单尺度）。
    ///
    /// 曾用于覆盖超出中值核范围的大斑点，但实测证伪：多尺度会在粗尺度上把纹理
    /// 平滑成「孤立异常」而制造误检（ksize=5+[1,3,6] → 6 处误检），
    /// 而直接放大 ksize 配合 Huang 滑窗中值几乎不增加耗时且零误检。
    /// 保留此旋钮供极端场景（斑点直径远超 ksize）调优，默认关闭。
    pub scales: Vec<u32>,
}

impl Default for DetectParams {
    fn default() -> Self {
        Self {
            median_ksize: 13,
            contrast_thr: 25.0,
            // 最小半径 1.0px：2px 直径灰尘仍值得修；配合孤立性校验不会引入噪声误检。
            min_radius_px: 1.0,
            max_radius_px: 28.0,
            min_area: 4,
            isolation_ratio: 0.35,
            // 笔触外扩系数：修复需采样周边做羽化，轻微外扩即可（1.15）。
            // 旧默认 1.4 会把小污点撑成明显过大的修复圈，易穿帮。
            radius_scale: 1.15,
            max_spots: 200,
            scales: vec![1],
        }
    }
}

/// 内部候选记录。
struct Candidate {
    cx_px: f32,
    cy_px: f32,
    minx: usize,
    maxx: usize,
    miny: usize,
    maxy: usize,
    mean_res: f32,
    /// 连通域像素数（用于等效圆半径，比外接矩形半边长更紧凑，避免长条污点撑成大圈）。
    area: u32,
}

/// 自动检测孤立小污点（传感器灰尘 / 亮斑 / 暗点），返回归一化笔触列表。
/// 返回空 Vec 表示未检测到（或图为空）。
///
/// 多尺度：在 `params.scales` 的每个降采样档各跑一遍单尺度检测，再跨尺度去重。
pub fn detect_spots(img: &RgbImage, params: &DetectParams) -> Vec<SpotStroke> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Vec::new();
    }

    let scales = if params.scales.is_empty() {
        vec![1u32]
    } else {
        params.scales.clone()
    };

    // (mean_res, stroke) —— mean_res 用于去重时择优与最终截断排序
    let mut all: Vec<(f32, SpotStroke)> = Vec::new();
    for f in scales {
        if f == 0 {
            continue;
        }
        let (sw, sh) = (w / f, h / f);
        // 太小则该尺度无意义（中值核都放不下）
        if f > 1 && (sw < params.median_ksize * 4 || sh < params.median_ksize * 4) {
            continue;
        }
        let mut p = params.clone();
        // 该尺度下的最大半径要换算回「原图 px」语义，避免大尺度把巨块也收进来
        p.max_radius_px = params.max_radius_px / f as f32;
        if f == 1 {
            all.extend(detect_single_scale(img, &p));
        } else {
            let small = downsample_box(img, f);
            all.extend(detect_single_scale(&small, &p));
        }
    }

    // 跨尺度去重：半径大者优先，中心落在已保留笔触内即视为同一处
    all.sort_by(|a, b| {
        b.1.r_norm
            .partial_cmp(&a.1.r_norm)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<(f32, SpotStroke)> = Vec::new();
    for (res, s) in all {
        let dup = kept.iter().any(|(_, k)| {
            let dx = k.cx - s.cx;
            let dy = k.cy - s.cy;
            (dx * dx + dy * dy).sqrt() <= k.r_norm.max(s.r_norm)
        });
        if !dup {
            kept.push((res, s));
        }
    }

    // 按对比度降序截断，避免极端图刷出上千笔触
    kept.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    kept.truncate(params.max_spots);
    kept.into_iter().map(|(_, s)| s).collect()
}

/// 整数倍盒式降采样（用于多尺度）。
fn downsample_box(img: &RgbImage, f: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let (nw, nh) = ((w / f).max(1), (h / f).max(1));
    let src = img.as_raw();
    let wu = w as usize;
    let fu = f as usize;
    let mut out = RgbImage::new(nw, nh);
    let dst = out.as_mut();
    dst.par_chunks_mut(nw as usize * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..nw as usize {
                let (mut sr, mut sg, mut sb, mut cnt) = (0u32, 0u32, 0u32, 0u32);
                for dy in 0..fu {
                    let sy = y * fu + dy;
                    if sy >= h as usize {
                        break;
                    }
                    for dx in 0..fu {
                        let sx = x * fu + dx;
                        if sx >= wu {
                            break;
                        }
                        let o = (sy * wu + sx) * 3;
                        sr += src[o] as u32;
                        sg += src[o + 1] as u32;
                        sb += src[o + 2] as u32;
                        cnt += 1;
                    }
                }
                let c = cnt.max(1);
                row[x * 3] = (sr / c) as u8;
                row[x * 3 + 1] = (sg / c) as u8;
                row[x * 3 + 2] = (sb / c) as u8;
            }
        });
    out
}

/// 单尺度检测核心，返回 (平均残差, 归一化笔触)。
fn detect_single_scale(img: &RgbImage, params: &DetectParams) -> Vec<(f32, SpotStroke)> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let wu = w as usize;
    let hu = h as usize;
    let n = wu * hu;

    // 1. 中值滤波估计无结构背景（各通道独立）
    let med = median_filter_rgb(img, params.median_ksize);

    // 2. 残差：最大通道 abs 差（突出异色孤立点）
    let src = img.as_raw();
    let mut residual = vec![0f32; n];
    residual
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, r)| {
            let p = &src[i * 3..i * 3 + 3];
            let m = med[i];
            *r = (p[0] as f32 - m[0] as f32)
                .abs()
                .max((p[1] as f32 - m[1] as f32).abs())
                .max((p[2] as f32 - m[2] as f32).abs());
        });

    // 3. 二值化
    let bin: Vec<bool> = residual.iter().map(|&r| r > params.contrast_thr).collect();

    // 4. 连通域（8 连通）分析 → 候选
    let mut visited = vec![false; n];
    let dx8 = [-1i32, -1, -1, 0, 0, 1, 1, 1];
    let dy8 = [-1i32, 0, 1, -1, 1, -1, 0, 1];
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..n {
        if !bin[start] || visited[start] {
            continue;
        }
        stack.clear();
        stack.push(start);
        visited[start] = true;
        let mut sx = 0u64;
        let mut sy = 0u64;
        let mut cnt = 0u32;
        let mut minx = wu;
        let mut miny = hu;
        let mut maxx = 0usize;
        let mut maxy = 0usize;
        let mut sres = 0f32;
        while let Some(px) = stack.pop() {
            let x = px % wu;
            let y = px / wu;
            sx += x as u64;
            sy += y as u64;
            cnt += 1;
            sres += residual[px];
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
            for k in 0..8 {
                let nx = x as i32 + dx8[k];
                let ny = y as i32 + dy8[k];
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let ni = (ny as usize) * wu + (nx as usize);
                if bin[ni] && !visited[ni] {
                    visited[ni] = true;
                    stack.push(ni);
                }
            }
        }
        if cnt >= params.min_area {
            candidates.push(Candidate {
                cx_px: sx as f32 / cnt as f32,
                cy_px: sy as f32 / cnt as f32,
                minx,
                maxx,
                miny,
                maxy,
                mean_res: sres / cnt as f32,
                area: cnt,
            });
        }
    }

    // 5. 过滤：半径区间 + 高对比 + 孤立性校验
    let short = w.min(h) as f32;
    let r_min_norm = params.min_radius_px / short;
    let r_max_norm = params.max_radius_px / short;
    let ring_limit = params.contrast_thr * params.isolation_ratio;

    let mut kept: Vec<(f32, SpotStroke)> = Vec::new();
    for c in &candidates {
        // 用等效圆半径 sqrt(area/π)，比「外接矩形半边长」紧凑得多：
        // 长条/椭圆污点不再被拉成大圆，避免修复圈过大穿帮。
        let radius_px = ((c.area as f32 / std::f32::consts::PI).sqrt()).max(1.0);
        let r_norm = radius_px / short;
        if r_norm < r_min_norm || r_norm > r_max_norm {
            continue;
        }
        if c.mean_res <= params.contrast_thr {
            continue;
        }
        // 孤立性：外扩一圈（至少 3px，或斑点半径的 1 倍），环内平均残差须足够低
        let pad = (radius_px.round() as usize).max(3);
        if !is_isolated(&residual, wu, hu, c, pad, ring_limit) {
            continue;
        }
        kept.push((
            c.mean_res,
            SpotStroke::new(
                c.cx_px / w as f32,
                c.cy_px / h as f32,
                r_norm * params.radius_scale,
                HealMode::Poisson,
            ),
        ));
    }
    kept
}

/// 孤立性校验：候选包围盒外扩 `pad` 形成的「环带」内，平均残差必须 ≤ `limit`。
/// 环带排除包围盒本身（斑点内部当然高残差）。
fn is_isolated(
    residual: &[f32],
    wu: usize,
    hu: usize,
    c: &Candidate,
    pad: usize,
    limit: f32,
) -> bool {
    let ox0 = c.minx.saturating_sub(pad);
    let oy0 = c.miny.saturating_sub(pad);
    let ox1 = (c.maxx + pad).min(wu - 1);
    let oy1 = (c.maxy + pad).min(hu - 1);
    let mut sum = 0f32;
    let mut cnt = 0u32;
    for y in oy0..=oy1 {
        for x in ox0..=ox1 {
            // 跳过包围盒内部
            if x >= c.minx && x <= c.maxx && y >= c.miny && y <= c.maxy {
                continue;
            }
            sum += residual[y * wu + x];
            cnt += 1;
        }
    }
    if cnt == 0 {
        return false;
    }
    (sum / cnt as f32) <= limit
}

/// 便捷入口：直接接收 RGB 字节（3 通道）+ 尺寸，无需调用方依赖 `image` crate。
/// 字节数与 `w*h*3` 不符时返回空（安全失败，不 panic）。
pub fn detect_spots_from_rgb(rgb: &[u8], w: u32, h: u32, params: &DetectParams) -> Vec<SpotStroke> {
    if rgb.len() != (w as usize) * (h as usize) * 3 {
        return Vec::new();
    }
    match RgbImage::from_raw(w, h, rgb.to_vec()) {
        Some(img) => detect_spots(&img, params),
        None => Vec::new(),
    }
}

/// 各通道独立中值滤波（边界 clamp），**Huang 直方图滑窗算法**。
///
/// 朴素实现每像素要排序 k² 个样本（k=13 时 169 个 × 3 通道），耗时随核大小
/// 平方级暴涨——实测 1400×933 上 ksize=13 需 394ms，超出 UI 同步执行预算。
/// 本实现按行滑动维护 256 bin 直方图：窗口右移一格只需「移除左列、加入右列」
/// 共 2k 个样本，中值位置增量修正（相邻像素中值变化很小，O(1) 摊还），
/// 使耗时几乎与核大小无关。
///
/// 边界 clamp 下滑窗仍严格成立：
/// `window(x+1) = window(x) − col[clamp(x−r)] + col[clamp(x+r+1)]`。
fn median_filter_rgb(img: &RgbImage, ksize: u32) -> Vec<[u8; 3]> {
    let (w, h) = img.dimensions();
    let wu = w as usize;
    let hu = h as usize;
    let r = (ksize / 2) as i32;
    let src = img.as_raw();
    let win = ((2 * r + 1) * (2 * r + 1)) as u32;
    let th = win / 2; // 目标为第 th 位序统计量（0-based）
    let mut out = vec![[0u8; 3]; wu * hu];

    out.par_chunks_mut(wu).enumerate().for_each(|(y, row)| {
        // 预先算好本行窗口覆盖的行索引（clamp 后），避免内层重复 clamp
        let ys: Vec<usize> = (-r..=r)
            .map(|dy| (y as i32 + dy).clamp(0, hu as i32 - 1) as usize)
            .collect();

        for ch in 0..3 {
            let mut hist = [0u32; 256];
            // 初始化 x=0 的窗口
            for dx in -r..=r {
                let cx = dx.clamp(0, wu as i32 - 1) as usize;
                for &cy in &ys {
                    hist[src[(cy * wu + cx) * 3 + ch] as usize] += 1;
                }
            }
            // 定位初始中值
            let mut acc = 0u32;
            let mut mdn = 0usize;
            for (v, &c) in hist.iter().enumerate() {
                if acc + c > th {
                    mdn = v;
                    break;
                }
                acc += c;
            }
            let mut ltmdn = acc; // 严格小于 mdn 的样本数
            row[0][ch] = mdn as u8;

            // 滑窗必须按 x 严格顺序推进（直方图是增量维护的），不能改成迭代器
            #[allow(clippy::needless_range_loop)]
            for x in 1..wu {
                let lx = ((x as i32 - 1) - r).clamp(0, wu as i32 - 1) as usize;
                let rx = (x as i32 + r).clamp(0, wu as i32 - 1) as usize;
                for &cy in &ys {
                    let base = cy * wu;
                    let vo = src[(base + lx) * 3 + ch] as usize;
                    hist[vo] -= 1;
                    if vo < mdn {
                        ltmdn -= 1;
                    }
                    let vn = src[(base + rx) * 3 + ch] as usize;
                    hist[vn] += 1;
                    if vn < mdn {
                        ltmdn += 1;
                    }
                }
                // 增量修正中值位置：保持 ltmdn <= th < ltmdn + hist[mdn]
                while ltmdn > th {
                    mdn -= 1;
                    ltmdn -= hist[mdn];
                }
                while ltmdn + hist[mdn] <= th {
                    ltmdn += hist[mdn];
                    mdn += 1;
                }
                row[x][ch] = mdn as u8;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn detects_isolated_dust_spots() {
        // 灰底 + 几个孤立黑点（传感器灰尘），应被检出。
        let mut img = RgbImage::from_pixel(128, 128, Rgb([180u8, 178, 176]));
        let dust = [(20i32, 30i32), (80, 60), (100, 110)];
        for (x, y) in dust {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    img.put_pixel((x + dx) as u32, (y + dy) as u32, Rgb([8u8, 8, 8]));
                }
            }
        }
        let spots = detect_spots(&img, &DetectParams::default());
        assert!(
            !spots.is_empty(),
            "应检出至少一个灰尘点，实际 {}",
            spots.len()
        );
        assert!(
            spots.len() <= dust.len(),
            "不应超过实际灰尘数，实际 {}",
            spots.len()
        );
        for s in &spots {
            assert!(s.cx >= 0.0 && s.cx <= 1.0 && s.cy >= 0.0 && s.cy <= 1.0);
            assert!(s.r_norm > 0.0 && s.r_norm < 0.2);
        }
    }

    #[test]
    fn detects_full_size_range_without_false_positives() {
        // 回归护栏：默认参数必须覆盖 r=1..6（直径 3~13px）全档灰尘且零误检。
        // 若有人把 median_ksize 调小，大斑点会静默隐形——此测试即为拦截该退化。
        let (w, h) = (600u32, 400u32);
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 150.0 + (y as f32 / h as f32) * 60.0;
                img.put_pixel(x, y, Rgb([v as u8, (v + 6.0) as u8, (v + 18.0) as u8]));
            }
        }
        let mut truth = Vec::new();
        for (i, r) in [1i32, 2, 3, 4, 5, 6].iter().enumerate() {
            let cx = w * (i as u32 + 1) / 7;
            let cy = h / 2;
            for dy in -r..=*r {
                for dx in -r..=*r {
                    if dx * dx + dy * dy <= r * r {
                        img.put_pixel(
                            (cx as i32 + dx) as u32,
                            (cy as i32 + dy) as u32,
                            Rgb([30u8, 30, 32]),
                        );
                    }
                }
            }
            truth.push((cx as f32, cy as f32, *r as f32));
        }

        let spots = detect_spots(&img, &DetectParams::default());
        let mut hit = 0;
        for (tx, ty, tr) in &truth {
            if spots.iter().any(|s| {
                let dx = s.cx * w as f32 - tx;
                let dy = s.cy * h as f32 - ty;
                (dx * dx + dy * dy).sqrt() <= tr + 5.0
            }) {
                hit += 1;
            }
        }
        assert_eq!(hit, truth.len(), "应检出全部 6 档灰尘，实际 {}", hit);
        assert!(
            spots.len() <= truth.len(),
            "不应产生误检，检出 {} > 真值 {}",
            spots.len(),
            truth.len()
        );
    }

    #[test]
    fn does_not_flag_clean_sky() {
        // 纯色渐变图（无孤立点），不应误检。
        let mut img = RgbImage::new(128, 128);
        for y in 0..128u32 {
            for x in 0..128u32 {
                let v = (y as f32 / 127.0 * 255.0) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let spots = detect_spots(&img, &DetectParams::default());
        assert!(spots.is_empty(), "纯色渐变不应误检，实际 {}", spots.len());
    }

    #[test]
    fn does_not_flood_on_texture() {
        // 高频噪声纹理（模拟树叶/织物）：孤立性校验应拒绝绝大多数候选。
        // 误检会被真实修复、破坏画面，因此这里是硬约束。
        let mut img = RgbImage::new(128, 128);
        let mut seed = 0x12345678u32;
        for y in 0..128u32 {
            for x in 0..128u32 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let v = (seed >> 24) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let spots = detect_spots(&img, &DetectParams::default());
        assert!(
            spots.len() <= 5,
            "纹理区不应刷出大量误检，实际 {}",
            spots.len()
        );
    }

    /// 朴素中值（参照实现），仅用于验证 Huang 滑窗算法的正确性。
    fn median_naive(img: &RgbImage, ksize: u32) -> Vec<[u8; 3]> {
        let (w, h) = img.dimensions();
        let (wu, hu) = (w as usize, h as usize);
        let k = (ksize / 2) as i32;
        let mut out = vec![[0u8; 3]; wu * hu];
        for y in 0..hu {
            for x in 0..wu {
                let mut c: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                for dy in -k..=k {
                    for dx in -k..=k {
                        let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as u32;
                        let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as u32;
                        let p = img.get_pixel(nx, ny);
                        for (ci, item) in c.iter_mut().enumerate() {
                            item.push(p[ci]);
                        }
                    }
                }
                let m = c[0].len() / 2;
                for item in c.iter_mut() {
                    item.sort_unstable();
                }
                out[y * wu + x] = [c[0][m], c[1][m], c[2][m]];
            }
        }
        out
    }

    #[test]
    fn huang_median_matches_naive() {
        // Huang 滑窗中值必须与朴素排序中值逐像素完全一致——
        // 否则就是引入了静默的数值错误。覆盖多种核大小与含边界的小图。
        let mut img = RgbImage::new(37, 23);
        let mut seed = 0xDEADBEEFu32;
        for y in 0..23u32 {
            for x in 0..37u32 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                img.put_pixel(
                    x,
                    y,
                    Rgb([
                        (seed >> 24) as u8,
                        (seed >> 16) as u8,
                        (seed >> 8) as u8,
                    ]),
                );
            }
        }
        for ks in [3u32, 5, 9, 13] {
            let fast = median_filter_rgb(&img, ks);
            let slow = median_naive(&img, ks);
            assert_eq!(fast.len(), slow.len());
            for (i, (f, s)) in fast.iter().zip(slow.iter()).enumerate() {
                assert_eq!(
                    f,
                    s,
                    "ksize={} 第 {} 像素不一致：Huang={:?} 朴素={:?}",
                    ks, i, f, s
                );
            }
        }
    }

    #[test]
    fn rejects_mismatched_buffer_length() {
        // 字节数不符应安全返回空，不 panic。
        let buf = vec![0u8; 10];
        let spots = detect_spots_from_rgb(&buf, 64, 64, &DetectParams::default());
        assert!(spots.is_empty());
    }
}
