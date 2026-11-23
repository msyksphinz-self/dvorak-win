//! 配列定義: 物理キー (スキャンコード) → 出したい文字 (通常 / Shift)

pub const MAX_SC: usize = 0x80;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyDef {
    pub normal: Option<char>,
    pub shifted: Option<char>,
}

#[derive(Clone)]
pub struct Layout {
    keys: [Option<KeyDef>; MAX_SC],
}

/// キー名 (US QWERTY の刻印) とスキャンコードの対応
const KEY_NAMES: &[(&str, u16)] = &[
    ("grave", 0x29),
    ("1", 0x02),
    ("2", 0x03),
    ("3", 0x04),
    ("4", 0x05),
    ("5", 0x06),
    ("6", 0x07),
    ("7", 0x08),
    ("8", 0x09),
    ("9", 0x0A),
    ("0", 0x0B),
    ("minus", 0x0C),
    ("equal", 0x0D),
    ("q", 0x10),
    ("w", 0x11),
    ("e", 0x12),
    ("r", 0x13),
    ("t", 0x14),
    ("y", 0x15),
    ("u", 0x16),
    ("i", 0x17),
    ("o", 0x18),
    ("p", 0x19),
    ("lbracket", 0x1A),
    ("rbracket", 0x1B),
    ("a", 0x1E),
    ("s", 0x1F),
    ("d", 0x20),
    ("f", 0x21),
    ("g", 0x22),
    ("h", 0x23),
    ("j", 0x24),
    ("k", 0x25),
    ("l", 0x26),
    ("semicolon", 0x27),
    ("quote", 0x28),
    ("backslash", 0x2B),
    ("z", 0x2C),
    ("x", 0x2D),
    ("c", 0x2E),
    ("v", 0x2F),
    ("b", 0x30),
    ("n", 0x31),
    ("m", 0x32),
    ("comma", 0x33),
    ("period", 0x34),
    ("slash", 0x35),
    // JIS キーボード固有
    ("ro", 0x73),
    ("yen", 0x7D),
];

pub fn sc_for_name(name: &str) -> Option<u16> {
    let lower = name.trim().to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("sc:") {
        let hex = hex.trim_start_matches("0x");
        return u16::from_str_radix(hex, 16).ok().filter(|&v| (v as usize) < MAX_SC);
    }
    KEY_NAMES.iter().find(|(n, _)| *n == lower).map(|&(_, sc)| sc)
}

/// 標準 Dvorak (US) のうち、QWERTY と異なる位置のキー
pub const DVORAK: &[(&str, char, char)] = &[
    ("minus", '[', '{'),
    ("equal", ']', '}'),
    ("q", '\'', '"'),
    ("w", ',', '<'),
    ("e", '.', '>'),
    ("r", 'p', 'P'),
    ("t", 'y', 'Y'),
    ("y", 'f', 'F'),
    ("u", 'g', 'G'),
    ("i", 'c', 'C'),
    ("o", 'r', 'R'),
    ("p", 'l', 'L'),
    ("lbracket", '/', '?'),
    ("rbracket", '=', '+'),
    ("a", 'a', 'A'),
    ("s", 'o', 'O'),
    ("d", 'e', 'E'),
    ("f", 'u', 'U'),
    ("g", 'i', 'I'),
    ("h", 'd', 'D'),
    ("j", 'h', 'H'),
    ("k", 't', 'T'),
    ("l", 'n', 'N'),
    ("semicolon", 's', 'S'),
    ("quote", '-', '_'),
    ("z", ';', ':'),
    ("x", 'q', 'Q'),
    ("c", 'j', 'J'),
    ("v", 'k', 'K'),
    ("b", 'x', 'X'),
    ("n", 'b', 'B'),
    ("m", 'm', 'M'),
    ("comma", 'w', 'W'),
    ("period", 'v', 'V'),
    ("slash", 'z', 'Z'),
];

impl Layout {
    pub fn empty() -> Self {
        Layout {
            keys: [None; MAX_SC],
        }
    }

    pub fn dvorak() -> Self {
        let mut l = Self::empty();
        for &(name, n, s) in DVORAK {
            l.set(sc_for_name(name).unwrap(), Some(n), Some(s));
        }
        l
    }

    pub fn set(&mut self, sc: u16, normal: Option<char>, shifted: Option<char>) {
        if (sc as usize) < MAX_SC {
            self.keys[sc as usize] = Some(KeyDef { normal, shifted });
        }
    }

    #[cfg(test)]
    pub fn get(&self, sc: u16) -> Option<KeyDef> {
        self.keys.get(sc as usize).copied().flatten()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, KeyDef)> + '_ {
        self.keys
            .iter()
            .enumerate()
            .filter_map(|(i, k)| k.map(|k| (i as u16, k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(sc_for_name("Q"), Some(0x10));
        assert_eq!(sc_for_name("sc:0x73"), Some(0x73));
        assert_eq!(sc_for_name("sc:7d"), Some(0x7D));
        assert_eq!(sc_for_name("sc:ff"), None);
        assert_eq!(sc_for_name("nope"), None);
    }

    #[test]
    fn dvorak_has_all_keys() {
        let l = Layout::dvorak();
        assert_eq!(l.iter().count(), DVORAK.len());
        assert_eq!(l.get(0x2C), Some(KeyDef { normal: Some(';'), shifted: Some(':') }));
    }
}
