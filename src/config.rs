//! 配置文件本体：load / save / validate。
//!
//! 设计要点：
//! - 配置文件位置：`dirs::config_dir()/qrctrl/config.toml`，TOML 格式
//! - 所有字段 `Option<T>`，`None` 表示「未设置」（用于三层叠加：built-in → 文件 → CLI）
//! - 损坏文件**绝不 panic**（tray app 双击启动下 panic = 静默崩溃），改名 `.bad-{ts}`
//!   备份后用 default 继续
//!
//! 配置页的 HTTP handlers 在 `api.rs`，页面 handler 在 `assets.rs`——
//! 「配置文件」和「配置页 Web 服务」是两个职责，分文件避免本模块无限膨胀。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const ONE_TB: u64 = 1024 * 1024 * 1024 * 1024;

/// 配置文件内容。全部 `Option<T>`，`None` = 未设置（让下层默认生效）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub addr: Option<String>,
    pub port: Option<u16>,
    pub name: Option<String>,
    pub save_dir: Option<PathBuf>,
    pub max_size: Option<u64>,
    pub token: Option<String>,
    pub prefer_ip: Option<String>,
    /// 主题偏好：`"dark"` / `"light"` / `"system"`。None = 跟随系统。
    /// 前端 index.html 通过 ws server_info 拿到后用 `[data-theme]` 应用。
    /// 与其他字段不同：theme 走 live-apply（POST /api/theme 立即改 state + 写文件），
    /// 因为切换主题不该要求用户重启——重启语义是给「真正影响 server 启动」的字段用的。
    pub theme: Option<String>,

    /// 触控板灵敏度倍数（默认 1.0）。后端 ws dispatch 收到 mouse_move 时
    /// 把 dx/dy 乘以该值再注入 enigo，前端完全不感知。
    /// 同 theme 一样走 live-apply（POST /api/mouse_sensitivity）：改 state + 写文件，
    /// 不需要重启。范围 0.1-5.0，过低触控板太肉、过高难以精准点击。
    pub mouse_sensitivity: Option<f32>,

    /// 跳过指定版本不再提示更新（如 "0.11.2"）。用户在配置页点「跳过此版本」写入，
    /// 后续检查发现最新版等于它时按已跳过处理。
    pub skip_version: Option<String>,
}

/// 返回配置文件路径。`dirs::config_dir()` 在某些嵌入式环境可能返回 None，做兜底。
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("qrctrl").join("config.toml"))
}

/// 加载配置文件。文件不存在 = `Config::default()`（所有字段 None）。
/// 解析失败 = 把原文件备份成 `config.toml.bad-{timestamp}`，返回 default。
/// 任何阶段都**不 panic**。
pub fn load() -> Config {
    let path = match config_path() {
        Some(p) => p,
        None => {
            tracing::warn!("系统未提供 config_dir，跳过配置文件");
            return Config::default();
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => {
            tracing::warn!("读取配置 {} 失败：{}，跳过配置文件", path.display(), e);
            return Config::default();
        }
    };
    match toml::from_str::<Config>(&text) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("解析配置 {} 失败：{}", path.display(), e);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = path.with_extension(format!("toml.bad-{}", ts));
            if std::fs::rename(&path, &backup).is_ok() {
                tracing::info!("原配置文件已备份到 {}", backup.display());
            }
            Config::default()
        }
    }
}

/// 写入配置文件。自动创建父目录。直接 write（非原子）——小 TOML 文件可接受。
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "系统未提供 config_dir")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, text)
}

/// 字段级校验。空字段（None）一律放行——代表「让下层默认生效」。
pub fn validate(cfg: &Config) -> Result<(), String> {
    if let Some(p) = cfg.port {
        if p == 0 {
            return Err("端口不能为 0".into());
        }
    }
    if let Some(ref addr) = cfg.addr {
        if addr.trim().is_empty() {
            return Err("监听地址不能为空".into());
        }
    }
    if let Some(s) = cfg.max_size {
        if s == 0 {
            return Err("单文件上限必须大于 0".into());
        }
        if s > ONE_TB {
            return Err("单文件上限不能超过 1 TB".into());
        }
    }
    if let Some(ref t) = cfg.token {
        if let Err(e) = crate::token::validate_token(t) {
            return Err(format!("token 不合法：{}", e));
        }
    }
    if let Some(ref theme) = cfg.theme {
        let t = theme.trim().to_lowercase();
        if !matches!(t.as_str(), "dark" | "light" | "system") {
            return Err("theme 必须是 dark / light / system".into());
        }
    }
    if let Some(s) = cfg.mouse_sensitivity {
        if !s.is_finite() || s < 0.1 || s > 5.0 {
            return Err("mouse_sensitivity 必须是 0.1-5.0 之间的有限数".into());
        }
    }
    if let Some(ref v) = cfg.skip_version {
        if v.trim().is_empty() {
            return Err("skip_version 不能是空字符串".into());
        }
    }
    Ok(())
}

/// 校验并归一化 theme 字符串（trim + lowercase + 必须是合法三选一）。
/// `set_theme_handler` 单独走这条路径，因为它要 live apply + 写文件，
/// 不能依赖 Config 反序列化（前端可能传非法值）。
pub fn normalize_theme(raw: &str) -> Result<String, String> {
    let t = raw.trim().to_lowercase();
    if matches!(t.as_str(), "dark" | "light" | "system") {
        Ok(t)
    } else {
        Err("theme 必须是 dark / light / system".into())
    }
}
