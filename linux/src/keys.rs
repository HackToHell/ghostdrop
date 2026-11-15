//! Key combos (`ctrl+shift+t`, `ctrl+grave_accent`, `f12`) for the hotkey and app
//! bindings, matched against xkb keysyms.
use xkbcommon::xkb::{self, Keysym};

/// Lower-case a keysym (A → a); non-letters are returned unchanged.
pub fn keysym_lower(sym: u32) -> u32 {
    let u = xkb::keysym_to_utf32(Keysym::new(sym));
    match char::from_u32(u) {
        Some(c) if c.is_alphabetic() => {
            let l = c.to_lowercase().next().unwrap_or(c);
            let s = xkb::utf32_to_keysym(l as u32).raw();
            if s == 0 { sym } else { s }
        }
        _ => sym,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub sup: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable character, matched against the unshifted keysym.
    Char(char),
    /// A named keysym (Tab, Prior, F12, ...).
    Sym(u32),
}

#[derive(Clone, Debug)]
pub struct KeyCombo {
    pub mods: Mods,
    pub key: Key,
}

/// Ghostty-style key names → xkb keysym names.
fn alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "grave_accent" | "backquote" | "grave" | "backtick" => "grave",
        "minus" => "minus",
        "equal" => "equal",
        "bracket_left" => "bracketleft",
        "bracket_right" => "bracketright",
        "backslash" => "backslash",
        "semicolon" => "semicolon",
        "quote" | "apostrophe" => "apostrophe",
        "comma" => "comma",
        "period" => "period",
        "slash" => "slash",
        "space" => "space",
        "tab" => "Tab",
        "enter" | "return" => "Return",
        "escape" | "esc" => "Escape",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "page_up" | "pageup" => "Prior",
        "page_down" | "pagedown" => "Next",
        "left" | "arrow_left" => "Left",
        "right" | "arrow_right" => "Right",
        "up" | "arrow_up" => "Up",
        "down" | "arrow_down" => "Down",
        "print" | "print_screen" => "Print",
        "menu" | "context_menu" => "Menu",
        "digit_0" => "0",
        "digit_1" => "1",
        "digit_2" => "2",
        "digit_3" => "3",
        "digit_4" => "4",
        "digit_5" => "5",
        "digit_6" => "6",
        "digit_7" => "7",
        "digit_8" => "8",
        "digit_9" => "9",
        _ => return None,
    })
}

impl KeyCombo {
    pub fn parse(raw: &str) -> Option<KeyCombo> {
        let s = raw.trim().to_lowercase();
        if s.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = s.split('+').map(|p| p.to_string()).collect();
        if s.ends_with("++") {
            parts.truncate(parts.len().saturating_sub(2));
            parts.push("+".into());
        }
        let key_name = parts.pop()?;
        if key_name.is_empty() {
            return None;
        }
        let mut mods = Mods::default();
        for p in parts {
            match p.as_str() {
                "ctrl" | "control" => mods.ctrl = true,
                "shift" => mods.shift = true,
                "alt" | "opt" | "option" | "meta" => mods.alt = true,
                "super" | "cmd" | "command" | "logo" | "win" | "mod4" => mods.sup = true,
                _ => return None,
            }
        }
        let key = if let Some(name) = alias(&key_name) {
            let sym = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
            if sym.raw() == 0 {
                return None;
            }
            if name.chars().count() == 1 { Key::Char(name.chars().next()?) } else { Key::Sym(sym.raw()) }
        } else if key_name.chars().count() == 1 {
            Key::Char(key_name.chars().next()?)
        } else {
            let sym = xkb::keysym_from_name(&key_name, xkb::KEYSYM_CASE_INSENSITIVE);
            if sym.raw() == 0 {
                return None;
            }
            Key::Sym(sym.raw())
        };
        // "grave" etc. resolve to a single character: match by character.
        let key = match key {
            Key::Sym(sym) => match char::from_u32(xkb::keysym_to_utf32(Keysym::new(sym))) {
                Some(c) if !c.is_control() && c != '\0' && sym < 0xff00 => Key::Char(c.to_lowercase().next().unwrap_or(c)),
                _ => Key::Sym(sym),
            },
            k => k,
        };
        Some(KeyCombo { mods, key })
    }

    /// `unshifted` is the level-0 keysym of the pressed key.
    pub fn matches(&self, mods: Mods, unshifted: u32) -> bool {
        if mods != self.mods {
            return false;
        }
        match &self.key {
            Key::Char(c) => char::from_u32(xkb::keysym_to_utf32(Keysym::new(unshifted)))
                .and_then(|u| u.to_lowercase().next())
                .is_some_and(|u| u == *c),
            Key::Sym(s) => {
                let a = keysym_lower(unshifted);
                let b = keysym_lower(*s);
                a == b
            }
        }
    }

    /// The keysym to look up a keycode for (X11 global grab).
    pub fn keysym(&self) -> u32 {
        match &self.key {
            Key::Sym(s) => *s,
            Key::Char(c) => xkb::utf32_to_keysym(*c as u32).raw(),
        }
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.mods.ctrl {
            parts.push("ctrl".to_string());
        }
        if self.mods.alt {
            parts.push("alt".to_string());
        }
        if self.mods.shift {
            parts.push("shift".to_string());
        }
        if self.mods.sup {
            parts.push("super".to_string());
        }
        parts.push(match &self.key {
            Key::Char('`') => "grave_accent".to_string(),
            Key::Char(c) => c.to_string(),
            Key::Sym(s) => xkb::keysym_get_name(Keysym::new(*s)),
        });
        parts.join("+")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_basic() {
        let c = KeyCombo::parse("ctrl+grave_accent").unwrap();
        assert!(c.mods.ctrl && !c.mods.shift);
        assert_eq!(c.key, Key::Char('`'));
        let c = KeyCombo::parse("ctrl+shift+t").unwrap();
        assert_eq!(c.key, Key::Char('t'));
        assert!(c.matches(Mods { ctrl: true, shift: true, ..Default::default() }, xkb::keysyms::KEY_t));
        let c = KeyCombo::parse("ctrl+page_down").unwrap();
        assert!(c.matches(Mods { ctrl: true, ..Default::default() }, xkb::keysyms::KEY_Next));
        assert!(KeyCombo::parse("f12").is_some());
        assert!(KeyCombo::parse("ctrl+nope").is_none());
    }
}
