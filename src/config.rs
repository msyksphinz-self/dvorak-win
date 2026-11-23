//! 設定ファイル (dvorak-win.ini) の読み込み

use crate::layout::{sc_for_name, Layout, DVORAK};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    /// RegisterHotKey の MOD_* (ALT=1, CONTROL=2, SHIFT=4, WIN=8)
    pub mods: u32,
    pub vk: u32,
}

#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
    pub qwerty_ctrl: bool,
    pub qwerty_alt: bool,
    pub qwerty_win: bool,
    pub toggle_hotkey: Option<Hotkey>,
    pub rehook_interval_ms: u32,
    /// 小文字の実行ファイル名
    pub exclude: Vec<String>,
    pub layout: Layout,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            qwerty_ctrl: true,
            qwerty_alt: true,
            qwerty_win: true,
            toggle_hotkey: parse_hotkey("Ctrl+Shift+F12"),
            rehook_interval_ms: 3000,
            exclude: Vec::new(),
            layout: Layout::dvorak(),
        }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub fn parse_hotkey(s: &str) -> Option<Hotkey> {
    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+') {
        let p = part.trim().to_ascii_lowercase();
        match p.as_str() {
            "ctrl" | "control" => mods |= 2,
            "alt" => mods |= 1,
            "shift" => mods |= 4,
            "win" => mods |= 8,
            "" => return None,
            key => {
                if vk.is_some() {
                    return None;
                }
                vk = Some(key_vk(key)?);
            }
        }
    }
    Some(Hotkey { mods, vk: vk? })
}

fn key_vk(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_alphanumeric()) {
        return Some(b[0].to_ascii_uppercase() as u32);
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    Some(match key {
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "space" => 0x20,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "esc" | "escape" => 0x1B,
        _ => return None,
    })
}

fn parse_char_token(t: &str) -> Result<Option<char>, ()> {
    match t {
        "none" => Ok(None),
        "space" => Ok(Some(' ')),
        _ => {
            let mut it = t.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Ok(Some(c)),
                _ => Err(()),
            }
        }
    }
}

/// 設定を解析する。問題があった行は警告として返し、その行は無視する。
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut warnings = Vec::new();
    let mut section = String::new();
    let mut custom_layout: Option<Layout> = None;

    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            if section == "layout" && custom_layout.is_none() {
                custom_layout = Some(Layout::empty());
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warnings.push(format!("{lineno} 行目: '=' がありません"));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        // 値に '=' を書く場合 (例: "rbracket = = +") に備え、キー名の後ろの最初の '=' で分割済み
        match section.as_str() {
            "general" => {
                let key = k.to_ascii_lowercase();
                let ok = match key.as_str() {
                    "enabled" => parse_bool(v).map(|b| cfg.enabled = b).is_some(),
                    "qwerty_shortcuts" => {
                        cfg.qwerty_ctrl = false;
                        cfg.qwerty_alt = false;
                        cfg.qwerty_win = false;
                        let mut ok = true;
                        for m in v.split(',').map(|s| s.trim().to_ascii_lowercase()) {
                            match m.as_str() {
                                "ctrl" => cfg.qwerty_ctrl = true,
                                "alt" => cfg.qwerty_alt = true,
                                "win" => cfg.qwerty_win = true,
                                "" | "none" => {}
                                _ => ok = false,
                            }
                        }
                        ok
                    }
                    "toggle_hotkey" => {
                        if v.is_empty() || v.eq_ignore_ascii_case("none") {
                            cfg.toggle_hotkey = None;
                            true
                        } else {
                            parse_hotkey(v).map(|h| cfg.toggle_hotkey = Some(h)).is_some()
                        }
                    }
                    "rehook_interval_ms" => v
                        .parse::<u32>()
                        .ok()
                        .filter(|&n| n == 0 || n >= 500)
                        .map(|n| cfg.rehook_interval_ms = n)
                        .is_some(),
                    "exclude" => {
                        cfg.exclude = v
                            .split(',')
                            .map(|s| s.trim().to_lowercase())
                            .filter(|s| !s.is_empty())
                            .collect();
                        true
                    }
                    _ => false,
                };
                if !ok {
                    warnings.push(format!("{lineno} 行目: 不正な設定 '{k} = {v}'"));
                }
            }
            "layout" => {
                let Some(sc) = sc_for_name(k) else {
                    warnings.push(format!("{lineno} 行目: 不明なキー名 '{k}'"));
                    continue;
                };
                let toks: Vec<&str> = v.split_whitespace().collect();
                let parsed = match toks.as_slice() {
                    [n] => parse_char_token(n).map(|n| (n, None)),
                    [n, s] => parse_char_token(n).and_then(|n| parse_char_token(s).map(|s| (n, s))),
                    _ => Err(()),
                };
                match parsed {
                    Ok((n, s)) => custom_layout.as_mut().unwrap().set(sc, n, s),
                    Err(()) => warnings.push(format!(
                        "{lineno} 行目: 値は「通常文字 Shift文字」の形式で書いてください: '{v}'"
                    )),
                }
            }
            _ => warnings.push(format!("{lineno} 行目: 不明なセクション [{section}]")),
        }
    }
    if let Some(l) = custom_layout {
        cfg.layout = l;
    }
    (cfg, warnings)
}


pub fn default_text() -> String {
    let mut s = String::from(
        "\
; Dvorak-Win 設定ファイル
; 変更後はトレイアイコンの右クリックメニュー「設定を再読み込み」で反映されます。

[general]
; 起動時に有効にするか
enabled = true

; これらの修飾キーを押している間は QWERTY のまま (Ctrl+C / Ctrl+V などを従来の位置で使う)
; 全部 Dvorak にしたい場合は none
qwerty_shortcuts = ctrl, alt, win

; 有効/無効の切り替えキー (none で無効)。例: Ctrl+Alt+D, Ctrl+Shift+F12, Pause
toggle_hotkey = Ctrl+Shift+F12

; Windows はフックの応答が遅れると黙ってフックを外すことがあるため、
; 一定間隔でフックを付け直します (ミリ秒, 0 で無効, 最小 500)
rehook_interval_ms = 3000

; Dvorak を適用しないアプリ (実行ファイル名, カンマ区切り)
; 例: exclude = mstsc.exe, vmconnect.exe, game.exe
exclude =

[layout]
; 物理キー (US QWERTY の刻印名) = 通常時の文字  Shift時の文字
; 文字は「OS のキーボード配列 (JIS/US) でその文字を打つキー」に自動変換されます。
; none = そのキーは変換しない, space = 空白
; キー名: grave 1-0 minus equal q-p lbracket rbracket a-l semicolon quote backslash
;         z-m comma period slash ro yen, または sc:XX (16進スキャンコード)
;
; 数字段の記号も US Dvorak にしたい場合は、例えば次のように追加:
;   2 = 2 @
;   6 = 6 ^
",
    );
    for &(name, n, sh) in DVORAK {
        s.push_str(&format!("{:<10}= {} {}\n", name, n, sh));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_roundtrips() {
        let (cfg, w) = parse(&default_text());
        assert!(w.is_empty(), "{w:?}");
        let d = Layout::dvorak();
        for sc in 0..0x80u16 {
            assert_eq!(cfg.layout.get(sc), d.get(sc), "sc {sc:#x}");
        }
        assert_eq!(cfg.toggle_hotkey, Some(Hotkey { mods: 2 | 4, vk: 0x7B }));
        assert!(cfg.qwerty_ctrl && cfg.qwerty_alt && cfg.qwerty_win);
    }

    #[test]
    fn general_options() {
        let (cfg, w) = parse(
            "[general]\nenabled = no\nqwerty_shortcuts = ctrl\ntoggle_hotkey = none\nexclude = MSTSC.exe, foo.exe\nrehook_interval_ms = 0\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert!(!cfg.enabled);
        assert!(cfg.qwerty_ctrl && !cfg.qwerty_alt && !cfg.qwerty_win);
        assert_eq!(cfg.toggle_hotkey, None);
        assert_eq!(cfg.exclude, vec!["mstsc.exe", "foo.exe"]);
        assert_eq!(cfg.rehook_interval_ms, 0);
        // [layout] が無ければ標準 Dvorak
        assert!(cfg.layout.get(0x10).is_some());
    }

    #[test]
    fn layout_lines() {
        let (cfg, w) = parse("[layout]\nq = ' \"\nrbracket = = +\n2 = 2 @\nyen = none\nbad = a b\nw = abc\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!(cfg.layout.get(0x1B).unwrap().normal, Some('='));
        assert_eq!(cfg.layout.get(0x03).unwrap().shifted, Some('@'));
        assert_eq!(cfg.layout.get(0x7D).unwrap().normal, None);
        // 書かれていないキーは変換しない
        assert_eq!(cfg.layout.get(0x1F), None);
    }

    #[test]
    fn hotkeys() {
        assert_eq!(parse_hotkey("Ctrl+Alt+D"), Some(Hotkey { mods: 3, vk: b'D' as u32 }));
        assert_eq!(parse_hotkey("Pause"), Some(Hotkey { mods: 0, vk: 0x13 }));
        assert_eq!(parse_hotkey("Ctrl+"), None);
        assert_eq!(parse_hotkey("Ctrl+Foo"), None);
    }
}
