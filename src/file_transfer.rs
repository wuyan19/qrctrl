//! 大文件传输：HTTP 流式上传 / 下载 + 内存级 transfer registry。
//!
//! 设计要点：
//! - 控制面（WebSocket）只协商元信息和返回带 token 的 URL
//! - 数据面（HTTP）流式接收 / 发送，内存恒定
//! - upload_id / download_id 一次性消费，5 分钟过期，避免堆积

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Json;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::state::AppState;

const ENTRY_TTL: Duration = Duration::from_secs(300);

pub struct UploadMeta {
    pub name: String,
    pub size: u64,
    pub created_at: Instant,
}

pub struct DownloadMeta {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub created_at: Instant,
}

#[derive(Clone, Default)]
pub struct TransferRegistry {
    uploads: Arc<parking_lot::Mutex<HashMap<String, UploadMeta>>>,
    downloads: Arc<parking_lot::Mutex<HashMap<String, DownloadMeta>>>,
}

impl TransferRegistry {
    pub fn register_upload(&self, meta: UploadMeta) -> String {
        let id = Uuid::new_v4().to_string();
        // parking_lot::lock() 不返回 Result（不 poison），无需 unwrap。
        self.uploads.lock().insert(id.clone(), meta);
        id
    }

    pub fn register_download(&self, meta: DownloadMeta) -> String {
        let id = Uuid::new_v4().to_string();
        self.downloads.lock().insert(id.clone(), meta);
        id
    }

    pub fn take_upload(&self, id: &str) -> Option<UploadMeta> {
        self.uploads.lock().remove(id)
    }

    pub fn take_download(&self, id: &str) -> Option<DownloadMeta> {
        self.downloads.lock().remove(id)
    }

    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        self.uploads
            .lock()
            .retain(|_, m| now.duration_since(m.created_at) < ENTRY_TTL);
        self.downloads
            .lock()
            .retain(|_, m| now.duration_since(m.created_at) < ENTRY_TTL);
    }
}

/// 把任意输入转成纯文件名，拒绝路径遍历。
/// "/etc/passwd" → "passwd"；"a/b/c.txt" → "c.txt"；"../etc/passwd" → None；"" → None。
pub fn sanitize_filename(raw: &str) -> Option<String> {
    use std::path::Component;
    let p = Path::new(raw);
    // 显式拒绝 ParentDir，避免 file_name() 截断后绕过（如 "../etc/passwd"）
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let name = p.file_name()?.to_str()?.to_string();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name)
}

/// 文件名冲突时加 UUID 短码后缀：`movie.mkv` → `movie_a3c7f8d2.mkv`。
pub fn resolve_conflict(dir: &Path, name: &str) -> PathBuf {
    let target = dir.join(name);
    if !target.exists() {
        return target;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let suffix = &Uuid::new_v4().to_string()[..8];
    dir.join(format!("{}_{}{}", stem, suffix, ext))
}

/// POST /upload/{id}?t=<token>
/// 流式接收 body 写盘到 save_dir，累计大小超过 max_size 时中断 + 删半成品。
/// 响应返回实际保存的文件名（可能与上传时的 name 不同——`resolve_conflict` 重名时会加 UUID 后缀），
/// 前端收集一批上传的所有 name，批结束后用 WS `set_clipboard_files` 一次性推剪贴板。
#[derive(Serialize)]
pub struct UploadResponse {
    /// 实际落盘的文件名（可能与上传 name 不同——重名时加 UUID 后缀）。
    pub name: String,
}

pub async fn upload_handler(
    _: crate::state::Authed,
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Body,
) -> Result<Json<UploadResponse>, StatusCode> {
    let meta = state
        .registry
        .take_upload(&id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let target = resolve_conflict(&state.save_dir, &meta.name);
    let mut file = tokio::fs::File::create(&target)
        .await
        .map_err(|e| {
            tracing::error!("创建文件失败 {}: {}", target.display(), e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    let max = state.max_size;
    let mut err: Option<StatusCode> = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("读取上传流失败: {}", e);
                err = Some(StatusCode::BAD_REQUEST);
                break;
            }
        };
        total += chunk.len() as u64;
        if total > max {
            err = Some(StatusCode::PAYLOAD_TOO_LARGE);
            break;
        }
        if let Err(e) = file.write_all(&chunk).await {
            tracing::error!("写入文件失败: {}", e);
            err = Some(StatusCode::INTERNAL_SERVER_ERROR);
            break;
        }
    }

    if let Some(code) = err {
        drop(file);
        let _ = tokio::fs::remove_file(&target).await;
        return Err(code);
    }

    if let Err(e) = file.flush().await {
        tracing::error!("flush 文件失败: {}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    tracing::info!(
        "上传完成: {} (声明 {} / 实际 {} 字节)",
        target.display(),
        meta.size,
        total
    );

    // 实际落盘文件名（重名时 resolve_conflict 会加 UUID 后缀）。前端收集后通过
    // WS `set_clipboard_files` 一次性推剪贴板，不在 upload 里推（避免多文件互相覆盖）。
    let saved_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(UploadResponse { name: saved_name }))
}

/// GET /download/{id}?t=<token>
/// 流式发送文件。Content-Disposition 触发浏览器下载。
pub async fn download_handler(
    _: crate::state::Authed,
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, StatusCode> {
    let meta = state
        .registry
        .take_download(&id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let file = match tokio::fs::File::open(&meta.path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("打开下载文件失败 {}: {}", meta.path.display(), e);
            return Err(StatusCode::NOT_FOUND);
        }
    };

    // filename 用 RFC 5987 编码处理非 ASCII 字符（中文文件名）
    let filename_star = percent_encode_filename(&meta.name);
    let disposition = format!("attachment; filename*=UTF-8''{}", filename_star);

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(&meta.mime).unwrap_or_else(|_| {
            header::HeaderValue::from_static("application/octet-stream")
        }),
    );
    if let Ok(v) = header::HeaderValue::from_str(&meta.size.to_string()) {
        resp.headers_mut().insert(header::CONTENT_LENGTH, v);
    }
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// RFC 3986 percent-encoding（仅 UTF-8 字节需要转义，ASCII 字母数字和 -_.~ 保留）。
fn percent_encode_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path() {
        assert_eq!(sanitize_filename("/etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_filename("a/b/c.txt").as_deref(), Some("c.txt"));
        assert_eq!(
            sanitize_filename(r"D:\dir\movie.mkv").as_deref(),
            Some("movie.mkv")
        );
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert_eq!(sanitize_filename("../etc/passwd"), None);
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename("."), None);
    }

    #[test]
    fn sanitize_rejects_empty() {
        assert_eq!(sanitize_filename(""), None);
    }

    #[test]
    fn sanitize_keeps_plain_name() {
        assert_eq!(sanitize_filename("movie.mkv").as_deref(), Some("movie.mkv"));
        assert_eq!(sanitize_filename("文档.pdf").as_deref(), Some("文档.pdf"));
    }

    #[test]
    fn resolve_conflict_no_existing() {
        let dir = tempdir();
        let got = resolve_conflict(&dir, "free.txt");
        assert_eq!(got.file_name().unwrap(), "free.txt");
    }

    #[test]
    fn resolve_conflict_adds_suffix() {
        let dir = tempdir();
        std::fs::write(dir.join("dup.txt"), b"x").unwrap();
        let got = resolve_conflict(&dir, "dup.txt");
        let name = got.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("dup_"));
        assert!(name.ends_with(".txt"));
        assert_ne!(name, "dup.txt");
    }

    #[test]
    fn resolve_conflict_no_extension() {
        let dir = tempdir();
        std::fs::write(dir.join("README"), b"x").unwrap();
        let got = resolve_conflict(&dir, "README");
        let name = got.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("README_"));
        assert!(!name.contains('.'));
    }

    #[test]
    fn percent_encode_keeps_ascii() {
        assert_eq!(percent_encode_filename("movie.mkv"), "movie.mkv");
    }

    #[test]
    fn percent_encode_escapes_non_ascii() {
        let got = percent_encode_filename("文档.pdf");
        assert!(got.starts_with("%E6%96%87"));
        assert!(got.ends_with(".pdf"));
    }

    #[test]
    fn percent_encode_escapes_spaces() {
        assert_eq!(percent_encode_filename("a b.txt"), "a%20b.txt");
    }

    /// 临时目录辅助：用进程唯一名 + 测试结束手动清理。
    /// 不用 tempfile crate（避免新依赖），测试内自己 cleanup。
    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "qrctrl_test_{}_{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
