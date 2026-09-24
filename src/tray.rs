//! 系统托盘：常驻后台 + 菜单（复制 URL / 显示二维码 / 退出）。
//!
//! 跨平台约束（来自 tray-icon 文档）：
//! - macOS: NSApplication 事件循环必须跑在主线程，tray icon 也必须在主线程创建。
//! - Windows/Linux: 事件循环和 tray icon 必须同线程。
//! 所以这里跑在 main 线程，server 的 tokio runtime 跑在子线程。
//!
//! 与 server 线程的协调：退出菜单触发 `Notify::notify_waiters()`，
//! server 端在 `axum::serve(...).with_graceful_shutdown(notify.notified())` 上等。

// tray_icon 变量赋值后从未读取，编译器误报 unused。它真正的作用是「持有」，让
// TrayIcon 在 event loop 期间不被 drop（drop 会让图标消失）。整个 event_loop.run
// closure 不返回（tao 的 run 是 `-> !`），所以变量无法被显式 read 或 drop。
#![allow(unused_assignments, unused_variables)]

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::dpi::PhysicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS};
use tokio::sync::Notify;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

use crate::net;
use crate::qr;

/// QR 窗口的渲染参数（物理像素；实际值按窗口 scale_factor 放大，见 `qr_module_scale`）。
const QR_MODULE_SCALE: u32 = 12;
const QR_BORDER_MODULES: u32 = 4;
/// QR 窗口打开期间每 3 秒重新枚举一次网卡，主机 IP 变化（DHCP 续租 /
/// 切换 Wi-Fi）时自动重绘二维码和 URL，无需重启进程。
const IP_REFRESH_INTERVAL: Duration = Duration::from_secs(3);

pub struct TrayState {
    pub device_name: String,
    /// 扫码 URL 的 token。URL 不再启动时算死——显示/复制时实时枚举网卡构造。
    pub token: String,
    pub port: u16,
    pub prefer_ip: Option<String>,
    /// 手机上传文件的保存目录，托盘菜单「打开文件保存目录」点开它。
    pub save_dir: PathBuf,
    /// 双击启动（无 console、无 banner）时为 true，tray 初始化后自动弹 QR 码窗口。
    pub auto_show_qr: bool,
}

impl TrayState {
    /// 实时构造当前扫码 URL（每次调用重新枚举网卡）。
    fn current_url(&self) -> String {
        net::build_scan_url(self.prefer_ip.as_deref(), self.port, &self.token)
    }
}

pub enum UserEvent {
    #[allow(dead_code)]
    TrayIconEvent(TrayIconEvent),
    MenuEvent(MenuEvent),
    /// 配置页「立即重启」按钮发起：server 线程通过 EventLoopProxy 发送，
    /// tray event loop 收到后通知 server shutdown + 自己 Exit，main 退出后 spawn 新进程。
    RestartRequested,
}

// QrWindowState 用 Rc<Window> 让 Context/Surface 都拥有 Window 的引用计数。
// 这样 struct 整体是 'static（不依赖 EventLoopWindowTarget 的生命周期）。
struct QrWindowState {
    window: Rc<Window>,
    #[allow(dead_code)]
    context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    pixels: Vec<u32>,
    pixel_w: u32,
    pixel_h: u32,
    /// 当前窗口展示的 URL。IP 定时检查发现 `current_url()` 与之不同时触发重绘。
    url: String,
}

pub fn run_tray_event_loop(
    state: TrayState,
    shutdown_notify: Arc<Notify>,
    // mut 仅 macOS 需要（set_activation_policy 是 &mut self），其他平台会触发 unused_mut
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))] mut event_loop: EventLoop<UserEvent>,
) {
    // macOS: 强制 Accessory + 隐藏 Dock。LSUIElement=true 只在 plist 启动阶段生效，
    // tao 的 launched() 会按内部默认（Regular）调 NSApp.setActivationPolicy，
    // 不显式覆盖就会冒出 Dock 图标——右键 Dock Quit 会发 terminate: 杀掉整个托盘进程。
    // 这里改 tao 内部状态，让 launched() 时 setActivationPolicy(Accessory)；
    // window 创建后再用 set_activation_policy_at_runtime 强推一次（见 open_qr_window）。
    #[cfg(target_os = "macos")]
    {
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_dock_visibility(false);
    }

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::TrayIconEvent(e));
    }));

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::MenuEvent(e));
    }));

    let menu = Menu::new();
    let copy_url_i = MenuItem::new("复制 URL", true, None);
    let show_qr_i = MenuItem::new("显示二维码", true, None);
    let open_save_dir_i = MenuItem::new("打开文件保存目录", true, None);
    let clear_clipboard_i = MenuItem::new("清空剪切板", true, None);
    let config_i = MenuItem::new("配置", true, None);
    let quit_i = MenuItem::new("退出", true, None);
    let _ = menu.append_items(&[
        &copy_url_i,
        &show_qr_i,
        &open_save_dir_i,
        &clear_clipboard_i,
        &config_i,
        &PredefinedMenuItem::separator(),
        &quit_i,
    ]);

    // tray_icon 必须保持 owned 直到 event loop 结束，否则图标会消失。
    let mut tray_icon: Option<tray_icon::TrayIcon> = None;
    let mut qr_window: Option<QrWindowState> = None;
    // QR 窗口打开期间的下次 IP 检查时刻（WaitUntil 用）
    let mut next_ip_check = Instant::now();

    event_loop.run(move |event, target, control_flow| {
        // QR 窗口开着时定时醒来检查 IP 变化；否则长眠等用户事件。
        *control_flow = if qr_window.is_some() {
            ControlFlow::WaitUntil(next_ip_check)
        } else {
            ControlFlow::Wait
        };
        match event {
            Event::NewEvents(StartCause::Init) => {
                let icon = load_icon();
                tray_icon = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_tooltip(format!("QR Control · {}", state.device_name))
                        .with_icon(icon)
                        .build()
                        .expect("tray icon build"),
                );

                // 双击启动（无 console、无 banner）时自动弹 QR 码窗口，
                // 让用户立刻能扫。从 PowerShell/terminal 启动时 banner 已有，不重复弹。
                if state.auto_show_qr {
                    match open_qr_window(target, &state.current_url()) {
                        Ok(w) => {
                            qr_window = Some(w);
                            next_ip_check = Instant::now() + IP_REFRESH_INTERVAL;
                        }
                        Err(e) => tracing::warn!("自动显示二维码失败: {}", e),
                    }
                }
            }
            // WaitUntil 到点：QR 窗口开着，重新枚举网卡看地址是否变化。
            // 没变化时仅重置闹钟，不重绘（重绘有可感知的窗口调整风险）。
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                next_ip_check = Instant::now() + IP_REFRESH_INTERVAL;
                if let Some(w) = qr_window.as_mut() {
                    let url = state.current_url();
                    if url != w.url {
                        tracing::info!("网络地址已变化，二维码更新为 {}", url);
                        if let Err(e) = refresh_qr_window(w, &url) {
                            tracing::warn!("二维码自动刷新失败: {}", e);
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::MenuEvent(e)) => {
                if e.id == copy_url_i.id() {
                    let url = state.current_url();
                    std::thread::spawn(move || {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            let _ = cb.set_text(url);
                        }
                    });
                } else if e.id == show_qr_i.id() {
                    let url = state.current_url();
                    match qr_window.as_mut() {
                        // 已开着：先同步最新地址（可能 IP 已变），再聚焦
                        Some(w) => {
                            if url != w.url {
                                if let Err(e) = refresh_qr_window(w, &url) {
                                    tracing::warn!("二维码刷新失败: {}", e);
                                }
                            }
                            w.window.set_focus();
                        }
                        None => match open_qr_window(target, &url) {
                            Ok(w) => {
                                qr_window = Some(w);
                                // 窗口重开时闹钟可能早已过期（WaitUntil 过去时刻会立即
                                // 触发一次多余的网卡枚举），重置回整 3 秒节流
                                next_ip_check = Instant::now() + IP_REFRESH_INTERVAL;
                            }
                            Err(e) => tracing::warn!("显示二维码失败: {}", e),
                        },
                    }
                } else if e.id == open_save_dir_i.id() {
                    let save_dir = state.save_dir.clone();
                    std::thread::spawn(move || open_in_file_manager(&save_dir));
                } else if e.id == clear_clipboard_i.id() {
                    // 清空剪贴板。arboard::Clipboard 不跨线程共享，照「复制 URL」
                    // 的模式在子线程里开新句柄、用完即弃；clear() 不区分格式地
                    // 清掉文本/图片/文件引用。
                    std::thread::spawn(|| {
                        match arboard::Clipboard::new().and_then(|mut cb| cb.clear()) {
                            Ok(()) => tracing::info!("剪贴板已清空"),
                            Err(e) => tracing::warn!("清空剪贴板失败: {}", e),
                        }
                    });
                } else if e.id == config_i.id() {
                    // 配置页 URL 由 net 模块统一构造（在扫码 URL 的 ?t= 前插 /config）
                    let config_url =
                        net::build_config_url(state.prefer_ip.as_deref(), state.port, &state.token);
                    std::thread::spawn(move || open_url_in_browser(&config_url));
                } else if e.id == quit_i.id() {
                    shutdown_notify.notify_waiters();
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::RestartRequested) => {
                // 配置页「立即重启」按钮通过 EventLoopProxy 发来。
                // tao 的 event_loop.run() 标记为 `-> !`：ControlFlow::Exit 后
                // Windows 直接 ExitProcess，macOS/Linux 也类似，run() 调用之后
                // 的 main 代码不会执行——所以 spawn 新进程必须放在这里。
                //
                // 时序：spawn 新进程 → notify server shutdown → 本进程 Exit
                // 新进程启动后会和本进程争端口（本进程 listener 还没 drop），
                // 通过 QRCTRL_RESTART_CHILD 环境变量让 probe_port 重试几次绑定。
                let exe = std::env::current_exe()
                    .unwrap_or_else(|_| std::path::PathBuf::from("qrctrl"));
                // 沿用本进程的 CLI 参数（--port / --token / --save-dir 等都透传），
                // 让重启后的进程行为与本进程一致；config.toml 中的改动也会生效。
                let mut cmd = std::process::Command::new(&exe);
                cmd.args(std::env::args().skip(1));
                // env::set_var 在 2024 edition 是 unsafe，改用 Command::env 注入子进程
                cmd.env("QRCTRL_RESTART_CHILD", "1");
                if let Err(e) = cmd.spawn() {
                    tracing::error!("重启 spawn 失败（{}），请手动启动", e);
                }
                shutdown_notify.notify_waiters();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested { .. },
                window_id,
                ..
            } => {
                if let Some(w) = qr_window.as_ref() {
                    if window_id == w.window.id() {
                        qr_window.take();
                    }
                }
            }
            // 拖到不同缩放比例的显示器：按新 DPI 重渲染二维码并调整窗口尺寸，
            // 否则从大模块屏拖到小模块屏 QR 会被裁剪、反向则四周白边变大。
            // 此刻 window.scale_factor() 已是新值，refresh_qr_window 直接读到。
            Event::WindowEvent {
                event: WindowEvent::ScaleFactorChanged { .. },
                window_id,
                ..
            } => {
                if let Some(w) = qr_window.as_mut() {
                    if window_id == w.window.id() {
                        let url = w.url.clone();
                        if let Err(e) = refresh_qr_window(w, &url) {
                            tracing::warn!("DPI 变化后二维码重渲染失败: {}", e);
                        }
                    }
                }
            }
            Event::RedrawRequested(window_id) => {
                if let Some(w) = qr_window.as_mut() {
                    if window_id == w.window.id() {
                        if let Err(e) = draw_qr(w) {
                            tracing::warn!("QR 重绘失败: {}", e);
                        }
                    }
                }
            }
            _ => {}
        }
    });
}

/// 按窗口 scale_factor 换算 QR 模块的物理像素数（HiDPI 屏上二维码同步放大，
/// 视觉尺寸各屏一致）。最低 1，防 0。
fn qr_module_scale(scale_factor: f64) -> u32 {
    ((QR_MODULE_SCALE as f64) * scale_factor).round().max(1.0) as u32
}

/// 按新 URL 重渲染已开窗口的二维码（IP 变化时刷新用）。标题同步换成新 URL，
/// 用户手动输入时照着标题抄就行，不用等二维码渲染完。
fn refresh_qr_window(state: &mut QrWindowState, url: &str) -> Result<(), String> {
    let module_scale = qr_module_scale(state.window.scale_factor());
    let (pixels, w, h) = qr::render_qr_to_pixels(url, module_scale, QR_BORDER_MODULES)?;
    state.pixels = pixels;
    state.pixel_w = w;
    state.pixel_h = h;
    state.url = url.to_string();
    state.window.set_title(url);
    state.window.set_inner_size(PhysicalSize::new(w, h));
    state.window.request_redraw();
    Ok(())
}

fn open_qr_window(
    target: &EventLoopWindowTarget<UserEvent>,
    url: &str,
) -> Result<QrWindowState, String> {
    // 先不可见 + 占位尺寸建窗口，读到真实 scale_factor 后按 DPI 渲染二维码、
    // 调整到最终尺寸再显示——避免先闪一个小窗再跳变。
    // always_on_top：浮动层级，扫码时不会被其他应用的窗口盖住
    // （macOS 上尤其关键：Accessory 后台应用不主动前置就会开在前台应用之下）。
    let window = Rc::new(
        WindowBuilder::new()
            // 标题就是扫码 URL：窗口本身就是展示这个地址用的，标题栏照抄
            // 方便用户手动输入（IP 变化时 refresh_qr_window 会同步更新）。
            .with_title(url)
            .with_visible(false)
            .with_always_on_top(true)
            .with_inner_size(PhysicalSize::new(200u32, 200u32))
            .with_resizable(false)
            .with_window_icon(load_window_icon())
            .build(target)
            .map_err(|e| format!("window build: {}", e))?,
    );

    // macOS: window 创建可能让 NSApp 重置激活策略到 Regular（Dock 图标再次出现），
    // 强推回 Accessory。每次开 QR 窗口都调一次，开销可忽略。
    #[cfg(target_os = "macos")]
    target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);

    let module_scale = qr_module_scale(window.scale_factor());
    let (pixels, pixel_w, pixel_h) =
        qr::render_qr_to_pixels(url, module_scale, QR_BORDER_MODULES)?;
    window.set_inner_size(PhysicalSize::new(pixel_w, pixel_h));

    let context =
        softbuffer::Context::new(Rc::clone(&window)).map_err(|e| format!("context: {}", e))?;
    let mut surface = softbuffer::Surface::new(&context, Rc::clone(&window))
        .map_err(|e| format!("surface: {}", e))?;

    let size = window.inner_size();
    let init_w = std::num::NonZeroU32::new(size.width.max(1) as u32).unwrap();
    let init_h = std::num::NonZeroU32::new(size.height.max(1) as u32).unwrap();
    surface
        .resize(init_w, init_h)
        .map_err(|e| format!("resize: {}", e))?;

    window.set_visible(true);
    // 打开即前置：置顶层级保证不被盖住，set_focus 再把窗口抬到最前并成为
    // key window（用户扫码时第一时间看得到）。
    window.set_focus();
    window.request_redraw();

    Ok(QrWindowState {
        window,
        context,
        surface,
        pixels,
        pixel_w,
        pixel_h,
        url: url.to_string(),
    })
}

fn draw_qr(state: &mut QrWindowState) -> Result<(), String> {
    let size = state.window.inner_size();
    let width = size.width as u32;
    let height = size.height as u32;
    if width == 0 || height == 0 {
        return Ok(());
    }

    let nz_w = std::num::NonZeroU32::new(width.max(1)).unwrap();
    let nz_h = std::num::NonZeroU32::new(height.max(1)).unwrap();
    state
        .surface
        .resize(nz_w, nz_h)
        .map_err(|e| format!("resize: {}", e))?;
    let mut buffer = state
        .surface
        .buffer_mut()
        .map_err(|e| format!("buffer: {}", e))?;

    // 整窗口白底（QR 内部已有静默区，但窗口比 QR 大时填白更稳）
    buffer.fill(0xFFFFFFFF);

    let qr_w = state.pixel_w;
    let qr_h = state.pixel_h;
    let off_x = width.saturating_sub(qr_w) / 2;
    let off_y = height.saturating_sub(qr_h) / 2;
    let copy_w = qr_w.min(width.saturating_sub(off_x));
    let copy_h = qr_h.min(height.saturating_sub(off_y));

    for py in 0..copy_h {
        for px in 0..copy_w {
            let src = state.pixels[(py * qr_w + px) as usize];
            let dst = (off_y + py) * width + (off_x + px);
            buffer[dst as usize] = src;
        }
    }

    buffer.present().map_err(|e| format!("present: {}", e))?;
    Ok(())
}

fn load_icon() -> tray_icon::Icon {
    let bytes = include_bytes!("../assets/tray-icon.png");
    let img = image::load_from_memory(bytes)
        .expect("load tray-icon.png")
        .into_rgba8();
    let (w, h) = img.dimensions();
    let rgba = img.into_raw();
    tray_icon::Icon::from_rgba(rgba, w, h).expect("icon from rgba")
}

/// QR 弹窗的窗口图标 + 任务栏图标。返回 Option 是为了解码失败时让窗口仍能创建——
/// `WindowBuilder::with_window_icon(None)` 是合法值（tao 会回退到默认图标）。
/// 实践中 `assets/icon.png` 由我们控制，不会失败；用 expect 会让 tray 应用在
/// 双击启动（无 console）下 silent crash，不可接受。
fn load_window_icon() -> Option<tao::window::Icon> {
    let bytes = include_bytes!("../assets/icon.png");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let rgba = img.into_raw();
    tao::window::Icon::from_rgba(rgba, w, h).ok()
}

/// 在系统文件管理器里打开 path。失败只打日志，不弹错误（tray 应用没 UI 兜底，
/// 且 save_dir 在 main 里已经同步创建过，失败基本意味着系统层面问题）。
fn open_in_file_manager(path: &std::path::Path) {
    // macOS: open、Windows: explorer、Linux: xdg-open —— 都是系统自带或主流发行版标配
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";

    if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
        tracing::warn!("打开 {} 失败: {}", path.display(), e);
    }
}

/// 用系统默认浏览器打开 URL。失败只打日志——tray 应用没 UI 兜底。
/// Windows 用 `cmd /C start "" <url>`：start 的第一个引号字符串当窗口标题，
/// 没它会把 URL 当文件路径解析；占位 "" 避免这个坑。
/// 同时设 `CREATE_NO_WINDOW` 避免父进程（windows_subsystem = "windows"）下
/// spawn 的 cmd 子进程闪一个控制台黑窗。
fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = std::process::Command::new("open").arg(url).spawn() {
            tracing::warn!("打开 {} 失败: {}", url, e);
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW = 0x08000000。父进程是 windows subsystem 时，子进程
        // 默认会创建一个新的控制台；这个 flag 让 cmd 静默执行后立即退出。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if let Err(e) = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            tracing::warn!("打开 {} 失败: {}", url, e);
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
            tracing::warn!("打开 {} 失败: {}", url, e);
        }
    }
}
