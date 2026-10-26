// SPDX-License-Identifier: GPL-3.0-or-later
//! Native window (tao + wry) and tray (tray-icon). See ADR 0003.
//!
//! Event-loop design: one tao `EventLoop<UserEvent>` on the main thread, driven with `run_return`
//! so `run` can shut the agent down afterwards. Everything else feeds it through an
//! `EventLoopProxy`: a status-watch thread, a shell-event thread, tray/menu callbacks, the
//! webview IPC handler and the autostart toggle worker.

#[cfg(target_os = "linux")]
mod desktop_entry;
#[cfg(target_os = "linux")]
mod native_drop;

use crate::app::{AgentHandle, AgentStatus, ShellEvent};
use nd_config::{ConfigBackend, SettingsPatch, Theme as CfgTheme};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::{Icon as WinIcon, Theme, Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon as TrayImg, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
#[cfg(windows)]
use windows::core::{w, HSTRING};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder};

const ICON_PNG: &[u8] = include_bytes!("../../res/icon.png");
const DARK_BG: (u8, u8, u8, u8) = (0x16, 0x18, 0x1B, 0xFF);
const LIGHT_BG: (u8, u8, u8, u8) = (0xF4, 0xF5, 0xF7, 0xFF);

#[derive(Debug)]
enum UserEvent {
    Status(AgentStatus),
    Show,
    Hide,
    Quit,
    SetTheme(CfgTheme),
    Menu(MenuId),
    TrayClick,
    /// Result of the "Start at login" toggle: the state the check item should show.
    AutostartResult(bool),
    /// Script that hands a file dropped on the Linux window to the page.
    #[cfg(target_os = "linux")]
    FileDrop(String),
}

/// Run the window + tray event loop on the calling (main) thread until the user quits,
/// then shut the agent down. `show_window` is false for `--minimized` / autostart.
pub fn run(handle: AgentHandle, show_window: bool) -> anyhow::Result<()> {
    // Wayland app_id / X11 WM_CLASS come from GTK's program name; set it before GTK initialises.
    #[cfg(target_os = "linux")]
    {
        gtk::glib::set_prgname(Some(desktop_entry::APP_ID));
        gtk::glib::set_application_name("Notideck");
        desktop_entry::ensure();
    }
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let ui_url = handle.ui_url();
    let settings = handle.settings();
    let backend: Arc<dyn ConfigBackend> = handle.backend.clone();
    let mut status_rx = handle.status();
    let initial_status = status_rx.borrow_and_update().clone();

    spawn_forwarders(&handle, proxy.clone());
    install_tray_handlers(proxy.clone());

    let (icon_rgba, icon_w, icon_h) = decode_icon();
    let window_icon = WinIcon::from_rgba(icon_rgba.clone(), icon_w, icon_h).ok();

    let theme = theme_of(settings.theme);
    let bg = bg_for(settings.theme, theme);
    let window = WindowBuilder::new()
        .with_title("Notideck")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(1024.0, 680.0))
        .with_window_icon(window_icon)
        .with_theme(theme)
        .with_background_color(bg)
        .with_visible(show_window)
        .build(&event_loop)?;

    let mut web_context = WebContext::new(Some(webview_dir()));
    let mut webview: Option<WebView> = match build_webview(&window, &mut web_context, &ui_url, bg, proxy.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::error!("webview creation failed: {e}");
            #[cfg(windows)]
            message_box(&format!(
                "Notideck could not start its window because the Microsoft Edge WebView2 \
                     runtime is missing or failed to start.\n\nInstall it from:\n\
                     https://developer.microsoft.com/microsoft-edge/webview2/\n\n\
                     Notideck keeps running in the tray; \"Open Notideck\" opens the interface \
                     in your browser instead.\n\nDetails: {e}"
            ));
            #[cfg(not(windows))]
            message_box(&format!(
                "Notideck could not start its window (webkit2gtk): {e}. It keeps running in the \
                 tray; \"Open Notideck\" opens the interface in your browser instead."
            ));
            None
        }
    };

    // Tray + menu (created once the loop starts).
    let status_item = MenuItem::new(status_text(&initial_status), false, None);
    let open_item = MenuItem::new("Open Notideck", true, None);
    let autostart_item = CheckMenuItem::new("Start at login", true, settings.autostart, None);
    let quit_item = MenuItem::new("Quit", true, None);
    let open_id = open_item.id().clone();
    let autostart_id = autostart_item.id().clone();
    let quit_id = quit_item.id().clone();
    let mut tray: Option<TrayIcon> = None;

    let mut handle = Some(handle);
    let mut quitting = false;
    let ui_url_ext = ui_url.clone();

    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {
                if show_window {
                    show_window_focused(&window);
                }
                if tray.is_none() {
                    match build_tray(
                        &status_item,
                        &open_item,
                        &autostart_item,
                        &quit_item,
                        &icon_rgba,
                        icon_w,
                        icon_h,
                        &initial_status,
                    ) {
                        Ok(t) => tray = Some(t),
                        Err(e) => tracing::error!("tray creation failed: {e}"),
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => window.set_visible(false),
            Event::UserEvent(ev) => match ev {
                UserEvent::Status(s) => {
                    status_item.set_text(status_text(&s));
                    if let Some(t) = &tray {
                        let _ = t.set_tooltip(Some(format!("Notideck \u{2014} {}", tooltip_status(&s))));
                    }
                }
                UserEvent::Show | UserEvent::TrayClick => {
                    if webview.is_some() {
                        show_window_focused(&window);
                    } else {
                        open_external(&ui_url_ext);
                    }
                }
                UserEvent::Hide => window.set_visible(false),
                UserEvent::SetTheme(t) => {
                    let th = theme_of(t);
                    window.set_theme(th);
                    let bg = bg_for(t, th);
                    window.set_background_color(Some(bg));
                    if let Some(w) = &webview {
                        let _ = w.set_background_color(bg);
                    }
                }
                UserEvent::Menu(id) => {
                    if id == open_id {
                        if webview.is_some() {
                            show_window_focused(&window);
                        } else {
                            open_external(&ui_url_ext);
                        }
                    } else if id == autostart_id {
                        let want = autostart_item.is_checked();
                        let backend = backend.clone();
                        let proxy = proxy.clone();
                        std::thread::spawn(move || {
                            let patch = SettingsPatch { autostart: Some(want), ..Default::default() };
                            let shown = match backend.update_settings(patch) {
                                Ok(s) => s.autostart,
                                Err(e) => {
                                    tracing::warn!("autostart toggle failed: {e}");
                                    !want
                                }
                            };
                            let _ = proxy.send_event(UserEvent::AutostartResult(shown));
                        });
                    } else if id == quit_id {
                        let _ = proxy.send_event(UserEvent::Quit);
                    }
                }
                UserEvent::AutostartResult(on) => autostart_item.set_checked(on),
                #[cfg(target_os = "linux")]
                UserEvent::FileDrop(script) => {
                    if let Some(wv) = &webview {
                        if let Err(e) = wv.evaluate_script(&script) {
                            tracing::warn!("cannot pass the dropped file to the page: {e}");
                        }
                    }
                }
                UserEvent::Quit => {
                    if !quitting {
                        quitting = true;
                        webview = None;
                        tray = None;
                        window.set_visible(false);
                        *flow = ControlFlow::Exit;
                    }
                }
            },
            _ => {}
        }
    });

    drop(webview);
    drop(web_context);
    drop(tray);
    if let Some(h) = handle.take() {
        h.shutdown();
    }
    Ok(())
}

fn spawn_forwarders(handle: &AgentHandle, proxy: EventLoopProxy<UserEvent>) {
    let mut status = handle.status();
    let p = proxy.clone();
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        rt.block_on(async move {
            while status.changed().await.is_ok() {
                let s = status.borrow_and_update().clone();
                if p.send_event(UserEvent::Status(s)).is_err() {
                    break;
                }
            }
        });
    });
    let mut shell = handle.shell_events();
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        rt.block_on(async move {
            loop {
                match shell.recv().await {
                    // The tray stack has no toast API: bring the window up so Allow/Deny is visible.
                    Ok(ShellEvent::Show | ShellEvent::PairingPending(_)) => {
                        if proxy.send_event(UserEvent::Show).is_err() {
                            break;
                        }
                    }
                    Ok(ShellEvent::Quit) => {
                        if proxy.send_event(UserEvent::Quit).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    });
}

fn install_tray_handlers(proxy: EventLoopProxy<UserEvent>) {
    let p = Mutex::new(proxy.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        let hit = matches!(
            e,
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
        );
        if hit {
            if let Ok(p) = p.lock() {
                let _ = p.send_event(UserEvent::TrayClick);
            }
        }
    }));
    let p = Mutex::new(proxy);
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Ok(p) = p.lock() {
            let _ = p.send_event(UserEvent::Menu(e.id));
        }
    }));
}

#[allow(clippy::too_many_arguments)]
fn build_tray(
    status_item: &MenuItem,
    open: &MenuItem,
    autostart: &CheckMenuItem,
    quit: &MenuItem,
    rgba: &[u8],
    w: u32,
    h: u32,
    status: &AgentStatus,
) -> anyhow::Result<TrayIcon> {
    let menu = Menu::new();
    menu.append_items(&[
        status_item,
        &PredefinedMenuItem::separator(),
        open,
        autostart,
        &PredefinedMenuItem::separator(),
        quit,
    ])?;
    let icon = TrayImg::from_rgba(rgba.to_vec(), w, h)?;
    Ok(TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(icon)
        .with_tooltip(format!("Notideck \u{2014} {}", tooltip_status(status)))
        .build()?)
}

fn build_webview(
    window: &Window,
    ctx: &mut WebContext,
    ui_url: &str,
    bg: (u8, u8, u8, u8),
    proxy: EventLoopProxy<UserEvent>,
) -> wry::Result<WebView> {
    let prefix = allowed_prefix(ui_url);
    let nav_prefix = prefix.clone();
    #[cfg(target_os = "linux")]
    let drop_proxy = proxy.clone();
    let ipc_proxy = proxy;
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut builder = WebViewBuilder::new_with_web_context(ctx)
        .with_url(ui_url)
        .with_background_color(bg)
        .with_initialization_script(format!(
            "window.__NOTIDECK_SHELL__ = {{ version: \"{}\" }};",
            env!("CARGO_PKG_VERSION")
        ))
        .with_devtools(cfg!(debug_assertions))
        .with_navigation_handler(move |url| {
            if url.starts_with(&nav_prefix) || url == "about:blank" {
                true
            } else {
                open_external(&url);
                false
            }
        })
        .with_new_window_req_handler(|url, _| {
            open_external(&url);
            NewWindowResponse::Deny
        })
        .with_ipc_handler(move |req| handle_ipc(req.body(), &ipc_proxy));
    #[cfg(windows)]
    {
        use wry::WebViewBuilderExtWindows;
        builder = builder.with_default_context_menus(cfg!(debug_assertions));
    }
    // No drag-drop handler on purpose: WebView2 then keeps its native handling, so dropping
    // files onto <input type=file> works.
    #[cfg(windows)]
    {
        builder.build(window)
    }
    // WebKitGTK gives the page no File objects for a file dropped from a file manager (only
    // text/uri-list), so read dropped images here and pass them in; other drops stay with WebKit.
    #[cfg(target_os = "linux")]
    {
        builder = builder.with_drag_drop_handler(move |ev| match ev {
            wry::DragDropEvent::Drop { paths, position } => match native_drop::drop_script(&paths, position) {
                Some(script) => {
                    let _ = drop_proxy.send_event(UserEvent::FileDrop(script));
                    true
                }
                None => false,
            },
            _ => false,
        });
    }
    // GTK child widget: also works under Wayland, where there is no raw window handle to attach to.
    #[cfg(not(windows))]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or(wry::Error::MessageSender)?;
        builder.build_gtk(vbox)
    }
}

fn handle_ipc(body: &str, proxy: &EventLoopProxy<UserEvent>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return };
    match v.get("cmd").and_then(|c| c.as_str()) {
        Some("open_external") => {
            if let Some(url) = v.get("url").and_then(|u| u.as_str()) {
                open_external(url);
            }
        }
        Some("hide") => {
            let _ = proxy.send_event(UserEvent::Hide);
        }
        Some("theme") => {
            let t = match v.get("value").and_then(|t| t.as_str()) {
                Some("dark") => CfgTheme::Dark,
                Some("light") => CfgTheme::Light,
                Some("system") => CfgTheme::System,
                _ => return,
            };
            let _ = proxy.send_event(UserEvent::SetTheme(t));
        }
        _ => {}
    }
}

/// Open http/https/mailto URLs in the system browser; everything else is ignored.
fn open_external(url: &str) {
    let l = url.to_ascii_lowercase();
    if !(l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:")) {
        tracing::warn!("refusing to open external url with unsupported scheme");
        return;
    }
    match nd_platform::native::launcher() {
        Ok(launcher) => {
            if let Err(e) = launcher.open_url(url) {
                tracing::warn!("open_url failed: {e}");
            }
        }
        Err(e) => tracing::warn!("no launcher: {e}"),
    }
}

fn allowed_prefix(ui_url: &str) -> String {
    let rest = ui_url.strip_prefix("http://127.0.0.1:").unwrap_or("");
    let port: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    format!("http://127.0.0.1:{port}/")
}

fn show_window_focused(window: &Window) {
    window.set_visible(true);
    window.set_minimized(false);
    #[cfg(windows)]
    {
        raise_window(window);
    }
    window.set_focus();
}

#[cfg(windows)]
fn raise_window(window: &Window) {
    // Windows refuses SetForegroundWindow to background processes (e.g. after a slow WebView2
    // cold start the user clicked elsewhere). A topmost toggle still raises the window above
    // others; set_focus then succeeds whenever Windows allows it.
    window.set_always_on_top(true);
    window.set_always_on_top(false);
}

fn theme_of(t: CfgTheme) -> Option<Theme> {
    match t {
        CfgTheme::Dark => Some(Theme::Dark),
        CfgTheme::Light => Some(Theme::Light),
        CfgTheme::System => None,
    }
}

fn bg_for(t: CfgTheme, th: Option<Theme>) -> (u8, u8, u8, u8) {
    match (t, th) {
        (CfgTheme::Light, _) => LIGHT_BG,
        // System: dark default avoids a white flash; the page repaints with its own theme.
        _ => DARK_BG,
    }
}

fn status_text(s: &AgentStatus) -> String {
    let mut t = match s.devices.len() {
        0 => "No phone connected".to_string(),
        1 => {
            let d = &s.devices[0];
            let tr = if d.transport.eq_ignore_ascii_case("usb") { "USB" } else { "LAN" };
            format!("{} \u{b7} {tr}", d.name)
        }
        n => format!("{n} phones connected"),
    };
    let p = s.problems.len();
    if p > 0 {
        t.push_str(&format!(" \u{26a0} {p} issue{}", if p == 1 { "" } else { "s" }));
    }
    t
}

fn tooltip_status(s: &AgentStatus) -> String {
    status_text(s)
}

#[cfg(not(windows))]
fn webview_dir() -> PathBuf {
    crate::instance::data_dir().join("webview")
}

#[cfg(windows)]
fn webview_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Notideck").join("webview")
}

fn decode_icon() -> (Vec<u8>, u32, u32) {
    fn inner() -> Option<(Vec<u8>, u32, u32)> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = dec.read_info().ok()?;
        let mut buf = vec![0; r.output_buffer_size()?];
        let info = r.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
            png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return None,
        };
        Some((rgba, info.width, info.height))
    }
    inner().unwrap_or_else(|| {
        tracing::warn!("icon.png could not be decoded; using a placeholder");
        ([0x9A, 0xB8, 0x7A, 0xFF].repeat(16 * 16), 16, 16)
    })
}

#[cfg(not(windows))]
fn message_box(text: &str) {
    tracing::error!("{text}");
}

#[cfg(windows)]
fn message_box(text: &str) {
    // SAFETY: plain modal message box with no owner window.
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("Notideck"), MB_OK | MB_ICONERROR);
    }
}
