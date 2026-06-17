//! 从 `assets/icon.png` 生成 `assets/windows/icon.ico`。
//!
//! 用法：`cargo run --example gen_icon`
//!
//! ICO 文件支持多尺寸，Windows 资源管理器、任务栏、Alt+Tab 等会按显示场景选最合适的。
//! 这里生成 [16, 24, 32, 48, 64, 128, 256] 七个常见尺寸，覆盖从通知区域到 Jumplist
//! 的所有展示位。源图 512×512，缩放用 Lanczos3 滤镜（image crate 提供的最锐的）。
//!
//! 生成后由 `build.rs` 通过 `winres` 嵌入 .exe 的资源段。源图改了重跑这个脚本即可，
//! 与 macOS 的 `scripts/build-macos-app.sh` 同一套「`assets/icon.png` 是源真」约定。

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use image::imageops::FilterType;
use ico::{IconDir, IconDirEntry, IconImage, ResourceType};

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // CARGO_MANIFEST_DIR 在 `cargo run --example` 时指向项目根，否则回退到相对路径。
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let src = Path::new(&manifest_dir).join("assets").join("icon.png");
    let dst_dir = Path::new(&manifest_dir).join("assets").join("windows");
    let dst = dst_dir.join("icon.ico");

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

    std::fs::create_dir_all(&dst_dir)?;
    let out = File::create(&dst)?;
    icon_dir.write(BufWriter::new(out))?;
    println!("[gen_icon] 写出：{}", dst.display());

    Ok(())
}
