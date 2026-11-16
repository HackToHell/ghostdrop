//! A tab = one libghostty surface (terminal) plus its latest rendered frame.
use crate::ffi::*;
use crate::ghostty::SurfaceId;
use crate::settings::Profile;
use std::ffi::{c_void, CString};

/// A frame from libghostty: CPU pixels (BGRA8, premultiplied) or a DMA-BUF
/// (top-down DRM ARGB8888, see patches/ghostty). Freed on drop, which also
/// closes the DMA-BUF fds; presenters dup what they keep.
pub struct Frame {
    handle: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub data: FrameData,
}

pub enum FrameData {
    Memory { pixels: *const u8, stride: usize, flip_y: bool },
    Dmabuf(Dmabuf),
}

#[derive(Clone, Copy, Debug)]
pub struct Dmabuf {
    pub fourcc: u32,
    pub modifier: u64,
    pub n_planes: usize,
    pub fds: [i32; 4],
    pub offsets: [u32; 4],
    pub strides: [u32; 4],
    /// Identity of the underlying buffer (inode of plane 0's dma_buf): the
    /// renderer cycles through a few, so presenters cache imports by this.
    pub ino: u64,
}

impl Frame {
    pub fn take(surface: ghostty_surface_t) -> Option<Frame> {
        let mut f = ghostty_frame_s::default();
        if !unsafe { ghostty_surface_take_frame(surface, &mut f) } {
            return None;
        }
        let data = if f.kind == GHOSTTY_FRAME_MEMORY as i32 && !f.pixels.is_null() {
            FrameData::Memory { pixels: f.pixels, stride: f.stride, flip_y: f.flip_y }
        } else if f.kind == GHOSTTY_FRAME_DMABUF as i32 && (1..=4).contains(&f.n_planes) && f.fds[0] >= 0 {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let ino = if unsafe { libc::fstat(f.fds[0], &mut st) } == 0 { st.st_ino as u64 } else { 0 };
            FrameData::Dmabuf(Dmabuf {
                fourcc: f.fourcc,
                modifier: f.modifier,
                n_planes: f.n_planes as usize,
                fds: f.fds,
                offsets: f.offsets,
                strides: f.strides,
                ino,
            })
        } else {
            unsafe { ghostty_frame_free(f.handle) };
            return None;
        };
        Some(Frame { handle: f.handle, width: f.width, height: f.height, data })
    }

    pub fn dmabuf(&self) -> Option<&Dmabuf> {
        match &self.data {
            FrameData::Dmabuf(d) => Some(d),
            FrameData::Memory { .. } => None,
        }
    }

    /// Row `y` in top-down order (CPU frames only).
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        let FrameData::Memory { pixels, stride, flip_y } = self.data else { return None };
        let src_y = if flip_y { self.height - 1 - y } else { y };
        Some(unsafe { std::slice::from_raw_parts(pixels.add(src_y as usize * stride), self.width as usize * 4) })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { ghostty_frame_free(self.handle) }
    }
}

pub struct Tab {
    pub id: SurfaceId,
    pub surface: ghostty_surface_t,
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
        app: ghostty_app_t,
        id: SurfaceId,
        profile: Profile,
        cwd: Option<String>,
        font_size: f32,
        scale: f64,
    ) -> Option<Tab> {
        let command = profile.effective_command().map(|c| CString::new(c).unwrap_or_default());
        let wd = profile.cwd.clone().or(cwd).map(|c| CString::new(c).unwrap_or_default());
        let env_k: Vec<CString> = profile.env.iter().map(|(k, _)| CString::new(k.as_str()).unwrap_or_default()).collect();
        let env_v: Vec<CString> = profile.env.iter().map(|(_, v)| CString::new(v.as_str()).unwrap_or_default()).collect();
        let mut env: Vec<ghostty_env_var_s> =
            env_k.iter().zip(&env_v).map(|(k, v)| ghostty_env_var_s { key: k.as_ptr(), value: v.as_ptr() }).collect();

        let surface = unsafe {
            let mut cfg = ghostty_surface_config_new();
            cfg.platform_tag = GHOSTTY_PLATFORM_LINUX as _;
            cfg.userdata = id as usize as *mut c_void;
            cfg.scale_factor = scale;
            cfg.font_size = font_size;
            cfg.wait_after_command = profile.wait_after_command;
            cfg.context = GHOSTTY_SURFACE_CONTEXT_TAB;
            cfg.command = command.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            cfg.working_directory = wd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            cfg.env_vars = if env.is_empty() { std::ptr::null_mut() } else { env.as_mut_ptr() };
            cfg.env_var_count = env.len();
            ghostty_surface_new(app, &cfg)
        };
        if surface.is_null() {
            return None;
        }
        // CPU frames until the core enables GPU frames (backend permitting).
        unsafe { ghostty_surface_set_gpu_frames(surface, false) };
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

    pub fn set_size(&self, w: u32, h: u32) {
        if w > 0 && h > 0 {
            unsafe { ghostty_surface_set_size(self.surface, w, h) }
        }
    }

    pub fn set_scale(&self, scale: f64) {
        unsafe { ghostty_surface_set_content_scale(self.surface, scale, scale) }
    }

    pub fn set_focus(&mut self, f: bool) {
        if self.focused != f {
            self.focused = f;
            unsafe { ghostty_surface_set_focus(self.surface, f) }
        }
        if f {
            self.bell = false;
        }
    }

    /// Ask for DMA-BUF (true) or CPU (false) frames. Cheap when unchanged.
    pub fn set_gpu(&self, on: bool) {
        if self.gpu.replace(on) != on {
            unsafe { ghostty_surface_set_gpu_frames(self.surface, on) };
            unsafe { ghostty_surface_refresh(self.surface) };
        }
    }

    /// Whether this tab currently asks for DMA-BUF frames.
    pub fn gpu(&self) -> bool {
        self.gpu.get()
    }

    pub fn set_visible(&self, v: bool) {
        unsafe { ghostty_surface_set_occlusion(self.surface, v) }
    }

    pub fn binding_action(&self, action: &str) -> bool {
        unsafe { ghostty_surface_binding_action(self.surface, action.as_ptr() as *const _, action.len()) }
    }

    pub fn request_close(&self) {
        unsafe { ghostty_surface_request_close(self.surface) }
    }

    pub fn process_exited(&self) -> bool {
        unsafe { ghostty_surface_process_exited(self.surface) }
    }

    /// Working directory to inherit for a new local tab.
    pub fn inherited_cwd(&self) -> Option<String> {
        unsafe {
            let c = ghostty_surface_inherited_config(self.surface, GHOSTTY_SURFACE_CONTEXT_TAB);
            if c.working_directory.is_null() {
                None
            } else {
                Some(std::ffi::CStr::from_ptr(c.working_directory).to_string_lossy().into_owned())
            }
        }
    }

    pub fn font_size(&self) -> f32 {
        unsafe { ghostty_surface_inherited_config(self.surface, GHOSTTY_SURFACE_CONTEXT_TAB).font_size }
    }

    pub fn grid(&self) -> (u16, u16) {
        let s = unsafe { ghostty_surface_size(self.surface) };
        (s.columns, s.rows)
    }

    pub fn visible_text(&self) -> String {
        unsafe {
            let mut t = ghostty_text_s::default();
            let sel = ghostty_selection_s {
                top_left: ghostty_point_s { tag: GHOSTTY_POINT_VIEWPORT, coord: GHOSTTY_POINT_COORD_TOP_LEFT, x: 0, y: 0 },
                bottom_right: ghostty_point_s {
                    tag: GHOSTTY_POINT_VIEWPORT,
                    coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT,
                    x: 0,
                    y: 0,
                },
                rectangle: false,
            };
            if !ghostty_surface_read_text(self.surface, sel, &mut t) {
                return String::new();
            }
            let s = std::ffi::CStr::from_ptr(t.text).to_string_lossy().into_owned();
            ghostty_surface_free_text(self.surface, &mut t);
            s
        }
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.frame = None;
        unsafe { ghostty_surface_free(self.surface) }
    }
}
