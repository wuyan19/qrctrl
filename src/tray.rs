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

use std::rc::Rc;
use std::sync::Arc;

use tao::dpi::PhysicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use tokio::sync::Notify;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

use crate::qr;

pub struct TrayState {
    pub device_name: String,
    pub url: String,
}

enum UserEvent {
    #[allow(dead_code)]
    TrayIconEvent(TrayIconEvent),
    MenuEvent(MenuEvent),
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
}

pub fn run_tray_event_loop(state: TrayState, shutdown_notify: Arc<Notify>) {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

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
    let quit_i = MenuItem::new("退出", true, None);
    let _ = menu.append_items(&[
        &copy_url_i,
        &show_qr_i,
        &PredefinedMenuItem::separator(),
        &quit_i,
    ]);

    // tray_icon 必须保持 owned 直到 event loop 结束，否则图标会消失。
    let mut tray_icon: Option<tray_icon::TrayIcon> = None;
    let mut qr_window: Option<QrWindowState> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let icon = load_icon();
                tray_icon = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_tooltip(format!("qrctrl · {}", state.device_name))
                        .with_icon(icon)
                        .build()
                        .expect("tray icon build"),
                );
            }
            Event::UserEvent(UserEvent::MenuEvent(e)) => {
                if e.id == copy_url_i.id() {
                    let url = state.url.clone();
                    std::thread::spawn(move || {
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            let _ = cb.set_text(url);
                        }
                    });
                } else if e.id == show_qr_i.id() {
                    if let Some(w) = qr_window.as_ref() {
                        w.window.set_focus();
                    } else {
                        match open_qr_window(target, &state.url) {
                            Ok(w) => qr_window = Some(w),
                            Err(e) => eprintln!("[tray] 显示二维码失败: {}", e),
                        }
                    }
                } else if e.id == quit_i.id() {
                    shutdown_notify.notify_waiters();
                    *control_flow = ControlFlow::Exit;
                }
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
            Event::RedrawRequested(window_id) => {
                if let Some(w) = qr_window.as_mut() {
                    if window_id == w.window.id() {
                        if let Err(e) = draw_qr(w) {
                            eprintln!("[tray] QR 重绘失败: {}", e);
                        }
                    }
                }
            }
            _ => {}
        }
    });
}

fn open_qr_window(
    target: &EventLoopWindowTarget<UserEvent>,
    url: &str,
) -> Result<QrWindowState, String> {
    let scale = 12u32;
    let border = 4u32;
    let (pixels, pixel_w, pixel_h) = qr::render_qr_to_pixels(url, scale, border)?;

    let window = Rc::new(
        WindowBuilder::new()
            .with_title("扫描连接 qrctrl")
            .with_inner_size(PhysicalSize::new(pixel_w, pixel_h))
            .with_resizable(false)
            .build(target)
            .map_err(|e| format!("window build: {}", e))?,
    );

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

    window.request_redraw();

    Ok(QrWindowState {
        window,
        context,
        surface,
        pixels,
        pixel_w,
        pixel_h,
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
