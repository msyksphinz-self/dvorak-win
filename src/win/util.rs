use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn exe_path() -> PathBuf {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]))
}

pub fn config_path() -> PathBuf {
    exe_path().with_file_name("dvorak-win.ini")
}

pub fn message_box(text: &str, icon: MESSAGEBOX_STYLE) {
    let t = wide(text);
    let c = wide("Dvorak-Win");
    unsafe {
        MessageBoxW(null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_SETFOREGROUND | icon);
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elev.TokenIsElevated != 0
    }
}

/// プロセスの実行ファイル名 (小文字, パスなし)
pub fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(|s| s.to_lowercase())
    }
}

/// Windows 11 の「効率モード」(EcoQoS) で間引かれないようにし、優先度を少し上げる。
/// バックグラウンドで間引かれるとフックの応答が遅れ、Windows にフックを外される原因になる。
pub fn tune_process() {
    unsafe {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
        SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS);
    }
}
