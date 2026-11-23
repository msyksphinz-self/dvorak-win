//! 常駐アプリ本体: タスクトレイ・ホットキー・前面アプリ監視・電源/セッション監視

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::util::*;
use super::{autostart, hook};
use crate::config::{self, Config};

const CLASS_NAME: &str = "DvorakWinTrayWindow";
const MUTEX_NAME: &str = "Local\\DvorakWin-SingleInstance";
const WM_TRAY: u32 = WM_APP + 10;
const TRAY_ID: u32 = 1;
const HOTKEY_ID: i32 = 1;
const TIMER_WATCH: usize = 1;

const ID_TOGGLE: usize = 1;
const ID_OPEN_CONFIG: usize = 2;
const ID_RELOAD: usize = 3;
const ID_AUTOSTART: usize = 4;
const ID_RESTART_ADMIN: usize = 5;
const ID_EXIT: usize = 9;

struct App {
    hwnd: HWND,
    cfg: Config,
    icon_on: HICON,
    icon_off: HICON,
    msg_taskbar_created: u32,
    elevated: bool,
    hotkey_ok: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn load_config(show_warnings: bool) -> Config {
    let path = config_path();
    if !path.exists() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF]; // メモ帳で文字化けしないよう BOM 付き UTF-8
        bytes.extend_from_slice(config::default_text().as_bytes());
        let _ = std::fs::write(&path, bytes);
    }
    let text = match std::fs::read(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => return Config::default(),
    };
    let (cfg, warnings) = config::parse(&text);
    if show_warnings && !warnings.is_empty() {
        message_box(
            &format!("設定ファイルに問題があります (該当行は無視します):\n\n{}", warnings.join("\n")),
            MB_ICONWARNING,
        );
    }
    cfg
}

unsafe fn make_icon(bg: u32, fg: u32) -> HICON {
    let size = GetSystemMetrics(SM_CXSMICON).max(16);
    let screen = GetDC(null_mut());
    let dc = CreateCompatibleDC(screen);
    let color = CreateCompatibleBitmap(screen, size, size);
    let mask = CreateBitmap(size, size, 1, 1, null());
    let old_bmp = SelectObject(dc, color);
    let mut rc = RECT { left: 0, top: 0, right: size, bottom: size };
    let brush = CreateSolidBrush(bg);
    FillRect(dc, &rc, brush);
    DeleteObject(brush);
    SetBkMode(dc, TRANSPARENT as _);
    SetTextColor(dc, fg);
    let face = wide("Segoe UI");
    let font = CreateFontW(
        -(size * 15 / 16),
        0,
        0,
        0,
        FW_BOLD as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        ANTIALIASED_QUALITY as u32,
        0,
        face.as_ptr(),
    );
    let old_font = SelectObject(dc, font);
    let text = wide("D");
    DrawTextW(dc, text.as_ptr(), 1, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(dc, old_font);
    SelectObject(dc, old_bmp);
    DeleteObject(font);
    let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&ii);
    DeleteObject(color);
    DeleteObject(mask);
    DeleteDC(dc);
    ReleaseDC(null_mut(), screen);
    icon
}

fn copy_tip(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

fn status_text(app: &App) -> String {
    let state = if !hook::ENABLED.load(Ordering::Relaxed) {
        "無効"
    } else if hook::EXCLUDED.load(Ordering::Relaxed) {
        "一時停止中 (除外アプリ)"
    } else {
        "有効"
    };
    let admin = if app.elevated { " [管理者]" } else { "" };
    format!("Dvorak-Win: {state}{admin}")
}

fn tray(app: &App, msg: NOTIFY_ICON_MESSAGE) -> bool {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = app.hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        let active = hook::ENABLED.load(Ordering::Relaxed) && !hook::EXCLUDED.load(Ordering::Relaxed);
        nid.hIcon = if active { app.icon_on } else { app.icon_off };
        copy_tip(&mut nid.szTip, &status_text(app));
        Shell_NotifyIconW(msg, &nid) != 0
    }
}

fn refresh_tray(app: &App) {
    // エクスプローラーの再起動などでアイコンが消えていたら追加し直す
    if !tray(app, NIM_MODIFY) {
        tray(app, NIM_ADD);
    }
}

fn register_hotkey(app: &mut App) {
    unsafe {
        UnregisterHotKey(app.hwnd, HOTKEY_ID);
        app.hotkey_ok = match app.cfg.toggle_hotkey {
            Some(h) => RegisterHotKey(app.hwnd, HOTKEY_ID, h.mods | MOD_NOREPEAT, h.vk) != 0,
            None => true,
        };
    }
}

fn update_exclusion(app: &App, hwnd: HWND) {
    let excluded = if app.cfg.exclude.is_empty() || hwnd.is_null() {
        false
    } else {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        process_name(pid).is_some_and(|n| app.cfg.exclude.iter().any(|e| *e == n))
    };
    let prev = hook::EXCLUDED.swap(excluded, Ordering::Relaxed);
    if prev != excluded {
        refresh_tray(app);
    }
}

unsafe extern "system" fn on_foreground(
    _h: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _obj: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    with_app(|app| update_exclusion(app, hwnd));
}

fn toggle(app: &App) {
    hook::ENABLED.fetch_xor(true, Ordering::Relaxed);
    refresh_tray(app);
}

fn reload(app: &mut App) {
    let cfg = load_config(true);
    hook::update_config(cfg.clone());
    app.cfg = cfg;
    register_hotkey(app);
    if !app.hotkey_ok {
        message_box("切り替えホットキーを登録できませんでした (他のアプリが使用中の可能性があります)", MB_ICONWARNING);
    }
    update_exclusion(app, unsafe { GetForegroundWindow() });
    refresh_tray(app);
}

fn run_elevated(args: &str) {
    let exe = wide(&exe_path().to_string_lossy());
    let verb = wide("runas");
    let params = wide(args);
    unsafe {
        ShellExecuteW(null_mut(), verb.as_ptr(), exe.as_ptr(), params.as_ptr(), null(), SW_SHOWNORMAL);
    }
}

fn open_config() {
    let path = config_path();
    if !path.exists() {
        load_config(false);
    }
    let file = wide("notepad.exe");
    let param = wide(&format!("\"{}\"", path.to_string_lossy()));
    let verb = wide("open");
    unsafe {
        ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), param.as_ptr(), null(), SW_SHOWNORMAL);
    }
}

fn toggle_autostart(app: &App) {
    let installed = autostart::is_installed();
    if app.elevated {
        let r = if installed { autostart::uninstall() } else { autostart::install() };
        match r {
            Ok(()) if installed => message_box("自動起動を解除しました。", MB_ICONINFORMATION),
            Ok(()) => message_box(
                "ログオン時に管理者権限で自動起動するよう登録しました。",
                MB_ICONINFORMATION,
            ),
            Err(e) => message_box(&format!("失敗しました: {e}"), MB_ICONERROR),
        }
    } else {
        run_elevated(if installed { "--uninstall-autostart" } else { "--install-autostart" });
    }
}

fn show_menu(app: &mut App) -> usize {
    unsafe {
        let menu = CreatePopupMenu();
        let add = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let w = wide(text);
            AppendMenuW(menu, flags, id, w.as_ptr());
        };
        let check = |b: bool| if b { MF_CHECKED } else { MF_UNCHECKED };

        let status = wide(&status_text(app));
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        let hotkey = if app.cfg.toggle_hotkey.is_some() && app.hotkey_ok { "\t(ホットキー)" } else { "" };
        add(
            MF_STRING | check(hook::ENABLED.load(Ordering::Relaxed)),
            ID_TOGGLE,
            &format!("Dvorak を有効にする{hotkey}"),
        );
        add(MF_STRING, ID_OPEN_CONFIG, "設定ファイルを開く");
        add(MF_STRING, ID_RELOAD, "設定を再読み込み");
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        add(
            MF_STRING | check(autostart::is_installed()),
            ID_AUTOSTART,
            "ログオン時に自動起動 (管理者権限)",
        );
        if !app.elevated {
            add(MF_STRING, ID_RESTART_ADMIN, "管理者権限で再起動");
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        add(MF_STRING, ID_EXIT, "終了");

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // これをしないとメニュー外をクリックしても閉じない (Windows の既知の挙動)
        SetForegroundWindow(app.hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            null(),
        ) as usize;
        PostMessageW(app.hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        cmd
    }
}

/// メニューのコマンド実行 (APP の借用はコマンドごとに取り直す)
fn run_command(hwnd: HWND, cmd: usize) {
    match cmd {
        ID_TOGGLE => {
            with_app(|app| toggle(app));
        }
        ID_OPEN_CONFIG => open_config(),
        ID_RELOAD => {
            with_app(reload);
        }
        ID_AUTOSTART => {
            with_app(|app| toggle_autostart(app));
        }
        ID_RESTART_ADMIN => run_elevated("--replace"),
        ID_EXIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ => {}
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => {
                    with_app(|app| toggle(app));
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    if let Some(cmd) = with_app(show_menu) {
                        run_command(hwnd, cmd);
                    }
                }
                _ => {}
            }
            0
        }
        WM_HOTKEY => {
            if wparam as i32 == HOTKEY_ID {
                with_app(|app| toggle(app));
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_WATCH {
                with_app(|app| {
                    update_exclusion(app, GetForegroundWindow());
                    refresh_tray(app);
                });
            }
            0
        }
        WM_POWERBROADCAST => {
            if matches!(wparam as u32, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) {
                hook::reset();
            }
            TRUE as LRESULT
        }
        WM_WTSSESSION_CHANGE => {
            if matches!(
                wparam as u32,
                WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT | WTS_SESSION_LOGON
            ) {
                hook::reset();
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            with_app(|app| {
                tray(app, NIM_DELETE);
            });
            WTSUnRegisterSessionNotification(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => {
            let tbc = with_app(|app| app.msg_taskbar_created).unwrap_or(0);
            if tbc != 0 && msg == tbc {
                // エクスプローラーが再起動した → トレイアイコンを再登録
                with_app(|app| {
                    tray(app, NIM_ADD);
                });
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

/// 既存のインスタンスを終了させる (--replace 用)
fn close_existing() {
    let class = wide(CLASS_NAME);
    unsafe {
        let h = FindWindowW(class.as_ptr(), null());
        if !h.is_null() {
            PostMessageW(h, WM_CLOSE, 0, 0);
        }
    }
}

fn acquire_single_instance(replace: bool) -> bool {
    let name = wide(MUTEX_NAME);
    for attempt in 0..50 {
        unsafe {
            let h = CreateMutexW(null(), 0, name.as_ptr());
            let err = GetLastError();
            if !h.is_null() && err != ERROR_ALREADY_EXISTS {
                return true; // ハンドルはプロセス終了まで保持
            }
            if !h.is_null() {
                CloseHandle(h);
            }
        }
        if !replace {
            return false;
        }
        if attempt == 0 {
            close_existing();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |a: &str| args.iter().any(|x| x == a);

    if has("--install-autostart") || has("--uninstall-autostart") {
        let install = has("--install-autostart");
        let r = if install { autostart::install() } else { autostart::uninstall() };
        match r {
            Ok(()) if install => message_box(
                "ログオン時に管理者権限で自動起動するよう登録しました。\n\
                 今すぐ管理者権限で動かす場合は、トレイメニューの「管理者権限で再起動」を選んでください。",
                MB_ICONINFORMATION,
            ),
            Ok(()) => message_box("自動起動を解除しました。", MB_ICONINFORMATION),
            Err(e) => message_box(&format!("失敗しました: {e}"), MB_ICONERROR),
        }
        return;
    }

    if !acquire_single_instance(has("--replace")) {
        message_box(
            "Dvorak-Win は既に起動しています。\nタスクトレイのアイコンから操作してください。",
            MB_ICONINFORMATION,
        );
        return;
    }

    tune_process();
    let cfg = load_config(true);
    hook::ENABLED.store(cfg.enabled, Ordering::Relaxed);
    let _hook_thread = hook::start(cfg.clone());

    unsafe {
        let hinst = GetModuleHandleW(null());
        let class = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let title = wide("Dvorak-Win");
        // メッセージ専用ウィンドウだとブロードキャスト (TaskbarCreated, 電源通知) を受け取れないので
        // 非表示の通常ウィンドウを作る
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hwnd.is_null() {
            message_box("ウィンドウを作成できませんでした", MB_ICONERROR);
            hook::stop();
            return;
        }

        let tbc_name = wide("TaskbarCreated");
        let msg_taskbar_created = RegisterWindowMessageW(tbc_name.as_ptr());
        // 管理者権限で動いているとき、通常権限のエクスプローラーからのメッセージを受け取れるようにする
        ChangeWindowMessageFilterEx(hwnd, msg_taskbar_created, MSGFLT_ALLOW, null_mut());
        ChangeWindowMessageFilterEx(hwnd, WM_TRAY, MSGFLT_ALLOW, null_mut());

        let mut app = App {
            hwnd,
            cfg,
            icon_on: make_icon(0x00C0_6A1E, 0x00FF_FFFF), // 青 (BGR)
            icon_off: make_icon(0x0080_8080, 0x00E0_E0E0), // 灰
            msg_taskbar_created,
            elevated: is_elevated(),
            hotkey_ok: true,
        };
        register_hotkey(&mut app);
        tray(&app, NIM_ADD);
        let hotkey_ok = app.hotkey_ok;
        APP.with(|a| *a.borrow_mut() = Some(app));
        if !hotkey_ok {
            message_box("切り替えホットキーを登録できませんでした (他のアプリが使用中の可能性があります)", MB_ICONWARNING);
        }

        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        let win_event = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        with_app(|app| update_exclusion(app, GetForegroundWindow()));
        SetTimer(hwnd, TIMER_WATCH, 2000, None);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if !win_event.is_null() {
            UnhookWinEvent(win_event);
        }
        hook::stop();
    }
}
