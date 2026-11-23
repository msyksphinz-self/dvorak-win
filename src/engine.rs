//! キー変換エンジン (OS 非依存・単体テスト可能)。
//!
//! 物理キー (スキャンコード) の押下/解放を受け取り、
//! 「そのまま通す」か「元のイベントを捨てて別のキー列を注入する」かを決める。
//!
//! 重要な設計ポイント:
//! * 配列は「物理キー → 出力したい文字 (通常/Shift)」で定義し、
//!   文字 → 仮想キー + Shift の要否は OS のキーボードレイアウト (JIS/US) から求める。
//!   そのため JIS キーボードでも US 配列の Dvorak 記号を正しく出せる。
//! * Shift を一時的に外す必要がある場合 (例: JIS で ':' は Shift なし) でも、
//!   IME の「Shift 単独押しで英数切替」を誤爆させないよう、
//!   Shift の再押下は「次のキーが必要とするとき」まで遅延させる。

use crate::layout::{Layout, MAX_SC};

pub const VK_LSHIFT: u8 = 0xA0;
pub const VK_RSHIFT: u8 = 0xA1;
/// IME/メニューに「Shift 単独押し」と判定させないためのダミーキー (未割り当て VK)。
pub const VK_MASK: u8 = 0xE8;

const SC_LSHIFT: u16 = 0x2A;
const SC_RSHIFT: u16 = 0x36;

/// 仮想キー + Shift が必要か
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub vk: u8,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// 英字など: Shift/CapsLock の状態はそのまま、VK だけ差し替える
    Simple(u8),
    /// 記号など: Shift の有無で出すキーが変わる
    PerShift {
        normal: Option<Stroke>,
        shifted: Option<Stroke>,
    },
}

pub struct CompiledMap {
    actions: [Option<Action>; MAX_SC],
}

impl CompiledMap {
    pub fn empty() -> Self {
        CompiledMap {
            actions: [None; MAX_SC],
        }
    }

    /// `resolve` は「文字 → その文字を出す VK + Shift 要否」(OS レイアウト依存)
    pub fn compile(layout: &Layout, resolve: impl Fn(char) -> Option<Stroke>) -> Self {
        let mut m = Self::empty();
        for (sc, def) in layout.iter() {
            let n = def.normal.and_then(&resolve);
            let s = def.shifted.and_then(&resolve);
            let action = match (n, s) {
                (Some(n), Some(s)) if n.vk == s.vk && !n.shift && s.shift => Action::Simple(n.vk),
                (Some(n), None) if def.shifted.is_none() && !n.shift => Action::Simple(n.vk),
                (None, None) => continue,
                (n, s) => Action::PerShift {
                    normal: n,
                    shifted: s,
                },
            };
            m.actions[sc as usize] = Some(action);
        }
        m
    }

    pub fn get(&self, sc: u16) -> Option<Action> {
        self.actions.get(sc as usize).copied().flatten()
    }
}

/// フックが受け取った物理キーイベント
#[derive(Clone, Copy, Debug)]
pub struct KeyIn {
    pub vk: u8,
    pub sc: u16,
    pub ext: bool,
    pub up: bool,
}

/// 注入するキーイベント
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyOut {
    pub vk: u8,
    pub up: bool,
    /// Some なら元イベントのスキャンコードをそのまま使う (再注入用)
    pub raw: Option<(u16, bool)>,
}

impl KeyOut {
    fn down(vk: u8) -> Self {
        KeyOut { vk, up: false, raw: None }
    }
    fn up(vk: u8) -> Self {
        KeyOut { vk, up: true, raw: None }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// 元イベントをそのまま通す
    Pass,
    /// 元イベントを破棄 (空なら破棄のみ) して、これらを注入する
    Replace(Vec<KeyOut>),
}

/// 物理イベント時点の Shift 以外の修飾キー状態 + Shift の論理状態 (OS から取得)
#[derive(Clone, Copy, Debug, Default)]
pub struct ModState {
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
    /// OS が認識している (論理) Shift 状態
    pub logical_lshift: bool,
    pub logical_rshift: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub qwerty_ctrl: bool,
    pub qwerty_alt: bool,
    pub qwerty_win: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            qwerty_ctrl: true,
            qwerty_alt: true,
            qwerty_win: true,
        }
    }
}

#[derive(Default, Clone, Copy, Debug)]
struct Side {
    /// 物理的に押されているか
    phys: bool,
    /// 物理的には押されているが、我々が論理的に離している
    suppressed: bool,
}

pub struct Engine {
    pub options: Options,
    shift: [Side; 2],
    /// 物理キーごとに、押下時に注入した VK (解放時に同じ VK を離すため)
    held: [Option<u8>; MAX_SC],
}

impl Engine {
    pub fn new(options: Options) -> Self {
        Engine {
            options,
            shift: [Side::default(); 2],
            held: [None; MAX_SC],
        }
    }

    /// スリープ復帰やロック解除後など、状態が信用できないときに呼ぶ
    pub fn reset(&mut self) {
        self.shift = [Side::default(); 2];
        self.held = [None; MAX_SC];
    }

    fn shift_side(k: &KeyIn) -> Option<usize> {
        match k.vk {
            VK_LSHIFT => Some(0),
            VK_RSHIFT => Some(1),
            0x10 => match k.sc {
                SC_LSHIFT => Some(0),
                SC_RSHIFT => Some(1),
                _ => None,
            },
            _ => None,
        }
    }

    fn shift_vk(side: usize) -> u8 {
        if side == 0 {
            VK_LSHIFT
        } else {
            VK_RSHIFT
        }
    }

    /// OS の論理状態と自分の認識を突き合わせて自己修復する
    /// (フックが一時的に外れて Shift の解放を見逃した場合など)
    fn heal(&mut self, m: &ModState) {
        let logical = [m.logical_lshift, m.logical_rshift];
        for (side, s) in self.shift.iter_mut().enumerate() {
            if s.suppressed {
                if logical[side] {
                    // 他の誰かが Shift を押した → 抑制状態は解除済みとみなす
                    s.suppressed = false;
                    s.phys = true;
                }
            } else {
                s.phys = logical[side];
            }
        }
    }

    /// 抑制中の Shift をすべて押し直すイベント
    fn restore_shifts(&mut self, out: &mut Vec<KeyOut>) {
        for side in 0..2 {
            let s = &mut self.shift[side];
            if s.suppressed {
                s.suppressed = false;
                if s.phys {
                    out.push(KeyOut::down(Self::shift_vk(side)));
                }
            }
        }
    }

    fn any_suppressed(&self) -> bool {
        self.shift.iter().any(|s| s.suppressed)
    }

    /// 変換しない (QWERTY のまま) キー押下の処理
    fn pass_down(&mut self, k: &KeyIn) -> Decision {
        if !self.any_suppressed() {
            return Decision::Pass;
        }
        // Shift を論理的に離しているので、押し直してから元のキーを再注入する
        let mut out = Vec::with_capacity(3);
        self.restore_shifts(&mut out);
        out.push(KeyOut {
            vk: k.vk,
            up: false,
            raw: Some((k.sc, k.ext)),
        });
        Decision::Replace(out)
    }

    /// `active == false` のとき (無効化中・除外アプリ) は変換しないが、
    /// 押しっぱなしのキーや Shift の後始末は行う。
    pub fn process(&mut self, k: KeyIn, m: ModState, map: &CompiledMap, active: bool) -> Decision {
        // --- Shift キー自身 ---
        if let Some(side) = Self::shift_side(&k) {
            let s = &mut self.shift[side];
            if k.up {
                s.phys = false;
                if s.suppressed {
                    // 論理的には既に離している。ここで通すと「Shift 単独押し」に見えるので捨てる
                    s.suppressed = false;
                    return Decision::Replace(Vec::new());
                }
            } else {
                s.phys = true;
                if s.suppressed {
                    // オートリピート。抑制中は押し直さない
                    return Decision::Replace(Vec::new());
                }
            }
            return Decision::Pass;
        }

        self.heal(&m);
        let sc = k.sc as usize;

        // --- 解放 ---
        if k.up {
            if !k.ext && sc < MAX_SC {
                if let Some(vk) = self.held[sc].take() {
                    return Decision::Replace(vec![KeyOut::up(vk)]);
                }
            }
            return Decision::Pass;
        }

        // --- 押下 ---
        let action = if active && !k.ext { map.get(k.sc) } else { None };
        let qwerty = (m.ctrl && self.options.qwerty_ctrl)
            || (m.alt && self.options.qwerty_alt)
            || (m.win && self.options.qwerty_win);

        let action = match action {
            Some(a) if !qwerty => a,
            _ => {
                if sc < MAX_SC && !k.ext {
                    self.held[sc] = None;
                }
                return self.pass_down(&k);
            }
        };

        // Ctrl/Alt/Win 併用時 (QWERTY 指定なし) は Shift をいじらず通常文字の VK を使う
        let other_mods = m.ctrl || m.alt || m.win;
        let phys_shift = self.shift.iter().any(|s| s.phys);

        let mut out = Vec::with_capacity(6);
        let vk = match action {
            Action::Simple(vk) => {
                self.restore_shifts(&mut out);
                out.push(KeyOut::down(vk));
                vk
            }
            Action::PerShift { normal, shifted } => {
                let stroke = if other_mods {
                    normal.or(shifted).map(|s| Stroke { shift: phys_shift, ..s })
                } else if phys_shift {
                    shifted
                } else {
                    normal
                };
                let Some(st) = stroke else {
                    self.held[sc] = None;
                    return self.pass_down(&k);
                };
                let logical_shift = self.shift.iter().any(|s| s.phys && !s.suppressed);
                if st.shift {
                    if phys_shift {
                        // 物理 Shift あり: 抑制中なら押し直す
                        self.restore_shifts(&mut out);
                        out.push(KeyOut::down(st.vk));
                    } else {
                        // 物理 Shift なし: 一時的に Shift を付ける
                        out.push(KeyOut::down(VK_LSHIFT));
                        out.push(KeyOut::down(st.vk));
                        out.push(KeyOut::up(VK_LSHIFT));
                    }
                } else if logical_shift {
                    // Shift を外す必要がある。IME の Shift 単独押し判定を避けるためダミーキーを挟む
                    out.push(KeyOut::down(VK_MASK));
                    out.push(KeyOut::up(VK_MASK));
                    for side in 0..2 {
                        let s = &mut self.shift[side];
                        if s.phys && !s.suppressed {
                            s.suppressed = true;
                            out.push(KeyOut::up(Self::shift_vk(side)));
                        }
                    }
                    out.push(KeyOut::down(st.vk));
                } else {
                    out.push(KeyOut::down(st.vk));
                }
                st.vk
            }
        };
        self.held[sc] = Some(vk);
        Decision::Replace(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{sc_for_name, Layout};

    // JIS 配列 (Windows 日本語キーボード) における 文字 → VK + Shift
    fn jis(c: char) -> Option<Stroke> {
        let s = |vk: u8, shift: bool| Some(Stroke { vk, shift });
        match c {
            'a'..='z' => s(c.to_ascii_uppercase() as u8, false),
            'A'..='Z' => s(c as u8, true),
            '0'..='9' => s(c as u8, false),
            '!' => s(b'1', true),
            '"' => s(b'2', true),
            '#' => s(b'3', true),
            '$' => s(b'4', true),
            '%' => s(b'5', true),
            '&' => s(b'6', true),
            '\'' => s(b'7', true),
            '(' => s(b'8', true),
            ')' => s(b'9', true),
            '-' => s(0xBD, false),
            '=' => s(0xBD, true),
            '^' => s(0xDE, false),
            '~' => s(0xDE, true),
            '\\' => s(0xE2, false),
            '_' => s(0xE2, true),
            '@' => s(0xC0, false),
            '`' => s(0xC0, true),
            '[' => s(0xDB, false),
            '{' => s(0xDB, true),
            ';' => s(0xBB, false),
            '+' => s(0xBB, true),
            ':' => s(0xBA, false),
            '*' => s(0xBA, true),
            ']' => s(0xDD, false),
            '}' => s(0xDD, true),
            ',' => s(0xBC, false),
            '<' => s(0xBC, true),
            '.' => s(0xBE, false),
            '>' => s(0xBE, true),
            '/' => s(0xBF, false),
            '?' => s(0xBF, true),
            '|' => s(0xDC, true),
            _ => None,
        }
    }

    fn setup() -> (Engine, CompiledMap) {
        let map = CompiledMap::compile(&Layout::dvorak(), jis);
        (Engine::new(Options::default()), map)
    }

    fn key(name: &str, up: bool) -> KeyIn {
        let sc = sc_for_name(name).unwrap();
        KeyIn { vk: 0, sc, ext: false, up }
    }
    fn shift(up: bool) -> KeyIn {
        KeyIn { vk: VK_LSHIFT, sc: SC_LSHIFT, ext: false, up }
    }
    fn d(vk: u8) -> KeyOut {
        KeyOut::down(vk)
    }
    fn u(vk: u8) -> KeyOut {
        KeyOut::up(vk)
    }
    fn none() -> ModState {
        ModState::default()
    }
    fn lshift() -> ModState {
        ModState { logical_lshift: true, ..Default::default() }
    }

    #[test]
    fn letters_are_simple() {
        let (mut e, map) = setup();
        // QWERTY の 's' の位置 → Dvorak 'o'
        assert_eq!(e.process(key("s", false), none(), &map, true), Decision::Replace(vec![d(b'O')]));
        assert_eq!(e.process(key("s", true), none(), &map, true), Decision::Replace(vec![u(b'O')]));
        // Shift 併用でも Shift はいじらない (CapsLock も OS 任せ)
        e.process(shift(false), none(), &map, true);
        assert_eq!(e.process(key("s", false), lshift(), &map, true), Decision::Replace(vec![d(b'O')]));
    }

    #[test]
    fn quote_needs_temporary_shift_on_jis() {
        let (mut e, map) = setup();
        // QWERTY 'q' → Dvorak '\'' は JIS では Shift+7
        assert_eq!(
            e.process(key("q", false), none(), &map, true),
            Decision::Replace(vec![d(VK_LSHIFT), d(b'7'), u(VK_LSHIFT)])
        );
        assert_eq!(e.process(key("q", true), none(), &map, true), Decision::Replace(vec![u(b'7')]));
    }

    #[test]
    fn colon_releases_shift_lazily() {
        let (mut e, map) = setup();
        assert_eq!(e.process(shift(false), none(), &map, true), Decision::Pass);
        // Shift+'z' → ':' (JIS では Shift なし)
        assert_eq!(
            e.process(key("z", false), lshift(), &map, true),
            Decision::Replace(vec![d(VK_MASK), u(VK_MASK), u(VK_LSHIFT), d(0xBA)])
        );
        assert_eq!(e.process(key("z", true), none(), &map, true), Decision::Replace(vec![u(0xBA)]));
        // Shift のオートリピートは捨てる (押し直さない)
        assert_eq!(e.process(shift(false), none(), &map, true), Decision::Replace(vec![]));
        // 続けて Shift+'q' → '"' (Shift+2): ここで初めて Shift を押し直す
        assert_eq!(
            e.process(key("q", false), none(), &map, true),
            Decision::Replace(vec![d(VK_LSHIFT), d(b'2')])
        );
        e.process(key("q", true), lshift(), &map, true);
        // 物理 Shift 解放は普通に通る
        assert_eq!(e.process(shift(true), lshift(), &map, true), Decision::Pass);
    }

    #[test]
    fn shift_release_while_suppressed_is_swallowed() {
        let (mut e, map) = setup();
        e.process(shift(false), none(), &map, true);
        e.process(key("z", false), lshift(), &map, true);
        e.process(key("z", true), none(), &map, true);
        // 論理的に既に離しているので、Shift の解放は捨てる (IME の英数切替誤爆防止)
        assert_eq!(e.process(shift(true), none(), &map, true), Decision::Replace(vec![]));
        // 以降は通常状態
        assert_eq!(e.process(key("z", false), none(), &map, true), Decision::Replace(vec![d(0xBB)]));
    }

    #[test]
    fn passthrough_key_restores_shift() {
        let (mut e, map) = setup();
        e.process(shift(false), none(), &map, true);
        e.process(key("z", false), lshift(), &map, true);
        e.process(key("z", true), none(), &map, true);
        // 数字キー (変換対象外) を Shift 押下中に押す → '!' になるよう Shift を押し直して再注入
        let one = KeyIn { vk: b'1', sc: 0x02, ext: false, up: false };
        assert_eq!(
            e.process(one, none(), &map, true),
            Decision::Replace(vec![
                d(VK_LSHIFT),
                KeyOut { vk: b'1', up: false, raw: Some((0x02, false)) }
            ])
        );
    }

    #[test]
    fn ctrl_shortcuts_stay_qwerty() {
        let (mut e, map) = setup();
        let ctrl = ModState { ctrl: true, ..Default::default() };
        assert_eq!(e.process(key("c", false), ctrl, &map, true), Decision::Pass);
        assert_eq!(e.process(key("c", true), ctrl, &map, true), Decision::Pass);

        e.options.qwerty_ctrl = false;
        // QWERTY 'c' → Dvorak 'j'
        assert_eq!(e.process(key("c", false), ctrl, &map, true), Decision::Replace(vec![d(b'J')]));
    }

    #[test]
    fn inactive_still_releases_held_keys() {
        let (mut e, map) = setup();
        assert_eq!(e.process(key("s", false), none(), &map, true), Decision::Replace(vec![d(b'O')]));
        // 押している途中で無効化されても、離すときは 'O' を離す
        assert_eq!(e.process(key("s", true), none(), &map, false), Decision::Replace(vec![u(b'O')]));
        assert_eq!(e.process(key("s", false), none(), &map, false), Decision::Pass);
    }

    #[test]
    fn heals_missed_shift_release() {
        let (mut e, map) = setup();
        e.process(shift(false), none(), &map, true);
        // Shift の解放を見逃した (フック外れ等)。OS 上は Shift は離れている
        assert_eq!(
            e.process(key("q", false), none(), &map, true),
            Decision::Replace(vec![d(VK_LSHIFT), d(b'7'), u(VK_LSHIFT)])
        );
    }

    #[test]
    fn default_dvorak_symbols_on_jis() {
        let map = CompiledMap::compile(&Layout::dvorak(), jis);
        let st = |vk, shift| Some(Stroke { vk, shift });
        // '-' キー位置 → '[' '{'  (JIS では同じキー)
        assert_eq!(map.get(0x0C), Some(Action::Simple(0xDB)));
        // '=' キー位置 → ']' '}'
        assert_eq!(map.get(0x0D), Some(Action::Simple(0xDD)));
        // ''' キー位置 → '-' '_'
        assert_eq!(map.get(0x28), Some(Action::PerShift { normal: st(0xBD, false), shifted: st(0xE2, true) }));
        // ']' キー位置 → '=' '+'
        assert_eq!(map.get(0x1B), Some(Action::PerShift { normal: st(0xBD, true), shifted: st(0xBB, true) }));
    }
}
