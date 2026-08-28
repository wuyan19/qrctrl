//! 在线升级（self-update）：检查 GitHub Releases → 下载 → sha256 校验 → 平台安装。
//!
//! 分层（自下而上，依赖只允许向下）：
//! 1. **纯逻辑**：manifest 解析 / 版本比较 / 平台 key / hex 编码 —— 无 IO，单测覆盖
//! 2. **IO 动作**：`check_latest` / `download_and_verify` / `install_update` ——
//!    同步阻塞函数，调用方（Updater 的后台任务）负责包在 spawn_blocking 里
//! 3. **状态机 `Updater`**：把 1/2 编排成前端可轮询的 `UpdatePhase`，
//!    `spawn_check` / `spawn_install` 是仅有的两个入口
//!
//! 平台安装策略（收敛在 `install_update` 的 cfg 分支，其余代码平台无关）：
//! - Windows / Linux：下载资产就是最终二进制，`self-replace` 处理「替换正在运行的自身」
//!   （Windows 的 rename dance 绕过运行中 exe 文件锁；Unix 靠 inode 语义天然安全）
//! - macOS：下载资产是 `.app.zip`，**整包替换** `qrctrl.app`——只换内部二进制会让
//!   ad-hoc 签名失效（启动被 Gatekeeper 拦）且 Info.plist 版本号 stale。rename 舞步
//!   保证替换失败可回滚
//!
//! 安全边界（纵深三层）：HTTPS 保通道 → manifest 内 sha256 保下载完整性 →
//! **minisign Ed25519 签名保真实性**（防 GitHub 账号被盗 / repo 被攻破后推恶意
//! 更新——攻击者拿不到私钥就产不出能通过验签的 manifest）。公钥编译期内嵌
//! （`assets/update-pubkey.txt`），私钥只存在于项目 `.sign/` 目录（gitignore）
//! 和 GitHub Secrets。manifest 外层是信封结构
//! `{payload, signature}`：签名对 payload 字节，客户端不重新序列化，验签字节
//! 与服务端签名字节完全一致（见 `SignedManifest`）。
//!
//! 密钥生成：`cargo run --example keygen_update`（私钥/口令落项目 `.sign/` 目录，
//! 已 gitignore）；CI 签名：`cargo run --example sign_update`（均为 dev 环境，
//! release binary 只含 minisign-verify 验签能力，不含私钥处理代码）。
//!
//! 重启时序：本模块**只负责把新版本放到磁盘上**（终态 `RestartPending`），
//! 不碰 tray_proxy / shutdown_notify——前端轮询到 `RestartPending` 后调现有的
//! `POST /api/restart`（spawn current_exe + QRCTRL_RESTART_CHILD 端口重试窗口），
//! 「进程协调只属于 tray/main」的模块边界保持不变。
//!
//! 更新源硬编码（`MANIFEST_URL`）：一旦可配置，签名体系就有被指向恶意服务器的
//! 社工入口。manifest 资产名（`update-manifest.json`）与 release.yml 的 manifest
//! job 是共享约定，改名必须两边同步。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
// PathBuf 只被 macOS 的 bundle 定位函数用到，其他平台 import 会报 unused。
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// manifest 资产挂在「最新非 draft 非 prerelease 的 release」下。走 GitHub 的
/// `releases/latest/download/<asset>` 稳定重定向端点而不是 REST API——后者未认证
/// 限速 60 次/小时/源 IP，前者走 release CDN 不占额度。draft release 不会被
/// `latest` 命中：发布后必须 publish，否则所有客户端静默查不到更新。
const MANIFEST_URL: &str =
    "https://github.com/wuyan19/qrctrl/releases/latest/download/update-manifest.json";
/// 下载硬上限，防 manifest 被篡改后 size 巨大撑爆磁盘。正常资产 < 20 MB。
const MAX_DOWNLOAD: u64 = 100 * 1024 * 1024;
/// 检查超时：后台任务，慢点无妨，但别挂死。
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// 下载超时：慢网下 20 MB 也该在 10 分钟内到。超时即 Failed，可重试。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// GitHub 强制要求 User-Agent，否则 403。
const USER_AGENT: &str = concat!("qrctrl/", env!("CARGO_PKG_VERSION"));
/// 更新签名公钥（minisign 公钥文件全文，标准两行：comment + base64）。
/// 生成：`cargo run --example keygen_update`（私钥/口令落 `.sign/`，已 gitignore）。
/// 密钥轮换：生成新密钥对后在数组里追加新公钥（include_str! 第二个文件），
/// 发一个过渡版本让存量客户端信任新钥，之后移除旧公钥。
const UPDATE_PUBKEYS: &[&str] = &[include_str!("../assets/update-pubkey.txt")];

// ===== 数据模型 =====

/// manifest 的外层信封。`payload` 是内层 UpdateManifest 的 JSON **字符串**，
/// `signature` 是对 payload 字节的 minisign 签名（.sig 全文）。
///
/// 为什么套一层字符串而不是直接签外层 JSON：验签必须针对**确定字节**。如果
/// 签名对象是「反序列化后重新序列化的 JSON」，两端的 key 顺序 / 空白 / 转义
/// 差异都会让验签失败；把 payload 当不透明字符串签名，客户端验证的就是服务端
/// 签名的原文，验完再 parse——字节确定性与解析自由度兼得。
#[derive(Debug, Deserialize)]
struct SignedManifest {
    payload: String,
    signature: String,
}

/// release.yml 的 manifest job 生成的更新清单（`SignedManifest.payload` 的内容）。
/// 字段与 CI 生成脚本约定（见 release.yml「Build update manifest」步骤）。
#[derive(Debug, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    /// 更新日志页（通常就是 release 页本身）。可为空，前端拿不到就不渲染链接。
    #[serde(default)]
    pub notes_url: String,
    /// 平台 key → 资产。key 见 [`platform_key`]。
    pub platforms: BTreeMap<String, PlatformAsset>,
}

/// 单个平台的更新资产描述。
#[derive(Debug, Clone, Deserialize)]
pub struct PlatformAsset {
    pub url: String,
    pub size: u64,
    /// sha256 hex（服务端生成时小写；比对大小写不敏感）。快速失败层：
    /// 下载流式算，不匹配立即中止，不用等读盘验签。
    pub sha256: String,
    /// minisign 签名（.sig 全文），对资产文件字节。真实性别：由 CI 用私钥
    /// （GitHub Secrets）生成，客户端用编译期内嵌公钥验。
    #[serde(default)]
    pub signature: String,
}

/// 一次检查的结果里，前端需要的全部信息（含内部下载所需的 asset）。
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes_url: String,
    pub asset: PlatformAsset,
}

pub enum UpdateOutcome {
    UpToDate,
    Available(ReleaseInfo),
}

/// 更新链路的全部错误。前端只拿 Display 文案，不区分 code——更新是线性流程，
/// 失败后用户动作只有「重试」或「手动下载」，无需程序化分支。
#[derive(Debug)]
pub enum UpdateError {
    /// HTTP / 网络 / 超时。
    Network(String),
    /// manifest 结构问题（JSON 坏 / 缺字段 / 缺当前平台资产 / 版本号非法）。
    Manifest(String),
    /// 本地文件系统（写临时文件 / rename / 权限）。
    Io(String),
    /// 下载内容 sha256 与 manifest 声明不符。已删除损坏文件。
    Checksum,
    /// minisign 验签失败：更新内容不可信（manifest 或资产被篡改 / 私钥已轮换），
    /// 已中止并删除半成品。这是最高优先级的失败——绝不降级为「跳过验签继续装」。
    InvalidSignature(String),
    /// 当前平台没有更新通道（CI 只发布四个 key 的资产，如 linux-arm64 会走到这）。
    UnsupportedPlatform,
    /// macOS 专属：当前进程不在 .app bundle 内（cargo run / 裸二进制形态），
    /// 没有可替换的 bundle 目标。引导用户手动下载 .app。
    /// （非 macOS 平台无构造点，cfg_attr 消 dead_code 警告。）
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    NotAppBundle,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Network(e) => write!(f, "网络错误：{}", e),
            UpdateError::Manifest(e) => write!(f, "更新清单无效：{}", e),
            UpdateError::Io(e) => write!(f, "本地文件操作失败：{}", e),
            UpdateError::Checksum => write!(f, "下载内容校验失败（sha256 不匹配），请重试"),
            UpdateError::InvalidSignature(detail) => {
                write!(f, "签名验证失败（{}），更新可能被篡改，已中止", detail)
            }
            UpdateError::UnsupportedPlatform => {
                write!(f, "当前平台暂不支持自动更新，请手动下载")
            }
            UpdateError::NotAppBundle => {
                write!(f, "非 .app 安装形态，不支持自动更新，请手动下载 .app")
            }
        }
    }
}

impl std::error::Error for UpdateError {}

// ===== 纯逻辑（单测覆盖）=====

/// 当前平台的 manifest key。与 release.yml 的资产命名一一对应：
/// `windows-x86_64` / `linux-x86_64` 是裸二进制，`macos-*-app` 是 .app.zip。
/// 其他平台（如 linux-arm64）返回 None → 检查时报 UnsupportedPlatform。
#[allow(unreachable_code)]
pub fn platform_key() -> Option<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some("windows-x86_64");
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some("macos-aarch64-app");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Some("macos-x86_64-app");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some("linux-x86_64");
    }
    None
}

/// latest 是否比 current 新。容忍 `v` 前缀（GitHub tag 惯例）。
/// 任一侧解析失败按「不新」处理——宁可漏一次更新提示，也不能把坏版本号当新版推给用户。
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn parse_version(v: &str) -> Option<semver::Version> {
    semver::Version::parse(v.trim().trim_start_matches('v')).ok()
}

pub fn parse_manifest(body: &str) -> Result<UpdateManifest, UpdateError> {
    let m: UpdateManifest = serde_json::from_str(body)
        .map_err(|e| UpdateError::Manifest(format!("JSON 解析失败：{}", e)))?;
    if parse_version(&m.version).is_none() {
        return Err(UpdateError::Manifest(format!("version 字段非法：{:?}", m.version)));
    }
    Ok(m)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

// ===== 验签（纯逻辑，单测覆盖）=====

/// minisign 公钥文件（标准两行：comment + base64）→ 可验证的 PublicKey。
/// 取最后一个非空行作为 base64——公钥文件格式由我们自己的 keygen 工具产出，
/// 但保持宽松解析以兼容标准 minisign CLI 生成的文件。
fn public_key_from_file_content(content: &str) -> Result<minisign_verify::PublicKey, UpdateError> {
    let b64 = content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| UpdateError::InvalidSignature("公钥文件为空".into()))?;
    minisign_verify::PublicKey::from_base64(b64.trim())
        .map_err(|e| UpdateError::InvalidSignature(format!("公钥解析失败：{}", e)))
}

/// 用给定公钥列表验签（任一通过即可——轮换过渡期新旧公钥并存）。
/// keys 是公钥文件全文的数组；测试注入临时密钥，生产走 [`UPDATE_PUBKEYS`]。
fn verify_with_keys(data: &[u8], sig_text: &str, keys: &[&str]) -> Result<(), UpdateError> {
    let sig_text = sig_text.trim();
    if sig_text.is_empty() {
        return Err(UpdateError::InvalidSignature("manifest / 资产缺少签名字段".into()));
    }
    let signature = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| UpdateError::InvalidSignature(format!("签名格式非法：{}", e)))?;
    // 公钥预解析（而非循环内反复解析）：内嵌公钥格式坏 = 程序自身错误，显式失败
    let public_keys: Vec<_> = keys
        .iter()
        .map(|k| public_key_from_file_content(k))
        .collect::<Result<Vec<_>, _>>()?;
    for pk in &public_keys {
        // 第三个参数 allow_legacy：接受旧版 minisign 的非 prehash 签名。
        // 我们的 keygen/sign 工具产出的就是当前格式，但放开 legacy 无害且
        // 兼容开发者用系统 minisign CLI 手动补签的场景。
        if pk.verify(data, &signature, true).is_ok() {
            return Ok(());
        }
    }
    Err(UpdateError::InvalidSignature("全部内嵌公钥验证失败".into()))
}

/// 生产入口：用编译期内嵌的公钥验签。
fn verify_signature(data: &[u8], sig_text: &str) -> Result<(), UpdateError> {
    verify_with_keys(data, sig_text, UPDATE_PUBKEYS)
}

// ===== IO 动作（同步阻塞，调用方包 spawn_blocking）=====

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

/// 404 有专门的语义（尚无已发布 release / 忘了 publish），单独映射。
fn map_http_error(e: ureq::Error) -> UpdateError {
    match &e {
        ureq::Error::StatusCode(s) if *s == 404 => UpdateError::Manifest(
            "更新清单不存在（尚无已发布的 release，或 release 未 publish）".into(),
        ),
        _ => UpdateError::Network(format!("{}", e)),
    }
}

fn io_err(ctx: &str) -> impl Fn(std::io::Error) -> UpdateError + '_ {
    move |e| UpdateError::Io(format!("{}：{}", ctx, e))
}

/// 信封解析 + 验签 + 内层解析（check_latest 与 verify_manifest example 共用，
/// 也是发布后 smoke 自检走到的同一条路径）。
pub fn parse_signed_manifest(body: &str) -> Result<UpdateManifest, UpdateError> {
    let signed: SignedManifest = serde_json::from_str(body).map_err(|e| {
        UpdateError::Manifest(format!("信封解析失败（应为 {{payload, signature}}）：{}", e))
    })?;
    // 验签先于一切内容解析——payload 在验签通过前是不可信输入。
    verify_signature(signed.payload.as_bytes(), &signed.signature)?;
    parse_manifest(&signed.payload)
}

/// 拉取 manifest（信封验签 → parse 内层）并与当前版本比较。
pub fn check_latest(current: &str) -> Result<UpdateOutcome, UpdateError> {
    let agent = http_agent(CHECK_TIMEOUT);
    let body = agent
        .get(MANIFEST_URL)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(map_http_error)?
        .body_mut()
        .read_to_string()
        .map_err(|e| UpdateError::Network(format!("读取 manifest 失败：{}", e)))?;

    let manifest = parse_signed_manifest(&body)?;
    let key = platform_key().ok_or(UpdateError::UnsupportedPlatform)?;
    let asset = manifest
        .platforms
        .get(key)
        .ok_or_else(|| UpdateError::Manifest(format!("manifest 缺当前平台（{}）的资产", key)))?
        .clone();

    if is_newer(&manifest.version, current) {
        Ok(UpdateOutcome::Available(ReleaseInfo {
            version: manifest.version,
            notes_url: manifest.notes_url,
            asset,
        }))
    } else {
        Ok(UpdateOutcome::UpToDate)
    }
}

/// 流式下载资产到 `dest`，边下边算 sha256，完成后与 manifest 声明对比。
/// 失败时删除半成品文件，不留垃圾。
pub fn download_and_verify(
    asset: &PlatformAsset,
    dest: &Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), UpdateError> {
    if asset.size > MAX_DOWNLOAD {
        return Err(UpdateError::Manifest(format!(
            "资产声明大小 {} 字节超过安全上限",
            asset.size
        )));
    }

    let agent = http_agent(DOWNLOAD_TIMEOUT);
    let mut resp = agent
        .get(&asset.url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(map_http_error)?;

    let file = std::fs::File::create(dest).map_err(io_err("创建临时文件"))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;

    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| UpdateError::Network(format!("下载中断：{}", e)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        std::io::Write::write_all(&mut writer, &buf[..n]).map_err(io_err("写临时文件"))?;
        done += n as u64;
        if done > MAX_DOWNLOAD {
            // 实际字节数超限：manifest 声明被伪造小、实际内容超大，立即止损。
            let _ = std::fs::remove_file(dest);
            return Err(UpdateError::Manifest(format!(
                "下载内容超过安全上限 {} 字节",
                MAX_DOWNLOAD
            )));
        }
        on_progress(done, asset.size);
    }
    std::io::Write::flush(&mut writer).map_err(io_err("刷写临时文件"))?;

    let actual = to_hex(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(asset.sha256.trim()) {
        let _ = std::fs::remove_file(dest);
        return Err(UpdateError::Checksum);
    }

    // 真实性别：minisign 验签资产字节。读回整文件验证（受 MAX_DOWNLOAD 上限
    // 约束，峰值内存可控）；验签失败绝不继续安装。
    let data = std::fs::read(dest).map_err(io_err("读回下载内容"))?;
    if let Err(e) = verify_signature(&data, &asset.signature) {
        let _ = std::fs::remove_file(dest);
        return Err(e);
    }
    Ok(())
}

// ===== 平台安装 =====

/// 把已下载并通过校验的资产安装到位。下载形态因平台而异（见模块注释），
/// 这是全模块唯一的平台分歧入口。
pub fn install_update(downloaded: &Path) -> Result<(), UpdateError> {
    #[cfg(target_os = "macos")]
    return install_app_bundle(downloaded);
    #[cfg(not(target_os = "macos"))]
    return install_binary(downloaded);
}

/// Windows / Linux：下载的就是最终二进制，self-replace 负责「替换运行中的自身」。
#[cfg(not(target_os = "macos"))]
fn install_binary(new_bin: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    set_executable(new_bin)?;
    self_replace::self_replace(new_bin)
        .map_err(|e| UpdateError::Io(format!("替换二进制失败：{}", e)))
}

/// 下载落盘的文件默认无执行位，Linux 上直接 rename 过去会导致新进程 spawn 失败。
/// Windows 上这个函数整个被 cfg 掉（权限位语义不同，无操作可做）。
#[cfg(unix)]
fn set_executable(p: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)
        .map_err(io_err("读取临时文件属性"))?
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(p, perm).map_err(io_err("设置执行位"))
}

/// macOS：整包替换 .app。步骤：解压到 bundle 同目录的 staging（同卷保证 rename
/// 不跨设备）→ 旧 bundle 挪到 backup → 新 bundle 就位 → 失败则回滚。
/// 运行中的进程不受影响（可执行文件按 inode 引用，旧 bundle 被挪走/删除都无碍）。
#[cfg(target_os = "macos")]
fn install_app_bundle(archive: &Path) -> Result<(), UpdateError> {
    let app = find_current_app_bundle().ok_or(UpdateError::NotAppBundle)?;
    let parent = app
        .parent()
        .ok_or_else(|| UpdateError::Io("bundle 没有父目录".into()))?;
    let staging = parent.join(format!(".qrctrl-update-{}", std::process::id()));
    let backup = parent.join(format!(".qrctrl-old-{}", std::process::id()));
    // 清理历史失败留下的 staging（.qrctrl-update-* 不带 pid 匹配——能走到本轮
    // 安装说明留下它的进程已不在。backup（.qrctrl-old-*）不动：可能仍被旧进程
    // 的可执行文件引用着，删了等于杀进程）。
    if let Ok(rd) = std::fs::read_dir(parent) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.file_name().to_string_lossy().starts_with(".qrctrl-update-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    extract_zip(archive, &staging)?;
    let new_app = locate_app_bundle(&staging)
        .ok_or_else(|| UpdateError::Manifest("更新包里找不到 .app bundle".into()))?;

    std::fs::rename(&app, &backup).map_err(io_err("备份旧 bundle"))?;
    match std::fs::rename(&new_app, &app) {
        Ok(()) => {
            // 清理是 best-effort：失败只留一个隐藏垃圾目录，不影响更新结果。
            let _ = std::fs::remove_dir_all(&backup);
            let _ = std::fs::remove_dir_all(&staging);
            Ok(())
        }
        Err(e) => {
            let err = UpdateError::Io(format!("新 bundle 就位失败：{}", e));
            match std::fs::rename(&backup, &app) {
                Ok(()) => Err(err),
                Err(rb) => Err(UpdateError::Io(format!(
                    "{}；且回滚失败（{}），请手动重新下载 .app",
                    err, rb
                ))),
            }
        }
    }
}

/// 从 current_exe 向上找 `.app` 祖先（`qrctrl.app/Contents/MacOS/qrctrl` → `qrctrl.app`）。
/// 找不到 = cargo run / 裸二进制形态，不支持自更新。
#[cfg(target_os = "macos")]
fn find_current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// 解压后定位 .app 根：优先 `staging/qrctrl.app`（CI `ditto -k --keepParent` 的
/// zip 根就是它）；否则兜底找 staging 下唯一的 `.app` 目录。认 `Contents/MacOS`
/// 子目录防误判。
#[cfg(target_os = "macos")]
fn locate_app_bundle(staging: &Path) -> Option<PathBuf> {
    let direct = staging.join("qrctrl.app");
    if direct.join("Contents").join("MacOS").is_dir() {
        return Some(direct);
    }
    let candidates: Vec<PathBuf> = std::fs::read_dir(staging)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "app")
                && p.join("Contents").join("MacOS").is_dir()
        })
        .collect();
    candidates.into_iter().next()
}

/// 安全解压 zip：`enclosed_name()` 拒绝 `..` / 绝对路径逃逸（zip 是外部输入，
/// 条目路径不可信），unix 权限位按 zip 记录恢复，主程序强制 0755（zip 记录
/// 缺失或被打平时丢失执行位时兜底，否则新 bundle 无法启动）。
#[cfg(target_os = "macos")]
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;

    let f = std::fs::File::open(archive).map_err(io_err("打开更新包"))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| UpdateError::Manifest(format!("更新包损坏：{}", e)))?;
    std::fs::create_dir_all(dest).map_err(io_err("创建解压目录"))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| UpdateError::Manifest(format!("读取更新包条目失败：{}", e)))?;
        let mode = entry.unix_mode();
        let Some(rel) = entry.enclosed_name() else {
            continue; // 路径逃逸的条目直接跳过，不落地
        };
        // 符号链接条目 fail-closed：我们的 .app bundle 不含 symlink（binary +
        // plist + 图标），出现即包内容与预期不符（格式漂移或被动手脚），
        // 拒绝整个更新而不是「跳过该条目装个残缺 bundle」。
        if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(UpdateError::Manifest(format!(
                "更新包含符号链接条目（{}），预期 .app 内无链接，拒绝安装",
                rel.to_string_lossy()
            )));
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(io_err("创建目录"))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(io_err("创建父目录"))?;
        }
        let mut file = std::fs::File::create(&out).map_err(io_err("解压文件"))?;
        std::io::copy(&mut entry, &mut file).map_err(io_err("解压文件"))?;
        let mode = if rel.ends_with(Path::new("Contents/MacOS/qrctrl")) {
            0o755
        } else {
            entry.unix_mode().map(|m| m & 0o777).unwrap_or(0o644)
        };
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode))
            .map_err(io_err("恢复文件权限"))?;
    }
    Ok(())
}

// ===== 状态机 =====

/// 更新流程状态。直接 Serialize 给前端（GET /api/update/status 的 phase 字段），
/// `tag = "state"` 让前端按 `phase.state` 分支渲染。
#[derive(Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdatePhase {
    /// 启动后还没检查过（更新仅手动触发，见 spawn_check）。
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
        notes_url: String,
        size: u64,
        /// 最新版恰好是用户跳过的版本：前端显示「已跳过」并提供取消跳过入口。
        skipped: bool,
    },
    Downloading {
        version: String,
        bytes: u64,
        total: u64,
    },
    Installing {
        version: String,
    },
    /// 新版本已就位，等前端触发 POST /api/restart。
    RestartPending {
        version: String,
    },
    Failed {
        error: String,
    },
}

/// phase 与「最近一次 Available 的资产」放同一把锁里，避免两个 Mutex 的
/// 获取顺序问题。skip_version 单独一把（它的读写方不会与 inner 交叉持锁，
/// 见各方法内的锁顺序注释）。
struct UpdaterInner {
    phase: UpdatePhase,
    pending: Option<ReleaseInfo>,
}

/// 更新状态机 + 后台任务编排。
///
/// 挂在 AppState（不是 CoreState）——更新是进程级动作而非协议业务，
/// `ws::dispatch` 的单测不该被迫构造它（CoreState 保持「纯业务核心」的边界）。
///
/// 所有网络/磁盘动作都在 `tokio::task::spawn_blocking` 里跑，tao event loop 和
/// tokio worker 都不被阻塞。
pub struct Updater {
    inner: Mutex<UpdaterInner>,
    skip_version: Mutex<Option<String>>,
}

impl Updater {
    pub fn new(skip_version: Option<String>) -> Self {
        Self {
            inner: Mutex::new(UpdaterInner {
                phase: UpdatePhase::Idle,
                pending: None,
            }),
            skip_version: Mutex::new(skip_version),
        }
    }

    /// GET /api/update/status 的响应体。
    pub fn status_json(&self) -> serde_json::Value {
        json!({
            "current": env!("CARGO_PKG_VERSION"),
            "phase": self.inner.lock().phase,
        })
    }

    /// 当前 skip_version 的克隆（api.rs 持久化时读取比较用）。
    pub fn skip_version(&self) -> Option<String> {
        self.skip_version.lock().clone()
    }

    /// 更新「跳过版本」（内存）。持久化由 api.rs 走 load-覆盖-save（与 theme 的
    /// live-apply 特例同模式）。锁顺序：skip_version 的 guard 不跨 inner.lock()
    /// 持有，与 run_check 一致，无交叉死锁面。
    pub fn set_skip_version(&self, v: Option<String>) {
        let skip = v.clone();
        *self.skip_version.lock() = v;
        let mut inner = self.inner.lock();
        if let UpdatePhase::Available { version, skipped, .. } = &mut inner.phase {
            *skipped = skip.as_deref() == Some(version.as_str());
        }
    }

    /// spawn 后台检查（POST /api/update/check 的后端，唯一入口——更新只手动触发）。
    /// 已在进行类状态（Checking / Downloading / Installing / RestartPending）时
    /// 是 no-op——前端连点「检查更新」不会叠出并发请求。
    pub fn spawn_check(self: &Arc<Self>) {
        let can_start = {
            let mut inner = self.inner.lock();
            let busy = matches!(
                inner.phase,
                UpdatePhase::Checking
                    | UpdatePhase::Downloading { .. }
                    | UpdatePhase::Installing { .. }
                    | UpdatePhase::RestartPending { .. }
            );
            if !busy {
                inner.phase = UpdatePhase::Checking;
            }
            !busy
        };
        if can_start {
            let this = self.clone();
            tokio::task::spawn_blocking(move || this.run_check());
        }
    }

    fn run_check(&self) {
        // skip 的 guard 当行释放，再进 inner——两把锁不嵌套。
        let skip = self.skip_version.lock().clone();
        let outcome = check_latest(env!("CARGO_PKG_VERSION"));
        let mut inner = self.inner.lock();
        match outcome {
            Ok(UpdateOutcome::UpToDate) => {
                inner.phase = UpdatePhase::UpToDate;
                inner.pending = None;
            }
            Ok(UpdateOutcome::Available(info)) => {
                let skipped = skip.as_deref() == Some(info.version.as_str());
                inner.phase = UpdatePhase::Available {
                    version: info.version.clone(),
                    notes_url: info.notes_url.clone(),
                    size: info.asset.size,
                    skipped,
                };
                inner.pending = Some(info);
            }
            Err(e) => {
                // 前端 failed 态只显示固定文案「获取新版本失败」，详情只进日志
                tracing::warn!("检查更新失败：{}", e);
                inner.phase = UpdatePhase::Failed {
                    error: e.to_string(),
                }
            }
        }
    }

    /// spawn 后台安装（POST /api/update/install）。只在 Available 状态可触发，
    /// 其他状态返回错误文案（api 转 400 给前端）。
    pub fn spawn_install(self: &Arc<Self>) -> Result<(), String> {
        let (asset, version) = {
            let mut inner = self.inner.lock();
            match inner.pending.take() {
                Some(info) if matches!(inner.phase, UpdatePhase::Available { .. }) => {
                    inner.phase = UpdatePhase::Downloading {
                        version: info.version.clone(),
                        bytes: 0,
                        total: info.asset.size,
                    };
                    (info.asset, info.version)
                }
                _ => return Err("当前没有可安装的更新（请先检查更新）".into()),
            }
        };
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.run_install(asset, version));
        Ok(())
    }

    fn run_install(&self, asset: PlatformAsset, version: String) {
        let dest = std::env::temp_dir().join(format!("qrctrl-update-{}.tmp", std::process::id()));
        // 进度节流：下载循环每 64KB 回调一次，逐次拿锁太频繁；按 1 MB 步进 + 收尾
        // 必达一次更新 phase。
        let mut last_report: u64 = 0;
        let result = download_and_verify(&asset, &dest, |done, total| {
            if done - last_report >= 1024 * 1024 || done >= total {
                last_report = done;
                self.inner.lock().phase = UpdatePhase::Downloading {
                    version: version.clone(),
                    bytes: done,
                    total,
                };
            }
        });
        let result = result.and_then(|()| {
            self.inner.lock().phase = UpdatePhase::Installing {
                version: version.clone(),
            };
            install_update(&dest)
        });
        // 临时文件收尾：Windows/Linux 上 self-replace 通常已把文件移动/消费掉，
        // 删除失败（NotFound）属预期；macOS 上是 zip 残壳。
        let _ = std::fs::remove_file(&dest);

        let mut inner = self.inner.lock();
        inner.phase = match result {
            Ok(()) => UpdatePhase::RestartPending { version },
            Err(e) => {
                // 同 run_check：前端只见固定文案，详情进日志
                tracing::warn!("安装更新失败：{}", e);
                UpdatePhase::Failed {
                    error: e.to_string(),
                }
            }
        };
    }
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;

    /// 整段一次性 hash 的便捷封装。生产路径（download_and_verify）是流式增量
    /// hash，不需要它——只用于测试已知向量。
    fn sha256_hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        to_hex(&h.finalize())
    }

    #[test]
    fn platform_key_matches_ci_assets() {
        // 当前编译平台必须有 key——否则 CI 发布了资产、客户端却报 UnsupportedPlatform。
        let key = platform_key().expect("当前平台应有 manifest key");
        assert!(key.starts_with("windows-") || key.starts_with("macos-") || key.starts_with("linux-"));
    }

    #[test]
    fn is_newer_handles_versions_and_prefix() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("v1.0.0", "0.9.0"));
        assert!(is_newer("1.0.0", "v0.9.9"));
        assert!(!is_newer("0.9.0", "0.9.0"), "相等不算新");
        assert!(!is_newer("0.8.0", "0.9.0"), "更旧不算新");
        assert!(!is_newer("垃圾", "0.9.0"), "坏版本号按不新处理");
        assert!(!is_newer("0.10.0", "垃圾"));
        // semver 语义：0.10.0 > 0.9.0（按数字段比，不是字符串前缀）
        assert!(is_newer("0.10.0", "0.9.9"));
    }

    #[test]
    fn parse_manifest_accepts_full_shape() {
        let body = r#"{
            "version": "1.2.3",
            "notes_url": "https://example.com/notes",
            "platforms": {
                "windows-x86_64": {"url": "https://e.com/a.exe", "size": 100, "sha256": "aa"}
            }
        }"#;
        let m = parse_manifest(body).expect("合法 manifest");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.notes_url, "https://example.com/notes");
        assert_eq!(m.platforms["windows-x86_64"].size, 100);
    }

    #[test]
    fn parse_manifest_defaults_notes_url() {
        let body = r#"{"version":"1.0.0","platforms":{}}"#;
        let m = parse_manifest(body).expect("notes_url 缺省应容忍");
        assert_eq!(m.notes_url, "");
    }

    #[test]
    fn parse_manifest_rejects_bad_input() {
        assert!(parse_manifest("不是 json").is_err());
        assert!(parse_manifest("{}").is_err(), "缺 version / platforms");
        // version 非法：后续版本比较会全部走 false，等于永远查不到更新——直接报错
        assert!(parse_manifest(r#"{"version":"xyz","platforms":{}}"#).is_err());
    }

    #[test]
    fn sha256_known_vector() {
        // "abc" 的 sha256 是公开测试向量
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    #[test]
    fn skip_version_marks_available_as_skipped() {
        let u = Updater::new(Some("9.9.9".into()));
        // 直接驱动状态：模拟一次 Available 结果
        u.inner.lock().phase = UpdatePhase::Available {
            version: "9.9.9".into(),
            notes_url: String::new(),
            size: 1,
            skipped: false,
        };
        // set_skip_version 应把 skipped 置真
        u.set_skip_version(Some("9.9.9".into()));
        match u.inner.lock().phase {
            UpdatePhase::Available { skipped, .. } => assert!(skipped),
            _ => panic!("set_skip_version 不该改变 phase 的种类"),
        }
        // 取消跳过后恢复
        u.set_skip_version(None);
        match u.inner.lock().phase {
            UpdatePhase::Available { skipped, .. } => assert!(!skipped),
            _ => unreachable!(),
        }
    }

    #[test]
    fn spawn_install_requires_available_phase() {
        let u = Arc::new(Updater::new(None));
        assert!(u.spawn_install().is_err(), "Idle 状态不允许安装");
    }

    // ===== 签名验签（生产代码 verify_with_keys + 测试密钥对）=====
    // 密钥对生成 / 签名用 dev-dep 的 minisign（examples 同款），验证端走生产
    // 代码路径——测的就是客户端真实会执行的逻辑。

    fn test_keypair() -> (String, minisign::SecretKey) {
        // 测试用无加密密钥对：sign 要求解密态的 sk，加密只影响私钥落盘形态，
        // 对验证路径（被测对象）无差别。
        let minisign::KeyPair { pk, sk } = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (pk.to_box().unwrap().to_string(), sk)
    }

    fn test_sign(sk: &minisign::SecretKey, data: &[u8]) -> String {
        minisign::sign(None, sk, data, None, None).unwrap().to_string()
    }

    #[test]
    fn signature_roundtrip_accepts_valid() {
        let (pk, sk) = test_keypair();
        let data = b"{\"version\":\"1.0.0\"}";
        let sig = test_sign(&sk, data);
        verify_with_keys(data, &sig, &[&pk]).expect("合法签名应通过");
    }

    #[test]
    fn signature_rejects_tampered_payload() {
        let (pk, sk) = test_keypair();
        let sig = test_sign(&sk, b"original");
        let err = verify_with_keys(b"tampered!", &sig, &[&pk])
            .expect_err("篡改后的数据必须验签失败");
        assert!(matches!(err, UpdateError::InvalidSignature(_)), "{}", err);
    }

    #[test]
    fn signature_rejects_wrong_key() {
        let (pk_a, _) = test_keypair();
        let (_, sk_b) = test_keypair();
        let sig = test_sign(&sk_b, b"data");
        assert!(verify_with_keys(b"data", &sig, &[&pk_a]).is_err());
    }

    #[test]
    fn signature_rejects_empty_or_garbage() {
        let (pk, _) = test_keypair();
        assert!(verify_with_keys(b"data", "", &[&pk]).is_err(), "空签名");
        assert!(verify_with_keys(b"data", "not a sig", &[&pk]).is_err(), "垃圾签名");
    }

    #[test]
    fn signature_accepts_key_rotation_list() {
        // 轮换过渡期：公钥列表 = [旧(不匹配), 新(匹配)]，任一通过即可
        let (old_pk, _) = test_keypair();
        let (new_pk, sk) = test_keypair();
        let sig = test_sign(&sk, b"payload");
        verify_with_keys(b"payload", &sig, &[&old_pk, &new_pk]).expect("多公钥列表应命中第二把");
    }

    #[test]
    fn signed_envelope_full_path_without_network() {
        // 模拟真实 manifest 的完整接收路径：外层信封 JSON → 验签 payload →
        // parse 内层 → 取平台资产（不联网，等价于 check_latest 的后半段）。
        let (pk, sk) = test_keypair();
        let payload = serde_json::json!({
            "version": "9.9.9",
            "notes_url": "https://example.com",
            "platforms": {
                "windows-x86_64": {
                    "url": "https://example.com/a.exe",
                    "size": 3,
                    "sha256": "aa",
                    "signature": "should-not-matter-here"
                }
            }
        })
        .to_string();
        let envelope = serde_json::json!({
            "payload": payload,
            "signature": test_sign(&sk, payload.as_bytes()),
        })
        .to_string();

        // 生产路径：信封解析 → 验签 → 内层解析 → 取资产
        let signed: SignedManifest = serde_json::from_str(&envelope).unwrap();
        verify_with_keys(signed.payload.as_bytes(), &signed.signature, &[&pk]).unwrap();
        let manifest = parse_manifest(&signed.payload).unwrap();
        assert_eq!(manifest.version, "9.9.9");
        assert!(manifest.platforms.contains_key("windows-x86_64"));

        // 篡改信封里的 payload（哪怕只改一个字符）必须整体拒绝
        let bad = envelope.replace("9.9.9", "9.9.8");
        let signed: SignedManifest = serde_json::from_str(&bad).unwrap();
        assert!(verify_with_keys(signed.payload.as_bytes(), &signed.signature, &[&pk]).is_err());
    }

    /// 联网测试（默认忽略，本地 `cargo test -- --ignored` 跑）：
    /// 验证 GitHub `releases/latest/download` 的 302 重定向跟随 + manifest
    /// 下载整条链路。404（尚无已发布 release）也算链路通——只断言不出现
    /// 网络/重定向层错误。
    #[test]
    #[ignore = "需要网络访问 GitHub"]
    fn network_manifest_fetch_reaches_github() {
        match check_latest(env!("CARGO_PKG_VERSION")) {
            Ok(_) => {}
            Err(UpdateError::Network(e)) => panic!("重定向或网络层失败：{}", e),
            Err(e) => println!("仓库当前状态：{}", e),
        }
    }
}
