//! A tab = one libghostty surface (terminal) plus its latest rendered frame.
pub use crate::ghostty::Frame;
use crate::ghostty::{ClipContent, KeyAction, MouseButton, Runtime, Surface, SurfaceId};
use crate::settings::Profile;

pub struct Tab {
    pub id: SurfaceId,
    surface: Surface,
    pub profile: Profile,
    pub term_title: String,
    pub title_override: Option<String>,
    pub pwd: Option<String>,
    pub bell: bool,
    pub frame: Option<Frame>,
    pub background: Option<[u8; 3]>,
    pub focused: bool,
    /// Whether libghostty currently hands this tab out as DMA-BUF frames.
    gpu: std::cell::Cell<bool>,
}

impl Tab {
    pub fn new(
        rt: &Runtime,
        id: SurfaceId,
        profile: Profile,
        cwd: Option<String>,
        font_size: f32,
        scale: f64,
    ) -> Option<Tab> {
        let cmd = profile.effective_command();
        let wd = profile.cwd.clone().or(cwd);
        let surface = rt.new_surface(id, cmd.as_deref(), wd.as_deref(), &profile.env, font_size, scale, profile.wait_after_command)?;
        Some(Tab {
            id,
            surface,
            profile,
            term_title: String::new(),
            title_override: None,
            pwd: None,
            bell: false,
            frame: None,
            background: None,
            focused: false,
            gpu: std::cell::Cell::new(false),
        })
    }

    pub fn title(&self) -> String {
        if let Some(t) = self.title_override.as_ref().filter(|t| !t.is_empty()) {
            return t.clone();
        }
        if !self.term_title.is_empty() {
            return self.term_title.clone();
        }
        self.profile.title.clone().unwrap_or_else(|| self.profile.name.clone())
    }

    pub fn take_frame(&self) -> Option<Frame> {
        self.surface.take_frame()
    }

    pub fn set_size(&self, w: u32, h: u32) {
        self.surface.set_size(w, h);
    }

    pub fn set_scale(&self, scale: f64) {
        self.surface.set_scale(scale);
    }

    pub fn set_focus(&mut self, f: bool) {
        if self.focused != f {
            self.focused = f;
            self.surface.set_focus(f);
        }
        if f {
            self.bell = false;
        }
    }

    /// Ask for DMA-BUF (true) or CPU (false) frames. Cheap when unchanged.
    pub fn set_gpu(&self, on: bool) {
        if self.gpu.replace(on) != on {
            self.surface.set_gpu_frames(on);
        }
    }

    /// Whether this tab currently asks for DMA-BUF frames.
    pub fn gpu(&self) -> bool {
        self.gpu.get()
    }

    pub fn set_visible(&self, v: bool) {
        self.surface.set_visible(v);
    }

    pub fn binding_action(&self, action: &str) -> bool {
        self.surface.binding_action(action)
    }

    pub fn request_close(&self) {
        self.surface.request_close();
    }

    pub fn process_exited(&self) -> bool {
        self.surface.process_exited()
    }

    pub fn inherited_tab_config(&self) -> (Option<String>, f32) {
        self.surface.inherited_tab_config()
    }

    pub fn grid(&self) -> (u16, u16) {
        self.surface.grid()
    }

    pub fn visible_text(&self) -> String {
        self.surface.visible_text()
    }

    pub fn key(
        &self,
        action: KeyAction,
        mods: u32,
        consumed_mods: u32,
        keycode: u32,
        text: Option<&str>,
        unshifted_codepoint: u32,
        composing: bool,
    ) {
        self.surface.key(action, mods, consumed_mods, keycode, text, unshifted_codepoint, composing);
    }

    pub fn mouse_pos(&self, x: f64, y: f64, mods: u32) {
        self.surface.mouse_pos(x, y, mods);
    }

    pub fn mouse_button(&self, pressed: bool, button: MouseButton, mods: u32) {
        self.surface.mouse_button(pressed, button, mods);
    }

    pub fn mouse_scroll(&self, dx: f64, dy: f64, precise: bool) {
        self.surface.mouse_scroll(dx, dy, precise);
    }

    pub fn complete_clipboard(&self, contents: &[ClipContent], available: &[String], state: usize, confirmed: bool) {
        self.surface.complete_clipboard(contents, available, state, confirmed);
    }

    pub fn deny_clipboard(&self, state: usize) {
        self.surface.deny_clipboard(state);
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        // Drop any held frame before Surface::drop frees the libghostty surface.
        self.frame = None;
    }
}
