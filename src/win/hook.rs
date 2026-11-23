//! 低レベルキーボードフック (専用スレッド)
//!
//! 安定動作のための工夫:
//! * フック専用スレッドを最高優先度で動かし、コールバックではファイル I/O 等を一切しない
//!   (Windows は LowLevelHooksTimeout を超えたフックを黙って外すため)。
//! * 一定間隔で「新しいフックを付けてから古いフックを外す」付け直しを行い、
//!   万一外されても数秒で自動復旧する。スリープ復帰・ロック解除時も即座に付け直す。
//! * コールバック内のパニックは捕捉し、元のキーをそのまま通す。

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::engine::{CompiledMap, Decision, Engine, KeyIn, KeyOut, ModState, Options, Stroke};
use crate::layout::Layout;

/// 自分が注入したイベントの印
const MAGIC: usize = 0x4456_4B57; // "DVKW"

const MSG_CONFIG: u32 = WM_APP + 1;
const MSG_REHOOK: u32 = WM_APP + 2;
const MSG_RESET: u32 = WM_APP + 3;

/// ユーザーが有効にしているか
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// 前面のアプリが除外対象か
pub static EXCLUDED: AtomicBool = AtomicBool::new(false);
/// フックを付け直した回数 (診断用)
pub static REHOOK_COUNT: AtomicU32 = AtomicU32::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);

struct State {
    hook: HHOOK,
    engine: Engine,
    layout: Layout,
    map: CompiledMap,
    hkl: HKL,
    rehook_interval_ms: u32,
    /// スレッドタイマーの ID (hwnd なしの SetTimer は ID を自動採番する)
    timer: usize,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn options_from(cfg: &Config) -> Options {
    Options {
        qwerty_ctrl: cfg.qwerty_ctrl,
        qwerty_alt: cfg.qwerty_alt,
        qwerty_win: cfg.qwerty_win,
    }
}

/// 文字 → その文字を出す VK + Shift (指定キーボードレイアウトで)
fn resolver(hkl: HKL) -> impl Fn(char) -> Option<Stroke> {
    move |c: char| {
        let mut buf = [0u16; 2];
        if c.encode_utf16(&mut buf).len() != 1 {
            return None;
        }
        let r = unsafe { VkKeyScanExW(buf[0], hkl) };
        if r == -1 {
            return None;
        }
        let vk = (r & 0xFF) as u8;
        let mods = ((r >> 8) & 0xFF) as u8;
        if mods & !1 != 0 {
            return None; // Ctrl/Alt が必要な文字は扱わない
        }
        Some(Stroke { vk, shift: mods & 1 != 0 })
    }
}

fn foreground_hkl() -> HKL {
    unsafe {
        let fg = GetForegroundWindow();
        let tid = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, null_mut()) };
        let hkl = GetKeyboardLayout(tid);
        if hkl.is_null() {
            GetKeyboardLayout(0)
        } else {
            hkl
        }
    }
}

fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

fn send(events: &[KeyOut], hkl: HKL) {
    if events.is_empty() {
        return;
    }
    let inputs: Vec<INPUT> = events
        .iter()
        .map(|e| {
            let (scan, ext) = match e.raw {
                Some(r) => r,
                None => {
                    let sc = unsafe { MapVirtualKeyExW(e.vk as u32, MAPVK_VK_TO_VSC_EX, hkl) };
                    ((sc & 0xFF) as u16, sc & 0xFF00 == 0xE000)
                }
            };
            let mut flags = 0;
            if e.up {
                flags |= KEYEVENTF_KEYUP;
            }
            if ext {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: e.vk as u16,
                        wScan: scan,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: MAGIC,
                    },
                },
            }
        })
        .collect();
    unsafe {
        SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
    }
}

/// true を返すと元イベントを破棄する
fn handle(kb: &KBDLLHOOKSTRUCT, wparam: WPARAM) -> bool {
    let up = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
    let k = KeyIn {
        vk: kb.vkCode as u8,
        sc: kb.scanCode as u16,
        ext: kb.flags & LLKHF_EXTENDED != 0,
        up,
    };
    if kb.vkCode > 0xFF || kb.vkCode == VK_PACKET as u32 {
        return false;
    }
    STATE.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(st) = guard.as_mut() else {
            return false;
        };
        if !up {
            // アプリごとに入力言語が違う場合に備え、前面のレイアウトに追従
            let hkl = foreground_hkl();
            if hkl != st.hkl {
                st.hkl = hkl;
                st.map = CompiledMap::compile(&st.layout, resolver(hkl));
            }
        }
        let mods = ModState {
            ctrl: key_down(VK_CONTROL as i32),
            alt: key_down(VK_MENU as i32),
            win: key_down(VK_LWIN as i32) || key_down(VK_RWIN as i32),
            logical_lshift: key_down(VK_LSHIFT as i32),
            logical_rshift: key_down(VK_RSHIFT as i32),
        };
        let active = ENABLED.load(Ordering::Relaxed) && !EXCLUDED.load(Ordering::Relaxed);
        match st.engine.process(k, mods, &st.map, active) {
            Decision::Pass => false,
            Decision::Replace(events) => {
                send(&events, st.hkl);
                true
            }
        }
    })
}

unsafe extern "system" fn ll_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        // 注入イベント (自分のもの・他アプリのもの) には手を出さない
        if kb.flags & LLKHF_INJECTED == 0 && kb.dwExtraInfo != MAGIC {
            if let Ok(true) = catch_unwind(AssertUnwindSafe(|| handle(kb, wparam))) {
                return 1;
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

fn install(st: &mut State) {
    unsafe {
        // 新しいフックを先に付ける → チェーンの先頭になる。その後で古いフックを外す
        let new = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), GetModuleHandleW(null_mut()), 0);
        if !new.is_null() {
            if !st.hook.is_null() {
                UnhookWindowsHookEx(st.hook);
            }
            st.hook = new;
            REHOOK_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn apply_config(st: &mut State, cfg: &Config) {
    st.engine.options = options_from(cfg);
    st.layout = cfg.layout.clone();
    st.map = CompiledMap::compile(&st.layout, resolver(st.hkl));
    st.rehook_interval_ms = cfg.rehook_interval_ms;
    unsafe {
        if st.timer != 0 {
            KillTimer(null_mut(), st.timer);
            st.timer = 0;
        }
        if st.rehook_interval_ms > 0 {
            st.timer = SetTimer(null_mut(), 0, st.rehook_interval_ms, None);
        }
    }
}

fn thread_main(cfg: Config, ready: mpsc::Sender<()>) {
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        // メッセージキューを作ってから準備完了を通知
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
        THREAD_ID.store(GetCurrentThreadId(), Ordering::SeqCst);
    }
    let hkl = foreground_hkl();
    let mut st = State {
        hook: null_mut(),
        engine: Engine::new(options_from(&cfg)),
        layout: Layout::empty(),
        map: CompiledMap::empty(),
        hkl,
        rehook_interval_ms: 0,
        timer: 0,
    };
    apply_config(&mut st, &cfg);
    install(&mut st);
    STATE.with(|c| *c.borrow_mut() = Some(st));
    let _ = ready.send(());

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_TIMER | MSG_REHOOK => {
                    STATE.with(|c| c.borrow_mut().as_mut().map(install));
                }
                MSG_RESET => {
                    STATE.with(|c| {
                        if let Some(st) = c.borrow_mut().as_mut() {
                            st.engine.reset();
                            st.hkl = foreground_hkl();
                            st.map = CompiledMap::compile(&st.layout, resolver(st.hkl));
                            install(st);
                        }
                    });
                }
                MSG_CONFIG => {
                    let cfg = Box::from_raw(msg.lParam as *mut Config);
                    STATE.with(|c| c.borrow_mut().as_mut().map(|st| apply_config(st, &cfg)));
                }
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        STATE.with(|c| {
            if let Some(st) = c.borrow_mut().take() {
                if !st.hook.is_null() {
                    UnhookWindowsHookEx(st.hook);
                }
            }
        });
    }
}

pub fn start(cfg: Config) -> std::thread::JoinHandle<()> {
    let (tx, rx) = mpsc::channel();
    let h = std::thread::Builder::new()
        .name("keyboard-hook".into())
        .spawn(move || thread_main(cfg, tx))
        .expect("フックスレッドを起動できません");
    let _ = rx.recv();
    h
}

fn post(msg: u32, lparam: LPARAM) -> bool {
    let tid = THREAD_ID.load(Ordering::SeqCst);
    tid != 0 && unsafe { PostThreadMessageW(tid, msg, 0, lparam) } != 0
}

pub fn update_config(cfg: Config) {
    let p = Box::into_raw(Box::new(cfg));
    if !post(MSG_CONFIG, p as LPARAM) {
        drop(unsafe { Box::from_raw(p) });
    }
}

/// スリープ復帰・ロック解除後など: 状態をリセットしてフックを付け直す
pub fn reset() {
    post(MSG_RESET, 0);
}

pub fn stop() {
    post(WM_QUIT, 0);
}
