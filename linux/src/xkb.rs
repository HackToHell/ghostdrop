//! xkbcommon keyboard state shared by the Wayland and X11 backends: turns
//! keycodes into everything libghostty's key encoder needs.
use crate::ghostty::mods as gmods;
use crate::keys::Mods;
use xkbcommon::xkb::{self, compose, Keycode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Debug)]
pub struct KeyInput {
    pub state: KeyState,
    /// xkb keycode (evdev + 8) — libghostty's native keycode on Linux.
    pub keycode: u32,
    /// Level-0 (unshifted) keysym, used for binding matches.
    pub unshifted: u32,
    pub sym: u32,
    pub text: String,
    pub mods: Mods,
    pub ghostty_mods: u32,
    pub consumed: u32,
    pub composing: bool,
}

struct ModIdx {
    shift: u32,
    ctrl: u32,
    alt: u32,
    sup: u32,
    caps: u32,
    num: u32,
}

pub struct Xkb {
    pub keymap: xkb::Keymap,
    pub state: xkb::State,
    compose: Option<compose::State>,
    idx: ModIdx,
    right: [bool; 4],
}

fn locale() -> String {
    for v in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(s) = std::env::var(v) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    "C".into()
}

impl Xkb {
    pub fn new(ctx: &xkb::Context, keymap: xkb::Keymap, state: xkb::State) -> Xkb {
        let get = |names: &[&str]| -> u32 {
            for n in names {
                let i = keymap.mod_get_index(*n);
                if i != xkb::MOD_INVALID {
                    return i;
                }
            }
            xkb::MOD_INVALID
        };
        let idx = ModIdx {
            shift: get(&[xkb::MOD_NAME_SHIFT]),
            ctrl: get(&[xkb::MOD_NAME_CTRL]),
            alt: get(&["Alt", xkb::MOD_NAME_ALT]),
            sup: get(&["Super", xkb::MOD_NAME_LOGO]),
            caps: get(&[xkb::MOD_NAME_CAPS]),
            num: get(&["NumLock", xkb::MOD_NAME_NUM]),
        };
        let compose = compose::Table::new_from_locale(ctx, std::ffi::OsStr::new(&locale()), compose::COMPILE_NO_FLAGS)
            .ok()
            .map(|t| compose::State::new(&t, compose::STATE_NO_FLAGS));
        Xkb { keymap, state, compose, idx, right: [false; 4] }
    }

    pub fn from_wayland(ctx: &xkb::Context, fd: std::os::fd::OwnedFd, size: u32) -> Option<Xkb> {
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS)
        }
        .ok()??;
        let state = xkb::State::new(&keymap);
        Some(Xkb::new(ctx, keymap, state))
    }

    pub fn update_mask(&mut self, dep: u32, lat: u32, lock: u32, dl: u32, ll: u32, lockl: u32) {
        self.state.update_mask(dep, lat, lock, dl, ll, lockl);
    }

    fn active(&self, i: u32) -> bool {
        i != xkb::MOD_INVALID && self.state.mod_index_is_active(i, xkb::STATE_MODS_EFFECTIVE)
    }

    fn consumed(&self, kc: Keycode, i: u32) -> bool {
        i != xkb::MOD_INVALID && self.state.mod_index_is_consumed(kc, i)
    }

    pub fn mods(&self) -> Mods {
        Mods {
            ctrl: self.active(self.idx.ctrl),
            shift: self.active(self.idx.shift),
            alt: self.active(self.idx.alt),
            sup: self.active(self.idx.sup),
        }
    }

    pub fn repeats(&self, keycode: u32) -> bool {
        self.keymap.key_repeats(Keycode::new(keycode))
    }

    pub fn reset_compose(&mut self) {
        if let Some(c) = self.compose.as_mut() {
            c.reset();
        }
    }

    /// Level-0 keysym of a keycode in the active layout.
    pub fn unshifted(&self, keycode: u32) -> u32 {
        let kc = Keycode::new(keycode);
        let layout = self.state.key_get_layout(kc);
        self.keymap.key_get_syms_by_level(kc, layout, 0).first().map_or(0, |s| s.raw())
    }

    /// Find a keycode whose unshifted keysym is `sym` (for global grabs).
    pub fn keycode_for(&self, sym: u32) -> Option<u32> {
        let want = crate::keys::keysym_lower(sym);
        let mut found = None;
        self.keymap.key_for_each(|km, kc| {
            if found.is_some() {
                return;
            }
            for s in km.key_get_syms_by_level(kc, 0, 0) {
                if crate::keys::keysym_lower(s.raw()) == want {
                    found = Some(kc.raw());
                }
            }
        });
        found
    }

    pub fn key(&mut self, keycode: u32, state: KeyState) -> KeyInput {
        let kc = Keycode::new(keycode);
        // Track sided modifiers (Shift_R, Control_R, Alt_R, Super_R).
        let side = match keycode {
            62 => Some(0),
            105 => Some(1),
            108 => Some(2),
            134 => Some(3),
            _ => None,
        };
        if let Some(i) = side {
            self.right[i] = state != KeyState::Release;
        }

        let sym = self.state.key_get_one_sym(kc);
        let mut text = String::new();
        let mut composing = false;
        if state != KeyState::Release {
            let mut handled = false;
            if let Some(c) = self.compose.as_mut() {
                if c.feed(sym) == compose::FeedResult::Accepted {
                    match c.status() {
                        compose::Status::Composing => {
                            composing = true;
                            handled = true;
                        }
                        compose::Status::Composed => {
                            text = c.utf8().unwrap_or_default();
                            c.reset();
                            handled = true;
                        }
                        compose::Status::Cancelled => {
                            c.reset();
                            handled = true;
                        }
                        compose::Status::Nothing => {}
                    }
                }
            }
            if !handled {
                text = self.state.key_get_utf8(kc);
            }
        }

        // A modifier key's own press/release must be reflected in the mods
        // (kitty protocol reports e.g. Shift press as `CSI 57441;2u`). X11
        // delivers the XKB state update after the key event, so apply it here.
        let mut mods = self.mods();
        {
            use xkb::keysyms as ks;
            let down = state != KeyState::Release;
            match sym.raw() {
                ks::KEY_Shift_L | ks::KEY_Shift_R => mods.shift = down,
                ks::KEY_Control_L | ks::KEY_Control_R => mods.ctrl = down,
                ks::KEY_Alt_L | ks::KEY_Alt_R | ks::KEY_Meta_L | ks::KEY_Meta_R => mods.alt = down,
                ks::KEY_Super_L | ks::KEY_Super_R | ks::KEY_Hyper_L | ks::KEY_Hyper_R => mods.sup = down,
                _ => {}
            }
        }
        let mut gm = 0u32;
        if mods.shift {
            gm |= gmods::SHIFT;
        }
        if mods.ctrl {
            gm |= gmods::CTRL;
        }
        if mods.alt {
            gm |= gmods::ALT;
        }
        if mods.sup {
            gm |= gmods::SUPER;
        }
        if self.active(self.idx.caps) {
            gm |= gmods::CAPS;
        }
        if self.active(self.idx.num) {
            gm |= gmods::NUM;
        }
        if self.right[0] {
            gm |= gmods::SHIFT_RIGHT;
        }
        if self.right[1] {
            gm |= gmods::CTRL_RIGHT;
        }
        if self.right[2] {
            gm |= gmods::ALT_RIGHT;
        }
        if self.right[3] {
            gm |= gmods::SUPER_RIGHT;
        }
        let mut consumed = 0u32;
        if mods.shift && self.consumed(kc, self.idx.shift) {
            consumed |= gmods::SHIFT;
        }
        if mods.alt && self.consumed(kc, self.idx.alt) {
            consumed |= gmods::ALT;
        }
        if mods.ctrl && self.consumed(kc, self.idx.ctrl) {
            consumed |= gmods::CTRL;
        }
        if mods.sup && self.consumed(kc, self.idx.sup) {
            consumed |= gmods::SUPER;
        }

        KeyInput {
            state,
            keycode,
            unshifted: self.unshifted(keycode),
            sym: sym.raw(),
            text,
            mods,
            ghostty_mods: gm,
            consumed,
            composing,
        }
    }
}
