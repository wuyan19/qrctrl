//! 给 `assets/icon.png` 应用 iOS 风格 squircle mask,覆盖写回。
//!
//! 用法:`cargo run --example apply_icon_mask`
//!
//! 为什么要这一步:Windows / Linux 平台**不会**自动给应用图标套 mask,
//! 所以源图必须自带圆角;macOS Launchpad 会自动 mask,但 squircle 形状
//! 跟系统一致,肉眼无「双重圆角」现象。
//!
//! 数学上 squircle 是超椭圆 `|u/n|^n + |v/n|^n = 1`,其中 u, v 是归一化
//! 到画布中心的坐标。iOS App Icon 的实际形状用一系列 Bezier 定义,n=5 的
//! 超椭圆是常见近似(再大就接近方形,再小就像普通圆角矩形)。
//!
//! 抗锯齿:在 p = |u|^n + |v|^n = 1 边界两侧,按像素距离线性过渡 alpha。
//! 1024×1024 + n=5 + aa_width=2 px 时,边界看起来平滑。
//!
//! 跟 `gen_icon.rs` 同一套「`assets/icon.png` 是源真」约定:
//! ① 用 Agnes / 其他工具生成新的方版 icon.png →
//! ② `cargo run --example apply_icon_mask` 应用圆角 →
//! ③ `cargo run --example gen_icon` 重生成 Windows ICO →
//! ④ `pwsh scripts/regen-tray-icon.ps1` 重生成 32×32 托盘图标 →
//! ⑤ `cargo build --release`。

use std::path::Path;

use image::{ImageBuffer, Rgba, RgbaImage};

const N: f64 = 5.0; // squircle 指数;iOS ≈ 5
const AA_WIDTH_PX: f64 = 2.0; // 边界抗锯齿宽度(像素)

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
    println!("[apply_icon_mask] 源图尺寸:{}x{},应用 n={} squircle mask", w, h, N);

    // 备份原方版到 icon.square.png,方便后续重做 mask 时拿原版
    let backup = Path::new(&manifest_dir)
        .join("assets")
        .join("icon.square.png");
    std::fs::copy(&src, &backup)?;
    println!("[apply_icon_mask] 原方版备份到 {}", backup.display());

    let masked = apply_squircle(&rgba);
    masked.save(&src)?;
    println!("[apply_icon_mask] 已写回 {}", src.display());
    println!("[apply_icon_mask] 接下来:");
    println!("  cargo run --example gen_icon");
    println!("  pwsh scripts/regen-tray-icon.ps1");
    println!("  cargo build --release");
    Ok(())
}

/// 对 RGBA 图像应用超椭圆 mask。
///
/// 归一化坐标 u = (x - cx) / r,v = (y - cy) / r,其中 r 是 squircle 半径
/// (画布半径减 padding,留 ~2 px 边距避免边缘抗锯齿残留)。
/// `p = |u|^n + |v|^n`,p ≤ 1 在 mask 内,p > 1 在 mask 外。
///
/// 抗锯齿:计算 p 在像素空间的梯度近似,在边界 ±AA_WIDTH_PX 范围内线性过渡 alpha。
/// 梯度估计:`dp/du = n * |u|^(n-1) * sign(u)`,1 px 对应的 Δu = 1/r,所以
/// 1 px 对应的 Δp ≈ n * |u|^(n-1) / r。边界处 |u|、|v| 大约 0.7(角附近),
/// 所以 Δp/px ≈ 5 * 0.7^4 / r ≈ 1.2 / r。1024 画布下 r ≈ 510,Δp/px ≈ 0.0024,
/// 远小于 1。用像素距离做 AA 更直观:dist_px = (1 - p) / Δp_per_px。
fn apply_squircle(src: &RgbaImage) -> RgbaImage {
    let (w, h) = src.dimensions();
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let pad = 2.0;
    let r = (w as f64 / 2.0) - pad;

    let mut out: RgbaImage = ImageBuffer::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let u = (x as f64 - cx) / r;
            let v = (y as f64 - cy) / r;

            let au = u.abs();
            let av = v.abs();
            let p = au.powf(N) + av.powf(N);

            // p 对应的像素距离(粗略估计):在边界处 p=1,dist_px = 0;
            // mask 内 p<1 → dist_px>0;mask 外 p>1 → dist_px<0。
            // 梯度:n * max(|u|,|v|)^(n-1) / r,角附近 au≈av≈0.7,中心附近更小。
            // 用 max 保证角附近 AA 不被低估。
            let grad = N * au.max(av).powf(N - 1.0) / r;
            let dist_px = if grad > 0.0 { (1.0 - p) / grad } else { 1.0 };

            // AA:dist_px ≥ AA_WIDTH/2 完全不透明,≤ -AA_WIDTH/2 完全透明,中间线性
            let mask_alpha = ((dist_px / AA_WIDTH_PX) + 0.5).clamp(0.0, 1.0);

            let src_px = src.get_pixel(x, y);
            let mut px: Rgba<u8> = *src_px;
            // 把 mask alpha 乘到原图 alpha 上(原图可能有透明区域)
            let combined = (px[3] as f64 * mask_alpha).round() as u8;
            px[3] = combined;
            out.put_pixel(x, y, px);
        }
    }
    out
}
