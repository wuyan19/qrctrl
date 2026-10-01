//! 从 `assets/icon.png` 生成平台派生图标:
//! - `assets/windows/icon.ico`:Windows 多尺寸 ICO;
//! - `assets/macos/icon-1024.png`:macOS 版 1024 主图(图形缩到 824 居中,
//!   四周 100px 透明边距——Apple 图标规范,满幅图标在 Dock/Finder 中会比
//!   系统图标显大)。
//!
//! 用法：`cargo run --example gen_icon`
//!
//! ICO 文件支持多尺寸，Windows 资源管理器、任务栏、Alt+Tab 等会按显示场景选最合适的。
//! 这里生成 [16, 24, 32, 48, 64, 128, 256] 七个常见尺寸，覆盖从通知区域到 Jumplist
//! 的所有展示位。源图 1024×1024，缩放用 Lanczos3 滤镜（image crate 提供的最锐的）。
//!
//! 生成后:ICO 由 `build.rs` 通过 `winres` 嵌入 .exe 的资源段;macOS 版由
//! `scripts/build-macos-app.sh` 用 sips/iconutil 切成 icns。源图改了重跑这个
//! 脚本即可,与 macOS 脚本同一套「`assets/icon.png` 是源真」约定。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use image::imageops::FilterType;
use image::{Rgba, RgbaImage};
use ico::{IconDir, IconDirEntry, IconImage, ResourceType};

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// macOS 主图:1024 画布,图形 824 居中(80%),四周 100px 透明。
const MACOS_ART: u32 = 824;
const MACOS_PAD: u32 = (1024 - MACOS_ART) / 2;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // CARGO_MANIFEST_DIR 在 `cargo run --example` 时指向项目根，否则回退到相对路径。
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let src = Path::new(&manifest_dir).join("assets").join("icon.png");

    println!("[gen_icon] 源图：{}", src.display());
    let src_img = image::open(&src).map_err(|e| format!("读 {} 失败：{}", src.display(), e))?;
    let src_rgba = src_img.to_rgba8();
    let (src_w, src_h) = src_rgba.dimensions();
    println!("[gen_icon] 源图尺寸：{}x{}", src_w, src_h);

    let mut icon_dir = IconDir::new(ResourceType::Icon);
    for &size in &SIZES {
        // 256 以下都从源图直接缩；源图就是 256 时 to_rgba8 拿到原数据，无质量损失。
        let resized = if src_w == size && src_h == size {
            src_rgba.clone()
        } else {
            image::imageops::resize(&src_rgba, size, size, FilterType::Lanczos3)
        };
        let rgba = resized.into_raw();
        let image = IconImage::from_rgba_data(size, size, rgba);
        let entry = IconDirEntry::encode(&image)?;
        icon_dir.add_entry(entry);
        println!("[gen_icon] + {}x{} 已编码", size, size);
    }

    let dst_dir = Path::new(&manifest_dir).join("assets").join("windows");
    std::fs::create_dir_all(&dst_dir)?;
    let out = File::create(dst_dir.join("icon.ico"))?;
    icon_dir.write(BufWriter::new(out))?;
    println!("[gen_icon] 写出：{}", dst_dir.join("icon.ico").display());

    write_macos_icon(&manifest_dir, &src_rgba)?;
    Ok(())
}

/// 缩到 824、贴到透明 1024 画布中心,写 `assets/macos/icon-1024.png`。
///
/// 源图 mask 时留的 2px 抗锯齿边距先按 alpha 包围盒裁掉,保证图形恰好
/// 占满 824(否则边距会被等比缩进去,图形变成 ~820,Dock 里比系统图标小一圈)。
fn write_macos_icon(manifest_dir: &str, src_rgba: &RgbaImage) -> Result<(), Box<dyn std::error::Error>> {
    let (sw, sh) = src_rgba.dimensions();
    let (x0, y0, x1, y1) = alpha_bbox(src_rgba)
        .ok_or("icon.png 全透明,没有可裁的图形边界")?;
    let art_src = image::imageops::crop_imm(src_rgba, x0, y0, x1 - x0, y1 - y0).to_image();
    println!(
        "[gen_icon] macOS 源裁剪:({},{})..({},{}) {}x{}(原图 {}x{})",
        x0, y0, x1, y1, x1 - x0, y1 - y0, sw, sh
    );
    let art = image::imageops::resize(&art_src, MACOS_ART, MACOS_ART, FilterType::Lanczos3);
    let mut canvas = RgbaImage::from_pixel(1024, 1024, Rgba([0, 0, 0, 0]));
    image::imageops::overlay(&mut canvas, &art, MACOS_PAD.into(), MACOS_PAD.into());

    let dst_dir = Path::new(manifest_dir).join("assets").join("macos");
    std::fs::create_dir_all(&dst_dir)?;
    let dst = dst_dir.join("icon-1024.png");
    canvas.save(&dst)?;
    println!("[gen_icon] 写出：{}（art {}px + pad {}px）", dst.display(), MACOS_ART, MACOS_PAD);
    Ok(())
}

/// alpha > 0 的包围盒 (x0, y0, x1, y1),全透明返回 None。
fn alpha_bbox(img: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = img.dimensions();
    let mut x0 = w; let mut y0 = h; let mut x1 = 0u32; let mut y1 = 0u32;
    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y)[3] > 0 {
                x0 = x0.min(x); y0 = y0.min(y);
                x1 = x1.max(x + 1); y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 { None } else { Some((x0, y0, x1, y1)) }
}
