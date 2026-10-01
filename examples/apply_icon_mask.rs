//! 给 `assets/icon.png` 应用 Apple 标准平滑圆角 mask,覆盖写回。
//!
//! 用法:`cargo run --example apply_icon_mask`
//!
//! 为什么要这一步:Windows / Linux 平台**不会**自动给应用图标套 mask,
//! 所以源图必须自带圆角;macOS Dock/Finder 直接显示自带形状,与系统图标
//! 一致时肉眼无「双重圆角」现象。
//!
//! 形状是 Apple 图标底形(macOS Big Sur 以来系统图标的轮廓):平滑圆角
//! 矩形——直边 → 三次贝塞尔 → 圆弧 → 三次贝塞尔拼接,曲率连续。构造同
//! Figma「corner smoothing」(figma.com/blog/desperately-seeking-squircles;
//! 实现按 MartinRGB/Figma_Squircles_Approximation 与 figma-squircle 移植)。
//! 参数按本机 macOS 系统图标实测(遮罩 412px@512 亚像素边界最小二乘):
//! 圆角半径 = 边长 × 0.225,平滑系数 ξ = 0.6,残差 rms ≈ 0.3px@512。
//! 旧版用超椭圆 n≈5 近似——无平直边、角部偏尖,与系统图标并排能看出
//! 差别,故弃用。
//!
//! 抗锯齿:边界附近像素 4×4 超采样算覆盖率(16 级 alpha),远离边界直接
//! 0/255,无渐变近似误差。
//!
//! 跟 `gen_icon.rs` 同一套「`assets/icon.png` 是源真」约定:
//! ① 用 Agnes / 其他工具生成新的方版 icon.png →
//! ② `cargo run --example apply_icon_mask` 应用圆角 →
//! ③ `cargo run --example gen_icon` 重生成 Windows ICO 与 macOS 版图标 →
//! ④ `pwsh scripts/regen-tray-icon.ps1` 重生成 32×32 托盘图标 →
//! ⑤ `cargo build --release`。

use std::path::Path;

use image::{ImageBuffer, Rgba, RgbaImage};

const RADIUS_F: f64 = 0.225; // 圆角半径 / 图形边长(Apple 图标实测,824 → 185.4)
const SMOOTHING: f64 = 0.6; // 平滑系数 ξ:0=普通圆角矩形,0.6≈Apple 图标形状
const SS: u32 = 4; // 边界像素的超采样倍数(每边)

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let src = Path::new(&manifest_dir).join("assets").join("icon.png");

    println!("[apply_icon_mask] 源图:{}", src.display());
    let img = image::open(&src).map_err(|e| format!("读 {} 失败:{}", src.display(), e))?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w != h {
        return Err(format!("icon.png 必须是正方形,当前 {}x{}", w, h).into());
    }
    println!(
        "[apply_icon_mask] 源图尺寸:{}x{},应用 Apple 平滑圆角 mask(图形边长 {},R={:.1}, ξ={})",
        w,
        h,
        w - 4,
        RADIUS_F * (w as f64 - 4.0),
        SMOOTHING
    );

    // 覆盖写回前不做本地备份——原方版靠 git 历史找回（icon.png 未提交前跑本工具
    // 的话请自行留底）。
    let masked = apply_apple_squircle(&rgba);
    masked.save(&src)?;
    println!("[apply_icon_mask] 已写回 {}", src.display());
    println!("[apply_icon_mask] 接下来:");
    println!("  cargo run --example gen_icon");
    println!("  pwsh scripts/regen-tray-icon.ps1");
    println!("  cargo build --release");
    Ok(())
}

/// Apple 平滑圆角的单角几何(figma-squircle 的 getPathParamsForCorner)。
///
/// 局部坐标系:角点为原点,x/y 沿两条边向图形内。圆角区曲线:
/// (p,0) —三次贝塞尔— (L+d, d) —圆弧— (d, d+L) —三次贝塞尔— (0, p)。
struct Corner {
    /// 圆角区沿边的总占用;超出它的行是平直边
    p: f64,
    /// 采样查找表:ly → lx(从圆角区起点到终点的边界曲线)
    lys: Vec<f64>,
    lxs: Vec<f64>,
    /// 边界曲线 |dlx/dly| 的最大值(圆弧 45°+18° 处最陡,≈2):
    /// 快速路径余量要按它放大,否则陡边处会把部分在外的像素判成整像素在内
    max_slope: f64,
}

impl Corner {
    fn new(half: f64, radius: f64, smoothing: f64) -> Self {
        // 空间不足时按 Figma 的做法回退:压低平滑使圆角区不越过中线
        let smoothing = smoothing.min(half / radius - 1.0);
        let p = (1.0 + smoothing) * radius;
        let arc_measure = 90.0 * (1.0 - smoothing); // 圆弧角度(ξ=0 时 90°)
        let arc_len = (arc_measure / 2.0).to_radians().sin() * radius * std::f64::consts::SQRT_2;
        let alpha = ((90.0 - arc_measure) / 2.0).to_radians();
        let p3p4 = radius * (alpha / 2.0).tan(); // 控制点 P3、P4 间距
        let beta = (45.0 * smoothing).to_radians();
        let c = p3p4 * beta.cos();
        let d = c * beta.tan();
        let b = (p - arc_len - c - d) / 3.0;
        let a = 2.0 * b;

        // 曲线两端点(局部系),从起点 (p,0) 采样到终点 (0,p)
        let start = (p, 0.0);
        let arc_s = (arc_len + d, d);
        let arc_e = (d, d + arc_len);

        let mut pts: Vec<(f64, f64)> = Vec::new();
        let n = 1000u32;
        // 贝塞尔 1:start → arc_s
        let b1 = [
            start,
            (p - a, 0.0),
            (p - a - b, 0.0),
            (p - a - b - c, d),
        ];
        // 贝塞尔 2:arc_e → (0, p)(圆弧自己从 arc_s 走到 arc_e)
        let b2 = [
            arc_e,
            (0.0, arc_e.1 + c),
            (0.0, arc_e.1 + b + c),
            (0.0, p),
        ];
        for i in 1..=n {
            pts.push(bezier3(&b1, i as f64 / n as f64));
        }
        // 圆弧:圆心在弧两端点中点的外侧(远离角点方向),从 arc_s 顺时针到 arc_e
        let mid = ((arc_s.0 + arc_e.0) / 2.0, (arc_s.1 + arc_e.1) / 2.0);
        let half_chord = ((arc_e.0 - arc_s.0).hypot(arc_e.1 - arc_s.1)) / 2.0;
        if half_chord > 1e-9 {
            // ξ=1 时弧长为 0,跳过
            let k = (radius * radius - half_chord * half_chord).max(0.0).sqrt();
            let center = (mid.0 + k / std::f64::consts::SQRT_2, mid.1 + k / std::f64::consts::SQRT_2);
            let a0 = (arc_s.1 - center.1).atan2(arc_s.0 - center.0);
            let a1 = (arc_e.1 - center.1).atan2(arc_e.0 - center.0);
            let mut a1 = a1;
            while a1 > a0 {
                a1 -= 2.0 * std::f64::consts::PI; // 凸向角点方向的短弧
            }
            for i in 1..=n {
                let ang = a0 + (a1 - a0) * i as f64 / n as f64;
                pts.push((center.0 + radius * ang.cos(), center.1 + radius * ang.sin()));
            }
        }
        for i in 1..=n {
            pts.push(bezier3(&b2, i as f64 / n as f64));
        }
        pts.sort_by(|u, v| u.1.partial_cmp(&v.1).unwrap());
        let max_slope = pts
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).abs() / (w[1].1 - w[0].1).max(1e-12))
            .fold(0.0_f64, f64::max);
        Corner {
            p,
            lys: pts.iter().map(|q| q.1).collect(),
            lxs: pts.iter().map(|q| q.0).collect(),
            max_slope,
        }
    }

    /// 圆角曲线在行 ly 处的 lx(查找表线性插值;越界取端点)。
    fn lx_at(&self, ly: f64) -> f64 {
        let i = self.lys.partition_point(|&v| v < ly);
        match i {
            0 => self.lxs[0],
            n if n >= self.lxs.len() => *self.lxs.last().unwrap(),
            i => {
                let t = (ly - self.lys[i - 1]) / (self.lys[i] - self.lys[i - 1]);
                self.lxs[i - 1] + t * (self.lxs[i] - self.lxs[i - 1])
            }
        }
    }
}

fn bezier3(pts: &[(f64, f64); 4], t: f64) -> (f64, f64) {
    let mt = 1.0 - t;
    let (c0, c1, c2, c3) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
    (
        c0 * pts[0].0 + c1 * pts[1].0 + c2 * pts[2].0 + c3 * pts[3].0,
        c0 * pts[0].1 + c1 * pts[1].1 + c2 * pts[2].1 + c3 * pts[3].1,
    )
}

/// 对 RGBA 图像应用 Apple 平滑圆角 mask(图形边缘留 2px 透明边距,
/// 避免边缘抗锯齿被裁)。
fn apply_apple_squircle(src: &RgbaImage) -> RgbaImage {
    let (w, h) = src.dimensions();
    let pad = 2.0;
    let edge = w as f64 - 2.0 * pad; // 图形边长
    let half = edge / 2.0;
    let corner = Corner::new(half, RADIUS_F * edge, SMOOTHING);
    let cx = w as f64 / 2.0;

    // half_width(dy):行 dy(相对中心)的图形半宽
    let half_width = |dy: f64| -> f64 {
        let ly = half - dy.abs();
        if ly < 0.0 {
            -1.0 // 图形外(哨兵值)
        } else if ly >= corner.p {
            half // 平直边
        } else {
            half - corner.lx_at(ly)
        }
    };

    let mut out: RgbaImage = ImageBuffer::new(w, h);
    let step = 1.0 / SS as f64;
    // 快速路径余量:像素在纵向上跨 ±0.5 行,行边界 hw 最多变化 max_slope×0.5,
    // 再加横向的 0.5,才是「整像素必在内」的安全距离(陡圆弧处 ≈1.5px)
    let margin = 0.5 * (1.0 + corner.max_slope);
    for y in 0..h {
        for x in 0..w {
            // 快速路径:像素中心到边界的水平距离足以判定整像素在内/在外
            let px = x as f64 + 0.5 - cx;
            let hw = half_width(y as f64 + 0.5 - cx);
            let dist = hw - px.abs();
            let coverage: f64 = if dist >= margin {
                1.0
            } else if dist <= -margin {
                0.0
            } else {
                // 边界带:SS×SS 超采样覆盖率
                let mut hit = 0u32;
                for sy in 0..SS {
                    for sx in 0..SS {
                        let sx_ = x as f64 + (sx as f64 + 0.5) * step - cx;
                        let sy_ = y as f64 + (sy as f64 + 0.5) * step - cx;
                        let hw_ = half_width(sy_);
                        if hw_ >= 0.0 && sx_.abs() <= hw_ {
                            hit += 1;
                        }
                    }
                }
                hit as f64 / (SS * SS) as f64
            };

            let src_px = src.get_pixel(x, y);
            let mut px: Rgba<u8> = *src_px;
            // mask alpha 乘到原图 alpha 上(原图可能有透明区域)
            px[3] = ((px[3] as f64) * coverage).round() as u8;
            out.put_pixel(x, y, px);
        }
    }
    out
}
