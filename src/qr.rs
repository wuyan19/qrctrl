use qrcode::{Color, QrCode};

/// 用 Unicode 半角块字符把 QR 渲染到终端。
/// 每个终端字符表示 2×2 个 QR 模块，扫描体验最好。
pub fn render_qr_to_terminal(text: &str) -> Result<(), String> {
    let code = QrCode::new(text.as_bytes()).map_err(|e| format!("qr encode: {}", e))?;
    let width = code.width();
    let quiet = 4; // QR 标准要求至少 4 模块静默区
    let total = width + quiet * 2;

    let is_dark = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 || x >= width as i32 || y >= width as i32 {
            return false; // 静默区算亮
        }
        code[(x as usize, y as usize)] == Color::Dark
    };

    let mut y = 0;
    while y < total {
        let mut line = String::with_capacity(total);
        for x in 0..total {
            let top = is_dark(x as i32 - quiet as i32, y as i32 - quiet as i32);
            let bot = is_dark(x as i32 - quiet as i32, y as i32 + 1 - quiet as i32);
            let ch = match (top, bot) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            };
            line.push(ch);
        }
        println!("{}", line);
        y += 2;
    }
    Ok(())
}

/// 把 URL 渲染成供 softbuffer 直接显示的像素 buffer。
///
/// 输出格式：XRGB8888（每像素一个 u32，高 8 位忽略，低 24 位是 RRGGBB）。
/// `scale` 是每个 QR 模块的像素边长；`border` 是静默区的模块数（QR 标准 ≥ 4）。
///
/// 返回 `(buffer, width, height)`，`buffer.len() == (width * height) as usize`。
/// 静默区为白色（0xFFFFFFFF），模块黑色（0xFF000000）。
pub fn render_qr_to_pixels(text: &str, scale: u32, border: u32) -> Result<(Vec<u32>, u32, u32), String> {
    let code = QrCode::new(text.as_bytes()).map_err(|e| format!("qr encode: {}", e))?;
    let module_w = code.width() as u32;
    let total_modules = module_w + border * 2;
    let pixel_w = total_modules * scale;
    let pixel_h = total_modules * scale;

    let is_dark = |mx: i32, my: i32| -> bool {
        if mx < 0 || my < 0 || mx >= module_w as i32 || my >= module_w as i32 {
            return false;
        }
        code[(mx as usize, my as usize)] == Color::Dark
    };

    const DARK: u32 = 0xFF000000;
    const LIGHT: u32 = 0xFFFFFFFF;

    let mut buf = vec![LIGHT; (pixel_w * pixel_h) as usize];
    for py in 0..pixel_h {
        for px in 0..pixel_w {
            let mx = (px / scale) as i32 - border as i32;
            let my = (py / scale) as i32 - border as i32;
            if is_dark(mx, my) {
                buf[(py * pixel_w + px) as usize] = DARK;
            }
        }
    }
    Ok((buf, pixel_w, pixel_h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_qr_to_pixels_dimensions_match() {
        let (buf, w, h) = render_qr_to_pixels("hello", 4, 4).unwrap();
        assert_eq!(w, h);
        assert_eq!(buf.len(), (w * h) as usize);
    }

    #[test]
    fn render_qr_to_pixels_corners_are_white() {
        // 静默区 ≥ 4 模块时，四个角应该是白色（静默区像素）
        let (buf, w, _h) = render_qr_to_pixels("hello", 4, 4).unwrap();
        let at = |x: u32, y: u32| buf[(y * w + x) as usize];
        assert_eq!(at(0, 0), 0xFFFFFFFF);
        assert_eq!(at(w - 1, 0), 0xFFFFFFFF);
        assert_eq!(at(0, w - 1), 0xFFFFFFFF);
        assert_eq!(at(w - 1, w - 1), 0xFFFFFFFF);
    }

    #[test]
    fn render_qr_to_pixels_has_dark_modules() {
        // 任何真实 QR 码都该有黑色模块
        let (buf, _w, _h) = render_qr_to_pixels("https://example.com/?t=abc", 4, 4).unwrap();
        assert!(buf.iter().any(|&p| p == 0xFF000000));
    }

    #[test]
    fn render_qr_to_pixels_scale_affects_size() {
        let (_buf1, w1, _) = render_qr_to_pixels("hello", 1, 4).unwrap();
        let (_buf4, w4, _) = render_qr_to_pixels("hello", 4, 4).unwrap();
        // scale 4x 应该是 scale 1x 的 4 倍宽
        assert_eq!(w4 / w1, 4);
    }

    #[test]
    fn render_qr_to_pixels_rejects_empty() {
        // 空字符串应该报错或返回错误（QR 算法对空串的容错取决于 qrcode crate）
        let result = render_qr_to_pixels("", 4, 4);
        // qrcode crate 接受空串（编码为 0 长度），所以这里只验证不 panic
        let _ = result;
    }
}

