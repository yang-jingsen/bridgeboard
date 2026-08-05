#![cfg_attr(windows, windows_subsystem = "windows")]

use bridgeboard::core::{self, BridgeEnv};
use bridgeboard::dashboard;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Once;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const DASHBOARD_URL: &str = "http://127.0.0.1:24000/";
static DASHBOARD_SERVER: Once = Once::new();
static LIFECYCLE_WORKER: Once = Once::new();

fn main() {
    let app = tauri::Builder::default()
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            start_dashboard_server();
            start_lifecycle_worker();
            build_tray(app.handle())?;
            show_bridgeboard(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("build Bridgeboard Tauri app");

    app.run(|_app, event| {
        if let RunEvent::ExitRequested { code, api, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open_i = MenuItem::with_id(app, "open", "Open Bridgeboard", true, None::<&str>)?;
    let browser_i = MenuItem::with_id(app, "browser", "Open Web Dashboard", true, None::<&str>)?;
    let ports_i = MenuItem::with_id(app, "ports", "Ports", true, None::<&str>)?;
    let doctor_i = MenuItem::with_id(app, "doctor", "Doctor", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep_1 = PredefinedMenuItem::separator(app)?;
    let sep_2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_i, &browser_i, &sep_1, &ports_i, &doctor_i, &sep_2, &quit_i,
        ],
    )?;

    let mut builder = TrayIconBuilder::new()
        .tooltip("Bridgeboard")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_bridgeboard(app),
            "browser" => {
                start_dashboard_server();
                let _ = webbrowser::open(DASHBOARD_URL);
            }
            "ports" => show_bridgeboard(app),
            "doctor" => show_bridgeboard(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_bridgeboard(&tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn show_bridgeboard(app: &AppHandle) {
    start_dashboard_server();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let Ok(url) = DASHBOARD_URL.parse() else {
        return;
    };
    let _ = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
        .title("Bridgeboard")
        .inner_size(1680.0, 980.0)
        .min_inner_size(1180.0, 680.0)
        .resizable(true)
        .build();
}

fn start_dashboard_server() {
    DASHBOARD_SERVER.call_once(|| {
        if dashboard_is_up() {
            return;
        }
        std::thread::spawn(|| match BridgeEnv::discover() {
            Ok(env) => {
                if let Err(err) = dashboard::serve(env, "127.0.0.1", 24000, true) {
                    eprintln!("bridgeboard dashboard server stopped: {err}");
                }
            }
            Err(err) => eprintln!("bridgeboard dashboard config failed: {err}"),
        });
        for _ in 0..30 {
            if dashboard_is_up() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

fn start_lifecycle_worker() {
    LIFECYCLE_WORKER.call_once(|| {
        std::thread::spawn(|| match BridgeEnv::discover() {
            Ok(env) => {
                if let Err(err) = core::startup(&env, false) {
                    eprintln!("bridgeboard startup failed: {err}");
                }
                loop {
                    std::thread::sleep(Duration::from_secs(15));
                    if let Err(err) = core::supervise_once(&env) {
                        eprintln!("bridgeboard supervise failed: {err}");
                    }
                }
            }
            Err(err) => eprintln!("bridgeboard lifecycle config failed: {err}"),
        });
    });
}

fn dashboard_is_up() -> bool {
    ("127.0.0.1", 24000)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .and_then(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(200)).ok())
        .is_some()
}
