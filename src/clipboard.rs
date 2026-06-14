//! 剪贴板读写包装。
//!
//! arboard 是阻塞 API，所有副作用函数（read_text / read_image / write_image）
//! 必须在 `tokio::task::spawn_blocking` 中调用。解码/编码抽成纯函数，
//! 便于单元测试（不接触系统剪贴板）。

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose, Engine as _};
use image::ImageEncoder;

/// 共享剪贴板句柄。Arc<Mutex<...>> 包裹，
/// macOS 上 arboard::Clipboard 仅 Send 不 Sync。
pub type ClipboardHandle = Arc<Mutex<arboard::Clipboard>>;

/// 像素总数上限（约 4000 万像素 ≈ 160 MB RGBA）。
const MAX_PIXELS: usize = 40_000_000;

/// base64 字符串上限（约 10 MB）。
pub const MAX_IMG_B64: usize = 10_000_000;

#[derive(Debug)]
pub enum CbError {
    ContentNotAvailable,
    ClipboardOccupied,
    ConversionFailure,
    TooLarge,
    Unknown(String),
}

impl std::fmt::Display for CbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CbError::ContentNotAvailable => write!(f, "content not available"),
            CbError::ClipboardOccupied => write!(f, "clipboard occupied"),
            CbError::ConversionFailure => write!(f, "conversion failure"),
            CbError::TooLarge => write!(f, "too large"),
            CbError::Unknown(s) => write!(f, "unknown: {}", s),
        }
    }
}

/// 把 CbError 映射为协议错误码字符串。
pub fn error_code(e: &CbError) -> &'static str {
    match e {
        CbError::ContentNotAvailable => "empty",
        CbError::ClipboardOccupied => "clipboard_busy",
        CbError::ConversionFailure => "decode_failed",
        CbError::TooLarge => "too_large",
        CbError::Unknown(_) => "internal",
    }
}

/// 创建共享剪贴板句柄。
pub fn new_handle() -> Result<ClipboardHandle, String> {
    let cb = arboard::Clipboard::new().map_err(|e| format!("剪贴板初始化失败: {}", e))?;
    Ok(Arc::new(Mutex::new(cb)))
}

fn map_err(e: arboard::Error) -> CbError {
    use arboard::Error;
    match e {
        Error::ContentNotAvailable => CbError::ContentNotAvailable,
        Error::ClipboardOccupied => CbError::ClipboardOccupied,
        Error::ConversionFailure => CbError::ConversionFailure,
        other => CbError::Unknown(other.to_string()),
    }
}

/// 把任意格式图片字节解码为 RGBA 像素。
/// 纯函数，不接触 arboard，便于单元测试。
pub fn decode_bytes_to_rgba(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>), CbError> {
    if bytes.is_empty() {
        return Err(CbError::ConversionFailure);
    }
    let img = image::load_from_memory(bytes).map_err(|_| CbError::ConversionFailure)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return Err(CbError::ConversionFailure);
    }
    let total = w.checked_mul(h).ok_or(CbError::TooLarge)?;
    if total > MAX_PIXELS {
        return Err(CbError::TooLarge);
    }
    let rgba = img.to_rgba8().into_raw();
    Ok((w, h, rgba))
}

/// 把 RGBA 像素编码为 PNG base64 字符串。
/// 纯函数，不接触 arboard，便于单元测试。
pub fn encode_rgba_to_png_base64(w: usize, h: usize, rgba: &[u8]) -> Result<String, CbError> {
    if rgba.len() != w * h * 4 {
        return Err(CbError::ConversionFailure);
    }
    let mut png_buf = Vec::with_capacity(rgba.len() / 2);
    let encoder = image::codecs::png::PngEncoder::new(&mut png_buf);
    encoder
        .write_image(&rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
        .map_err(|_| CbError::ConversionFailure)?;
    Ok(general_purpose::STANDARD.encode(&png_buf))
}

/// base64 解码辅助。失败时返回 ConversionFailure。
pub fn decode_base64(s: &str) -> Result<Vec<u8>, CbError> {
    general_purpose::STANDARD
        .decode(s)
        .map_err(|_| CbError::ConversionFailure)
}

/// 读剪贴板文本。返回 Ok(None) 表示空 / 无文本格式。
pub fn read_text(h: &ClipboardHandle) -> Result<Option<String>, CbError> {
    let mut cb = h.lock().map_err(|e| CbError::Unknown(format!("lock: {}", e)))?;
    match cb.get_text() {
        Ok(s) => Ok(Some(s)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(map_err(e)),
    }
}

/// 读剪贴板图片并编码为 PNG base64。返回 Ok(None) 表示空 / 无图片格式。
pub fn read_image_png_base64(h: &ClipboardHandle) -> Result<Option<(String, String)>, CbError> {
    let img = {
        let mut cb = h.lock().map_err(|e| CbError::Unknown(format!("lock: {}", e)))?;
        match cb.get_image() {
            Ok(img) => img,
            Err(arboard::Error::ContentNotAvailable) => return Ok(None),
            Err(e) => return Err(map_err(e)),
        }
    };
    let w = img.width;
    let h = img.height;
    let rgba = img.bytes.into_owned();
    let b64 = encode_rgba_to_png_base64(w, h, &rgba)?;
    Ok(Some(("image/png".to_string(), b64)))
}

/// 把图片字节（任意格式）解码后写入剪贴板。
pub fn write_image_from_bytes(handle: &ClipboardHandle, bytes: &[u8]) -> Result<(), CbError> {
    let (w, h, rgba) = decode_bytes_to_rgba(bytes)?;
    let mut cb = handle.lock().map_err(|e| CbError::Unknown(format!("lock: {}", e)))?;
    cb.set_image(arboard::ImageData {
        width: w,
        height: h,
        bytes: Cow::Owned(rgba),
    })
    .map_err(map_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 2x2 RGBA 测试像素（红 / 绿 / 蓝 / 白）。
    fn sample_rgba() -> (usize, usize, Vec<u8>) {
        let rgba = vec![
            255, 0, 0, 255,
            0, 255, 0, 255,
            0, 0, 255, 255,
            255, 255, 255, 255,
        ];
        (2, 2, rgba)
    }

    #[test]
    fn decode_empty_bytes_returns_conversion_failure() {
        let r = decode_bytes_to_rgba(&[]);
        assert!(matches!(r, Err(CbError::ConversionFailure)));
    }

    #[test]
    fn decode_corrupt_bytes_returns_conversion_failure() {
        let r = decode_bytes_to_rgba(&[0xff, 0xff, 0xff, 0xff, 0xff]);
        assert!(matches!(r, Err(CbError::ConversionFailure)));
    }

    #[test]
    fn encode_then_decode_roundtrip() {
        let (w, h, rgba) = sample_rgba();
        let b64 = encode_rgba_to_png_base64(w, h, &rgba).expect("encode ok");
        let bytes = general_purpose::STANDARD.decode(&b64).expect("base64 decode");
        let (w2, h2, rgba2) = decode_bytes_to_rgba(&bytes).expect("decode ok");
        assert_eq!((w2, h2), (w, h));
        assert_eq!(rgba2, rgba);
    }

    #[test]
    fn encode_wrong_byte_count_returns_conversion_failure() {
        let r = encode_rgba_to_png_base64(2, 2, &[0; 8]); // 应该是 16 字节
        assert!(matches!(r, Err(CbError::ConversionFailure)));
    }

    #[test]
    fn decode_png_bytes_succeeds() {
        // 构造合法 1x1 PNG（PNG signature + IHDR + IDAT + IEND，红点）。
        let png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
            0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41,
            0x54, 0x08, 0xd7, 0x63, 0xf8, 0xff, 0xff, 0x3f,
            0x00, 0x05, 0xfe, 0x02, 0xfe, 0xdc, 0xcc, 0x59,
            0xe7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
            0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let (w, h, rgba) = decode_bytes_to_rgba(png).expect("decode ok");
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba.len(), 4);
    }

    #[test]
    fn max_img_b64_is_about_10mb() {
        // 边界 sanity check：常量与文档一致。
        assert_eq!(MAX_IMG_B64, 10_000_000);
    }

    #[test]
    fn error_code_mapping() {
        assert_eq!(error_code(&CbError::ContentNotAvailable), "empty");
        assert_eq!(error_code(&CbError::ClipboardOccupied), "clipboard_busy");
        assert_eq!(error_code(&CbError::ConversionFailure), "decode_failed");
        assert_eq!(error_code(&CbError::TooLarge), "too_large");
        assert_eq!(error_code(&CbError::Unknown("x".into())), "internal");
    }

    #[test]
    fn decode_base64_invalid_returns_conversion_failure() {
        let r = decode_base64("!!!not base64!!!");
        assert!(matches!(r, Err(CbError::ConversionFailure)));
    }
}
