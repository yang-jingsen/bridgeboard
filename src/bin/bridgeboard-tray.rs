#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("bridgeboard-tray.exe is a Windows tray entrypoint.");
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    win_tray::run()
}

#[cfg(windows)]
mod win_tray {
    use bridgeboard::core::{self, BridgeEnv};
    use bridgeboard::dashboard::{self, DashboardEnv};
    use std::mem::size_of;
    use std::net::{TcpStream, ToSocketAddrs};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::Once;
    use std::time::Duration;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, LoadImageW, PostQuitMessage,
        RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, CS_HREDRAW,
        CS_VREDRAW, CW_USEDEFAULT, HICON, HWND_MESSAGE, IDI_APPLICATION, IMAGE_ICON,
        LR_LOADFROMFILE, MF_SEPARATOR, MF_STRING, MSG, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
        WINDOW_STYLE, WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
    };

    const WM_TRAYICON: u32 = WM_APP + 1;
    const TRAY_ID: u32 = 1;
    const ID_OPEN_BRIDGEBOARD: usize = 1001;
    const ID_OPEN_DASHBOARD: usize = 1002;
    const ID_PORTS: usize = 1003;
    const ID_DOCTOR: usize = 1004;
    const ID_QUIT: usize = 1005;
    const DASHBOARD_URL: &str = "http://127.0.0.1:24000/";
    static DASHBOARD_SERVER: Once = Once::new();
    static LIFECYCLE_WORKER: Once = Once::new();

    pub fn run() -> windows::core::Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hinstance = HINSTANCE(instance.0);
            let class_name = w!("BridgeboardTrayWindow");
            let wc = WNDCLASSW {
                hInstance: hinstance,
                lpszClassName: class_name,
                lpfnWndProc: Some(wnd_proc),
                style: CS_HREDRAW | CS_VREDRAW,
                ..Default::default()
            };
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("Bridgeboard Tray"),
                WINDOW_STYLE::default(),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinstance),
                None,
            )?;

            add_tray_icon(hwnd);
            start_dashboard_server();
            start_lifecycle_worker();

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).into() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            delete_tray_icon(hwnd);
        }
        Ok(())
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAYICON => {
                let event = lparam.0 as u32;
                if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xffff;
                match id {
                    ID_OPEN_BRIDGEBOARD => open_bridgeboard(),
                    ID_OPEN_DASHBOARD => open_dashboard(),
                    ID_PORTS => open_terminal(&["ports", "--peers"]),
                    ID_DOCTOR => open_terminal(&["doctor"]),
                    ID_QUIT => {
                        delete_tray_icon(hwnd);
                        PostQuitMessage(0);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                delete_tray_icon(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn add_tray_icon(hwnd: HWND) {
        let Ok(fallback_icon) = LoadIconW(None, IDI_APPLICATION) else {
            return;
        };
        let icon = load_bridgeboard_icon().unwrap_or(fallback_icon);
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };
        fill_wstr(&mut nid.szTip, "Bridgeboard");
        let _ = Shell_NotifyIconW(NIM_ADD, &mut nid);
    }

    unsafe fn delete_tray_icon(hwnd: HWND) {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &mut nid);
    }

    unsafe fn show_menu(hwnd: HWND) {
        let menu = match CreatePopupMenu() {
            Ok(menu) => menu,
            Err(_) => return,
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_BRIDGEBOARD, w!("Open Bridgeboard"));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN_DASHBOARD, w!("Open Web Dashboard"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_PORTS, w!("Ports"));
        let _ = AppendMenuW(menu, MF_STRING, ID_DOCTOR, w!("Doctor"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));

        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None);
        }
        let _ = DestroyMenu(menu);
    }

    fn open_dashboard() {
        start_dashboard_server();
        shell_open(DASHBOARD_URL);
    }

    fn open_bridgeboard() {
        if let Some(path) = sibling_exe("bridgeboard-ui.exe") {
            if Command::new(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok()
            {
                return;
            }
        }
        open_dashboard();
    }

    fn start_dashboard_server() {
        DASHBOARD_SERVER.call_once(|| {
            if dashboard_is_up() {
                return;
            }
            std::thread::spawn(|| match DashboardEnv::discover() {
                Ok(env) => {
                    if let Err(err) = dashboard::serve(env, "127.0.0.1", 24000, true) {
                        eprintln!("bridgeboard dashboard server stopped: {err}");
                    }
                }
                Err(err) => eprintln!("bridgeboard dashboard config failed: {err}"),
            });
            for _ in 0..20 {
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
        let Ok(mut addrs) = "127.0.0.1:24000".to_socket_addrs() else {
            return false;
        };
        let Some(addr) = addrs.next() else {
            return false;
        };
        TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
    }

    fn open_terminal(args: &[&str]) {
        let bin = bridgeboard_bin();
        let command = format!(
            "& '{}' {}",
            escape_ps(&bin),
            args.iter()
                .map(|arg| quote_ps_arg(arg))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let _ = Command::new("powershell")
            .args(["-NoExit", "-Command", &command])
            .stdin(Stdio::null())
            .spawn();
    }

    fn shell_open(target: &str) {
        let wide = to_wide(target);
        unsafe {
            let _ = ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
        }
    }

    fn bridgeboard_bin() -> String {
        if let Ok(value) = std::env::var("BRIDGEBOARD_BIN") {
            if !value.trim().is_empty() {
                return value;
            }
        }
        if let Ok(path) = std::env::current_exe() {
            if let Some(dir) = path.parent() {
                let candidate = dir.join("bridgeboard.exe");
                if candidate.exists() {
                    return candidate.display().to_string();
                }
            }
        }
        "bridgeboard".to_string()
    }

    fn sibling_exe(name: &str) -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let candidate = dir.join(name);
        candidate.exists().then_some(candidate)
    }

    fn icon_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let candidate = dir.join("bridgeboard.ico");
        candidate.exists().then_some(candidate)
    }

    unsafe fn load_bridgeboard_icon() -> Option<HICON> {
        let path = icon_path()?;
        let wide = to_wide(&path.display().to_string());
        let handle = LoadImageW(
            None,
            PCWSTR(wide.as_ptr()),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE,
        )
        .ok()?;
        Some(HICON(handle.0))
    }

    fn escape_ps(value: &str) -> String {
        value.replace('\'', "''")
    }

    fn quote_ps_arg(value: &str) -> String {
        if value.chars().any(char::is_whitespace) {
            format!("'{}'", escape_ps(value))
        } else {
            value.to_string()
        }
    }

    fn fill_wstr<const N: usize>(target: &mut [u16; N], value: &str) {
        for (idx, code) in value.encode_utf16().take(N.saturating_sub(1)).enumerate() {
            target[idx] = code;
        }
    }

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
