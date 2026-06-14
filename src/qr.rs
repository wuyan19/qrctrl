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
