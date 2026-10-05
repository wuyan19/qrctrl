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
/// `\` 一律视作路径分隔符（`std::path` 在 Unix 上不认它，但手机上传的名字可能带
/// Windows 风格路径，不能在 Mac/Linux 上原样落盘）：r"D:\dir\movie.mkv" → "movie.mkv"。
pub fn sanitize_filename(raw: &str) -> Option<String> {
    use std::path::Component;
    // 先归一化 `\` → `/`，让 Path 的组件语义跨平台一致（`..\` 变体也因此被拒）
    let normalized = raw.replace('\\', "/");
    let p = Path::new(&normalized);
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

/// 判断 name 是否是「可直接 join 到 save_dir 的纯文件名」：不含任何路径分隔符、
/// 不是 `.`/`..`，`sanitize_filename` 结果与自身完全一致。
///
/// 上传写盘通道用 `sanitize_filename`（会剥掉目录部分改写后落盘，天然安全）；
/// `set_clipboard_files` 通道必须用这个严格版本——它是「读」不是「写」，
/// 接受 `"../../etc/passwd"` 这类名字会把任意路径的文件推进剪贴板，
/// 再经 get_file → /download 变成任意文件外带。
pub fn is_plain_filename(name: &str) -> bool {
    sanitize_filename(name).as_deref() == Some(name)
}

/// save_dir 列表上限：目录被塞爆时保住 WS 消息体积和手机端列表长度。
/// 正常使用（手机逐个推文件到 PC）远达不到这个量级。
const MAX_SAVE_DIR_LIST: usize = 200;

/// 列出 save_dir 顶层的普通文件：跳过子目录与 dot 开头的隐藏文件（与配置页
/// list_dir 的降噪规则一致），按修改时间倒序（最近上传的排最前），最多
/// MAX_SAVE_DIR_LIST 个，每个包装成可直接注册下载的 DownloadMeta。
///
/// 给「拉文件」的剪贴板兜底通道用：剪贴板无文件时前端弹保存目录选择器，
/// 条目直接带 /download url。用 `DirEntry::metadata`（不穿透符号链接）判定
/// 普通文件，避免 save_dir 里被手工放进来的链接把目录外的文件带出去。
pub fn list_save_dir_files(save_dir: &Path) -> Vec<DownloadMeta> {
    let entries = match std::fs::read_dir(save_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(std::time::SystemTime, DownloadMeta)> = Vec::new();
    for entry in entries.flatten() {
        let name = match entry.file_name().to_str() {
            Some(n) if !n.starts_with('.') => n.to_string(),
            _ => continue,
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let path = entry.path();
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();
        out.push((
            modified,
            DownloadMeta {
                path,
                name,
                size: meta.len(),
                mime,
                created_at: Instant::now(),
            },
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().take(MAX_SAVE_DIR_LIST).map(|(_, m)| m).collect()
}

/// 删除 save_dir 顶层的文件，返回删除失败的名字列表。
///
/// 安全线：names 只接受纯文件名（`is_plain_filename` 过滤，含路径成分的一律
/// 计入失败，不给「删目录外文件」留口子）；用 `symlink_metadata` 判定真实
/// 普通文件（不跟随符号链接，目录 / 链接都不删）。失败原因（Windows 上文件
/// 被占用、已被外部删掉等）不逐个区分——前端只按 failed 数量提示。
pub fn delete_save_dir_files(save_dir: &Path, names: &[String]) -> Vec<String> {
    let mut failed = Vec::new();
    for name in names {
        if !is_plain_filename(name) {
            failed.push(name.clone());
            continue;
        }
        let path = save_dir.join(name);
        let is_regular = std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_file())
            .unwrap_or(false);
        if !is_regular || std::fs::remove_file(&path).is_err() {
            failed.push(name.clone());
        }
    }
    failed
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
        .core
        .registry
        .take_upload(&id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let target = resolve_conflict(&state.core.save_dir, &meta.name);
    let mut file = tokio::fs::File::create(&target)
        .await
        .map_err(|e| {
            tracing::error!("创建文件失败 {}: {}", target.display(), e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    let max = state.core.max_size;
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
        .core
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
    fn is_plain_filename_accepts_bare_names() {
        assert!(is_plain_filename("movie.mkv"));
        assert!(is_plain_filename("文档.pdf"));
        assert!(is_plain_filename("a b c.txt"));
    }

    #[test]
    fn is_plain_filename_rejects_any_path_component() {
        // sanitize 会剥掉目录部分改写（写盘通道安全），is_plain_filename 要求
        // 名字本身就是纯文件名——含分隔符/`..`/绝对路径一律拒绝
        assert!(!is_plain_filename("../etc/passwd"));
        assert!(!is_plain_filename("/etc/passwd"));
        assert!(!is_plain_filename("a/b/c.txt"));
        assert!(!is_plain_filename(r"D:\dir\movie.mkv"));
        assert!(!is_plain_filename(".."));
        assert!(!is_plain_filename("."));
        assert!(!is_plain_filename(""));
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
    fn list_save_dir_files_sorts_newest_first_and_skips_noise() {
        let dir = tempdir();
        std::fs::write(dir.join("old.txt"), b"1").unwrap();
        // 保证 mtime 严格递增：主流文件系统（NTFS/ext4/APFS）精度远高于此间隔
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("new.bin"), b"22").unwrap();
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        std::fs::write(dir.join(".hidden"), b"3").unwrap();

        let files = list_save_dir_files(&dir);
        let names: Vec<&str> = files.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["new.bin", "old.txt"]);
        assert_eq!(files[0].size, 2);
        // 未知扩展名兜底 octet-stream，已知扩展名有具体 mime
        assert_eq!(files[0].mime, "application/octet-stream");
        assert!(files[1].mime.starts_with("text/"));
    }

    #[test]
    fn list_save_dir_files_missing_dir_returns_empty() {
        let files = list_save_dir_files(Path::new("/nonexistent/qrctrl-test"));
        assert!(files.is_empty());
    }

    #[test]
    fn delete_save_dir_files_removes_only_regular_top_level() {
        let dir = tempdir();
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();

        let failed = delete_save_dir_files(
            &dir,
            &[
                "a.txt".to_string(),
                "../evil".to_string(),
                "sub".to_string(),
                "missing.txt".to_string(),
            ],
        );

        assert!(!dir.join("a.txt").exists());
        assert!(dir.join("b.txt").exists());
        assert!(dir.join("sub").exists());
        // 穿越名、目录名、不存在的名字都计入 failed（顺序跟随输入）
        assert_eq!(failed, vec!["../evil", "sub", "missing.txt"]);
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

    /// 临时目录辅助：Drop 时整目录删除（断言失败也能清理，不跨运行累积）。
    /// 不用 tempfile crate（避免新依赖）。Deref 到 Path 让调用点直接 `dir.join(...)`。
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = std::path::Path;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "qrctrl_test_{}_{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
