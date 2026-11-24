//! Wayland backend: a wlr-layer-shell surface (sway, Hyprland, KDE, river, ...).
use crate::core::{Button, Core, Cursor, Placement, PointerInput, Pos, Window};
use crate::draw::Canvas;
use crate::ghostty::{self, Clip};
use crate::ipc;
use crate::xkb::{KeyState, Xkb};
use calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
        DataDeviceManagerState, WritePipe,
    },
    delegate_registry,
    dispatch2::Dispatch2,
    dmabuf::{DmabufFeedback, DmabufHandler, DmabufState},
    subcompositor::SubcompositorState,
    output::{OutputHandler, OutputState},
    primary_selection::{
        device::{PrimarySelectionDevice, PrimarySelectionDeviceHandler},
        selection::{PrimarySelectionSource, PrimarySelectionSourceHandler},
        PrimarySelectionManagerState,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use crate::tab::Frame;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_subsurface::WlSubsurface};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
};
use std::time::{Duration, Instant};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_keyboard, wl_output,
        wl_pointer, wl_seat, wl_shm, wl_surface,
    },
    Connection, QueueHandle, WEnum,
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1, zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, wp_fractional_scale_v1,
    wp_fractional_scale_v1::WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use xkbcommon::xkb;

const TEXT_MIMES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];
/// A lost frame callback must not stall redraws or GPU presents forever.
const FRAME_TIMEOUT: Duration = Duration::from_millis(250);
/// Compositor must answer zwp_linux_buffer_params_v1.create within this window.
const DMABUF_CREATE_TIMEOUT: Duration = Duration::from_millis(500);
/// Per-step timeout for async Wayland clipboard pipe reads/writes.
const CLIP_READ_STEP_TIMEOUT: Duration = Duration::from_secs(2);
const CLIP_WRITE_STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// Zero-copy presentation: DMA-BUF frames become wl_buffers
/// (linux-dmabuf) on a subsurface over the terminal area.
struct WlGpu {
    sub: Option<GpuSurface>,
    buffers: Vec<(GpuKey, GpuBuf, u64)>,
    clock: u64,
    frame_pending: Option<Instant>,
}

type GpuKey = (u64, u32, u32, u32, u64);

enum GpuBuf {
    /// `create` sent; the compositor answers with created/failed.
    Pending { params: ZwpLinuxBufferParamsV1, created_at: Instant },
    Ready(WlBuffer),
}

struct GpuSurface {
    subsurface: WlSubsurface,
    surface: wl_surface::WlSurface,
    viewport: Option<WpViewport>,
    mapped: bool,
}

impl WlGpu {
    fn drop_surface(&mut self) {
        if let Some(s) = self.sub.take() {
            if let Some(v) = s.viewport {
                v.destroy();
            }
            s.subsurface.destroy();
            s.surface.destroy();
        }
        for (_, b, _) in self.buffers.drain(..) {
            match b {
                GpuBuf::Pending { params, .. } => params.destroy(),
                GpuBuf::Ready(b) => b.destroy(),
            }
        }
        self.frame_pending = None;
    }

    fn frame_pending(&self) -> bool {
        self.frame_pending.is_some_and(|t| t.elapsed() < FRAME_TIMEOUT)
    }
}

const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241;
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub struct WlWin {
    conn: Connection,
    qh: QueueHandle<WlState>,
    handle: LoopHandle<'static, WlState>,
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: Option<SlotPool>,
    ddm: Option<DataDeviceManagerState>,
    psm: Option<PrimarySelectionManagerState>,
    data_device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    copy_source: Option<CopyPasteSource>,
    primary_source: Option<PrimarySelectionSource>,
    clip_text: String,
    primary_text: String,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    xkb_ctx: xkb::Context,
    xkb: Option<Xkb>,
    repeat: (i32, i32),
    repeat_key: Option<u32>,
    repeat_token: Option<RegistrationToken>,
    layer: Option<LayerSurface>,
    configured: bool,
    visible: bool,
    kb_focus: bool,
    logical: (u32, u32),
    scale: i32,
    dmabuf: DmabufState,
    subcompositor: Option<SubcompositorState>,
    gpu: Option<WlGpu>,
    /// GPU setup from the compositor's dmabuf feedback is done (or moot).
    gpu_probed: bool,
    /// A new GPU frame waits to be presented.
    needs_present: bool,
    /// Mapping is waiting for the output-size probe (see `show`).
    probe: Option<Placement>,
    /// Logical size of the output the compositor put this surface on.
    known_output: Option<(f64, f64)>,
    /// wp_fractional_scale_v1 preferred scale (x120); 0 = not in use.
    scale120: u32,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    frac: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    frame_pending: Option<Instant>,
    needs_redraw: bool,
    serial: u32,
    clip_results: Vec<(u64, Option<String>)>,
    /// In-flight clipboard read pipes: token -> (calloop source, step deadline).
    active_reads: HashMap<u64, (RegistrationToken, Instant)>,
    /// In-flight clipboard write pipes: id -> (calloop source, step deadline).
    active_writes: HashMap<u64, (RegistrationToken, Instant)>,
    next_write_id: u64,
    /// Keyboard interactivity of the mapped layer surface.
    kbd: Kbd,
    /// The compositor drops focus from on-demand layer surfaces (sway 1.12
    /// does so on every layer re-arrange), so stay exclusive. Learned at
    /// runtime, the first time focus vanishes right after relaxing.
    ondemand_broken: bool,
    /// `quick-terminal-keyboard-interactivity = exclusive` at the last show.
    exclusive_cfg: bool,
    /// Last keyboard leave (for focus-flicker detection).
    kb_left_at: Option<Instant>,
    pointer_inside: bool,
    pointer_left_at: Option<Instant>,
}

pub struct WlState {
    pub win: WlWin,
    pub core: Core,
}

/// Keyboard interactivity. Layer surfaces can't ask for focus, so we map (and
/// re-focus) with Exclusive, which makes the compositor give it to us, and
/// relax to OnDemand once it arrives so clicking another window takes it away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kbd {
    /// Exclusive until keyboard focus arrives, then OnDemand.
    Claiming,
    /// OnDemand. `relaxed_at`: just relaxed from Claiming; losing focus right
    /// away means this compositor drops on-demand focus.
    OnDemand { relaxed_at: Option<Instant> },
    /// Exclusive for good: configured, or on-demand doesn't work here.
    Exclusive,
}

/// Focus lost this soon after relaxing to on-demand means the compositor
/// drops on-demand focus. (Waiting for a second drop to rule out a quick
/// click elsewhere gives sway's focus-follows-mouse a mapping in which
/// hovering another window hides the drop-down.)
const ONDEMAND_PROBATION: Duration = Duration::from_millis(150);

/// Our own wl_keyboard handler (we need xkb's consumed modifiers, which
/// SCTK's keyboard helper doesn't expose).
pub struct KbData;

fn layer_kind(p: &Placement) -> Layer {
    if p.overlay_layer {
        Layer::Overlay
    } else {
        Layer::Top
    }
}

impl WlWin {
    /// Fractional scaling is active (exact device pixels via a viewport).
    fn fractional(&self) -> bool {
        self.scale120 > 0 && self.viewport.is_some()
    }

    fn scale_f(&self) -> f64 {
        if self.fractional() {
            self.scale120 as f64 / 120.0
        } else {
            self.scale as f64
        }
    }

    fn physical(&self) -> (u32, u32) {
        if self.fractional() {
            // Per wp_fractional_scale_v1: buffer = logical * scale, rounded half away from zero.
            let s = self.scale_f();
            ((self.logical.0 as f64 * s).round() as u32, (self.logical.1 as f64 * s).round() as u32)
        } else {
            (self.logical.0 * self.scale as u32, self.logical.1 * self.scale as u32)
        }
    }

    fn output_sizes(&self) -> Vec<(f64, f64)> {
        self.output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .filter_map(|i| i.logical_size.map(|(w, h)| (w as f64, h as f64)))
            .collect()
    }

    /// Logical size of the output we're on (or will most likely appear on).
    fn output_size(&self) -> Option<(f64, f64)> {
        self.known_output.or_else(|| self.output_sizes().first().copied())
    }

    fn configure_layer(&self, layer: &LayerSurface, p: &Placement) {
        let (sw, sh) = self.output_size().unwrap_or((1920.0, 1080.0));
        let (w, h) = p.resolve(sw, sh);
        let (anchor, size) = match p.pos {
            Pos::Top => (Anchor::TOP | if w.is_none() { Anchor::LEFT | Anchor::RIGHT } else { Anchor::empty() }, (w.unwrap_or(0.0), h.unwrap_or(sh))),
            Pos::Bottom => (Anchor::BOTTOM | if w.is_none() { Anchor::LEFT | Anchor::RIGHT } else { Anchor::empty() }, (w.unwrap_or(0.0), h.unwrap_or(sh))),
            Pos::Left => (Anchor::LEFT | if h.is_none() { Anchor::TOP | Anchor::BOTTOM } else { Anchor::empty() }, (w.unwrap_or(sw), h.unwrap_or(0.0))),
            Pos::Right => (Anchor::RIGHT | if h.is_none() { Anchor::TOP | Anchor::BOTTOM } else { Anchor::empty() }, (w.unwrap_or(sw), h.unwrap_or(0.0))),
            Pos::Center => (Anchor::empty(), (w.unwrap_or(800.0), h.unwrap_or(400.0))),
        };
        let anchor = if p.fullscreen { Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT } else { anchor };
        let size = if p.fullscreen { (0.0, 0.0) } else { size };
        layer.set_anchor(anchor);
        layer.set_size(size.0.round() as u32, size.1.round() as u32);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(self.interactivity());
    }

    fn interactivity(&self) -> KeyboardInteractivity {
        match self.kbd {
            Kbd::OnDemand { .. } => KeyboardInteractivity::OnDemand,
            Kbd::Claiming | Kbd::Exclusive => KeyboardInteractivity::Exclusive,
        }
    }

    /// Keyboard state for a placement: exclusive if configured (or on-demand
    /// is broken here); otherwise keep what we have. Re-placing a visible
    /// window (reload, fullscreen) must not grab focus from another window.
    fn kbd_for(&self, p: &Placement) -> Kbd {
        if p.exclusive_keyboard || self.ondemand_broken {
            return Kbd::Exclusive;
        }
        match self.kbd {
            // It was exclusive only because the config said so.
            Kbd::Exclusive if self.exclusive_cfg => Kbd::OnDemand { relaxed_at: None },
            k => k,
        }
    }

    /// Put the GPU subsurface over the terminal (`rect`, physical px) or
    /// take it away (`None`: CPU frames). Its position is parent state, so
    /// call this before committing the main surface.
    fn place_gpu_surface(&mut self, rect: Option<(u32, u32, u32, u32)>) {
        let scale = self.scale_f();
        let Some(gpu) = self.gpu.as_mut() else { return };
        let Some(r) = rect else {
            if let Some(s) = gpu.sub.as_mut().filter(|s| s.mapped) {
                s.surface.attach(None, 0, 0);
                s.surface.commit();
                s.mapped = false;
            }
            return;
        };
        let Some(layer) = self.layer.as_ref() else { return };
        if gpu.sub.is_none() {
            let Some(sc) = self.subcompositor.as_ref() else { return };
            let (subsurface, surface) = sc.create_subsurface(layer.wl_surface().clone(), &self.qh);
            // Frames arrive at their own pace; don't tie them to the parent.
            subsurface.set_desync();
            // Pointer input goes to the main surface underneath.
            if let Ok(region) = smithay_client_toolkit::compositor::Region::new(&self.compositor) {
                surface.set_input_region(Some(region.wl_region()));
            }
            let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, &self.qh, NoEvents));
            gpu.sub = Some(GpuSurface { subsurface, surface, viewport, mapped: false });
        }
        let s = gpu.sub.as_ref().unwrap();
        s.subsurface.set_position((r.0 as f64 / scale).round() as i32, (r.1 as f64 / scale).round() as i32);
    }

    /// Present a DMA-BUF frame on the subsurface. Ok(false) = try again
    /// later (previous frame not shown yet, or the buffer is being created).
    fn present_gpu(&mut self, frame: &Frame, rect: (u32, u32, u32, u32)) -> Result<bool, String> {
        let fractional = self.fractional();
        let (scale, iscale) = (self.scale_f(), self.scale);
        let Some(gpu) = self.gpu.as_mut() else { return Ok(false) };
        let Some(d) = frame.dmabuf() else { return Ok(true) };
        if gpu.frame_pending() || gpu.sub.is_none() {
            return Ok(false);
        }
        // Wait for a frame that fits (resizes take a frame to catch up).
        if frame.width > rect.2 || frame.height > rect.3 {
            return Ok(true);
        }
        gpu.clock += 1;
        let key: GpuKey = (d.ino, frame.width, frame.height, d.strides[0], d.modifier);
        let buffer = match gpu.buffers.iter_mut().find(|b| b.0 == key) {
            Some((_, GpuBuf::Ready(b), used)) => {
                *used = gpu.clock;
                b.clone()
            }
            Some((_, GpuBuf::Pending { created_at, .. }, _)) => {
                if created_at.elapsed() >= DMABUF_CREATE_TIMEOUT {
                    return Err("compositor did not respond to dmabuf buffer creation".into());
                }
                return Ok(false);
            }
            None => {
                let params = self.dmabuf.create_params(&self.qh).map_err(|e| e.to_string())?;
                for i in 0..d.n_planes {
                    let fd = unsafe { BorrowedFd::borrow_raw(d.fds[i]) };
                    params.add(fd, i as u32, d.offsets[i], d.strides[i], d.modifier);
                }
                let p = params.create(frame.width as i32, frame.height as i32, d.fourcc, zwp_linux_buffer_params_v1::Flags::empty());
                crate::core::trace!(
                    "gpu: creating {}x{} fourcc={:#x} modifier={:#x} planes={}",
                    frame.width, frame.height, d.fourcc, d.modifier, d.n_planes
                );
                gpu.buffers.push((key, GpuBuf::Pending { params: p, created_at: Instant::now() }, gpu.clock));
                if gpu.buffers.len() > 6 {
                    let (i, _) = gpu.buffers.iter().enumerate().min_by_key(|(_, b)| b.2).unwrap();
                    match gpu.buffers.remove(i).1 {
                        GpuBuf::Pending { params, .. } => params.destroy(),
                        GpuBuf::Ready(b) => b.destroy(),
                    }
                }
                return Ok(false);
            }
        };
        let s = gpu.sub.as_mut().unwrap();
        s.surface.attach(Some(&buffer), 0, 0);
        match s.viewport.as_ref().filter(|_| fractional) {
            Some(vp) => {
                s.surface.set_buffer_scale(1);
                vp.set_destination(
                    ((frame.width as f64 / scale).round() as i32).max(1),
                    ((frame.height as f64 / scale).round() as i32).max(1),
                );
            }
            None => s.surface.set_buffer_scale(iscale),
        }
        s.surface.damage_buffer(0, 0, frame.width as i32, frame.height as i32);
        s.surface.frame(&self.qh, smithay_client_toolkit::compositor::FrameCallbackData(s.surface.clone()));
        s.surface.commit();
        s.mapped = true;
        gpu.frame_pending = Some(Instant::now());
        Ok(true)
    }

    fn frame_pending(&self) -> bool {
        self.frame_pending.is_some_and(|t| t.elapsed() < FRAME_TIMEOUT)
    }

    /// Expire hung clipboard read/write pipes (dead or paused owner/reader).
    fn expire_io(&mut self) {
        let now = Instant::now();
        let stale_reads: Vec<u64> = self.active_reads.iter().filter(|(_, (_, d))| now >= *d).map(|(&k, _)| k).collect();
        for token in stale_reads {
            if let Some((reg, _)) = self.active_reads.remove(&token) {
                self.handle.remove(reg);
                crate::core::trace!("wayland clipboard: read {token} owner timed out");
                self.clip_results.push((token, None));
            }
        }
        let stale_writes: Vec<u64> = self.active_writes.iter().filter(|(_, (_, d))| now >= *d).map(|(&k, _)| k).collect();
        for wid in stale_writes {
            if let Some((reg, _)) = self.active_writes.remove(&wid) {
                self.handle.remove(reg);
                crate::core::trace!("wayland clipboard: write {wid} reader timed out");
            }
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let mut best: Option<Instant> = None;
        let mut consider = |d: Instant| {
            best = Some(best.map_or(d, |b| b.min(d)));
        };
        for (_, d) in self.active_reads.values() {
            consider(*d);
        }
        for (_, d) in self.active_writes.values() {
            consider(*d);
        }
        if self.needs_redraw {
            if let Some(t) = self.frame_pending {
                consider(t + FRAME_TIMEOUT);
            }
        }
        if self.needs_present {
            if let Some(g) = self.gpu.as_ref() {
                if let Some(t) = g.frame_pending {
                    consider(t + FRAME_TIMEOUT);
                }
                for (_, buf, _) in &g.buffers {
                    if let GpuBuf::Pending { created_at, .. } = buf {
                        consider(*created_at + DMABUF_CREATE_TIMEOUT);
                    }
                }
            }
        }
        best
    }

    fn stop_repeat(&mut self) {
        self.repeat_key = None;
        if let Some(t) = self.repeat_token.take() {
            self.handle.remove(t);
        }
    }

    fn mods(&self) -> crate::keys::Mods {
        self.xkb.as_ref().map(|x| x.mods()).unwrap_or_default()
    }
}

impl Window for WlWin {
    fn show(&mut self, p: &Placement) {
        if let Some(layer) = self.layer.as_ref() {
            if self.probe.is_some() {
                // Still sizing for the output: the probe's configure applies
                // the latest placement.
                self.probe = Some(*p);
                return;
            }
            self.kbd = self.kbd_for(p);
            self.exclusive_cfg = p.exclusive_keyboard;
            self.configure_layer(layer, p);
            layer.commit();
            return;
        }
        self.kbd = if p.exclusive_keyboard || self.ondemand_broken { Kbd::Exclusive } else { Kbd::Claiming };
        self.exclusive_cfg = p.exclusive_keyboard;
        let surface = self.compositor.create_surface(&self.qh);
        if let (Some(m), Some(v)) = (self.frac_mgr.as_ref(), self.viewporter.as_ref()) {
            self.frac = Some(m.get_fractional_scale(&surface, &self.qh, FracData));
            self.viewport = Some(v.get_viewport(&surface, &self.qh, NoEvents));
        }
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, layer_kind(p), Some("ghostdrop"), None);
        self.known_output = None;
        let sizes = self.output_sizes();
        if !p.fullscreen && sizes.windows(2).any(|w| w[0] != w[1]) {
            // Outputs differ and layer-shell won't say which one we'll get.
            // Ask for the whole output first; the configure reveals its size,
            // then size for real before the first buffer (one round trip).
            self.configure_layer(&layer, p);
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_size(0, 0);
            layer.set_exclusive_zone(-1);
            self.probe = Some(*p);
        } else {
            self.configure_layer(&layer, p);
        }
        layer.commit();
        self.layer = Some(layer);
        self.kb_left_at = None;
        self.visible = true;
        self.configured = false;
        self.frame_pending = None;
    }

    fn hide(&mut self) {
        self.stop_repeat();
        // libghostty drops its GPU buffers while hidden; let go of ours too.
        if let Some(g) = self.gpu.as_mut() {
            g.drop_surface();
        }
        self.needs_present = false;
        if let Some(f) = self.frac.take() {
            f.destroy();
        }
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
        self.layer = None;
        self.visible = false;
        self.configured = false;
        self.kb_focus = false;
        self.frame_pending = None;
        self.pointer_inside = false;
        self.kbd = Kbd::Claiming;
        self.kb_left_at = None;
        self.probe = None;
        self.known_output = None;
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn is_focused(&self) -> bool {
        self.kb_focus
    }

    fn focus(&mut self) {
        if self.kb_focus {
            return;
        }
        let Some(layer) = self.layer.as_ref() else { return };
        // Exclusive makes the compositor hand us the keyboard; Claiming
        // relaxes back to on-demand once it does.
        if self.kbd != Kbd::Exclusive {
            self.kbd = Kbd::Claiming;
        }
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
    }

    fn disable_gpu(&mut self, why: &str) {
        if let Some(mut g) = self.gpu.take() {
            eprintln!("ghostdrop: GPU presentation off ({why}); using CPU frames");
            g.drop_surface();
        }
        self.needs_present = false;
        self.needs_redraw = true;
    }

    fn scale(&self) -> f64 {
        self.scale_f()
    }

    fn size(&self) -> (u32, u32) {
        self.physical()
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    fn request_present(&mut self) {
        if self.gpu.is_some() {
            self.needs_present = true;
        } else {
            self.needs_redraw = true;
        }
    }

    fn set_cursor(&mut self, c: Cursor) {
        let icon = match c {
            Cursor::Default => CursorIcon::Default,
            Cursor::Text => CursorIcon::Text,
            Cursor::Pointer => CursorIcon::Pointer,
            Cursor::Crosshair => CursorIcon::Crosshair,
            Cursor::ResizeH => CursorIcon::EwResize,
            Cursor::ResizeV => CursorIcon::NsResize,
            Cursor::Grab => CursorIcon::Grab,
            Cursor::Grabbing => CursorIcon::Grabbing,
            Cursor::NotAllowed => CursorIcon::NotAllowed,
            Cursor::Wait => CursorIcon::Wait,
            Cursor::Hidden => {
                if let Some(p) = self.pointer.as_ref() {
                    let _ = p.hide_cursor();
                }
                return;
            }
        };
        if let Some(p) = self.pointer.as_ref() {
            let _ = p.set_cursor(&self.conn, icon);
        }
    }

    fn set_clipboard(&mut self, clip: Clip, text: String) {
        crate::core::trace!(
            "set_clipboard {clip:?} ({} bytes) ddm={} dev={} psm={} pdev={} serial={}",
            text.len(),
            self.ddm.is_some(),
            self.data_device.is_some(),
            self.psm.is_some(),
            self.primary_device.is_some(),
            self.serial
        );
        match clip {
            Clip::Standard => {
                let (Some(ddm), Some(dev)) = (self.ddm.as_ref(), self.data_device.as_ref()) else { return };
                let src = ddm.create_copy_paste_source(&self.qh, TEXT_MIMES.to_vec());
                src.set_selection(dev, self.serial);
                self.copy_source = Some(src);
                self.clip_text = text;
            }
            Clip::Primary => {
                let (Some(psm), Some(dev)) = (self.psm.as_ref(), self.primary_device.as_ref()) else { return };
                let src = psm.create_selection_source(&self.qh, TEXT_MIMES.to_vec());
                src.set_selection(dev, self.serial);
                self.primary_source = Some(src);
                self.primary_text = text;
            }
        }
    }

    fn read_clipboard(&mut self, clip: Clip, token: u64) {
        // Our own selection: answer directly (no round trip through the compositor).
        match clip {
            Clip::Standard if self.copy_source.is_some() => {
                self.clip_results.push((token, Some(self.clip_text.clone())));
                return;
            }
            Clip::Primary if self.primary_source.is_some() => {
                self.clip_results.push((token, Some(self.primary_text.clone())));
                return;
            }
            _ => {}
        }
        let pick = |mimes: &[String]| -> Option<String> {
            TEXT_MIMES.iter().find(|m| mimes.iter().any(|x| x == *m)).map(|m| m.to_string())
        };
        let pipe = match clip {
            Clip::Standard => self.data_device.as_ref().and_then(|d| d.data().selection_offer()).and_then(|o| {
                let mime = o.with_mime_types(pick)?;
                o.receive(mime).ok()
            }),
            Clip::Primary => self.primary_device.as_ref().and_then(|d| d.data().selection_offer()).and_then(|o| {
                let mime = o.with_mime_types(pick)?;
                o.receive(mime).ok()
            }),
        };
        let Some(pipe) = pipe else {
            self.clip_results.push((token, None));
            return;
        };
        let _ = self.conn.flush();
        let fd: OwnedFd = pipe.into();
        let file = std::fs::File::from(fd);
        let _ = rustix::fs::fcntl_setfl(&file, rustix::fs::OFlags::NONBLOCK);
        let mut data = Vec::new();
        let res = self.handle.insert_source(Generic::new(file, Interest::READ, Mode::Level), move |_, f, st| {
            let f: &mut std::fs::File = unsafe { f.get_mut() };
            let mut buf = [0u8; 65536];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => {
                        st.win.active_reads.remove(&token);
                        let text = String::from_utf8_lossy(&data).into_owned();
                        st.core.clipboard_result(&mut st.win, token, Some(text));
                        return Ok(PostAction::Remove);
                    }
                    Ok(n) => {
                        if data.len().saturating_add(n) > crate::core::MAX_CLIPBOARD_BYTES {
                            crate::core::trace!("wayland clipboard: read {token} exceeded {} bytes; aborting", crate::core::MAX_CLIPBOARD_BYTES);
                            st.win.active_reads.remove(&token);
                            st.core.clipboard_result(&mut st.win, token, None);
                            return Ok(PostAction::Remove);
                        }
                        data.extend_from_slice(&buf[..n]);
                        if let Some((_, d)) = st.win.active_reads.get_mut(&token) {
                            *d = Instant::now() + CLIP_READ_STEP_TIMEOUT;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        st.win.active_reads.remove(&token);
                        st.core.clipboard_result(&mut st.win, token, None);
                        return Ok(PostAction::Remove);
                    }
                }
            }
        });
        match res {
            Ok(reg) => {
                self.active_reads.insert(token, (reg, Instant::now() + CLIP_READ_STEP_TIMEOUT));
            }
            Err(_) => {
                self.clip_results.push((token, None));
            }
        }
    }

    fn backend_name(&self) -> &'static str {
        "wayland"
    }

    fn hotkey_status(&self) -> String {
        "wayland: bind the hotkey in your compositor, e.g. sway: bindsym ctrl+grave exec ghostdrop".into()
    }
}

impl WlState {
    /// Present a frame if one is wanted and the compositor is ready for it.
    fn after_dispatch(&mut self) {
        self.win.expire_io();
        for _ in 0..8 {
            let results = std::mem::take(&mut self.win.clip_results);
            let had_results = !results.is_empty();
            for (token, text) in results {
                self.core.clipboard_result(&mut self.win, token, text);
            }
            let had_events = ghostty::has_pending();
            if had_events {
                self.core.process(&mut self.win);
            }
            if self.core.quit || (!had_results && !had_events && self.win.clip_results.is_empty()) {
                break;
            }
        }
        if self.core.quit {
            return;
        }
        if !self.win.clip_results.is_empty() || ghostty::has_pending() {
            ghostty::wake();
        }
        self.redraw();
        self.present();
    }

    /// Present a pending GPU frame on the subsurface.
    fn present(&mut self) {
        let w = &mut self.win;
        if !(w.needs_present && w.visible && w.configured && w.gpu.is_some()) {
            return;
        }
        let size = w.physical();
        match self.core.gpu_frame(size, w.scale_f()) {
            None => w.needs_present = false,
            Some((frame, rect)) => match w.present_gpu(frame, rect) {
                Ok(true) => w.needs_present = false,
                Ok(false) => {}
                Err(e) => self.core.gpu_failed(w, &e),
            },
        }
    }

    /// Compose and commit the main surface (chrome, or everything on CPU frames).
    fn redraw(&mut self) {
        let w = &mut self.win;
        if !(w.needs_redraw && w.visible && w.configured && !w.frame_pending()) {
            return;
        }
        let Some(layer) = w.layer.as_ref() else { return };
        let (pw, ph) = w.physical();
        let scale = w.scale_f();
        if pw == 0 || ph == 0 {
            return;
        }
        let stride = pw as i32 * 4;
        let need = (stride * ph as i32) as usize;
        if w.pool.as_ref().is_none_or(|p| p.len() < need * 2) {
            w.pool = SlotPool::new(need * 2, &w.shm).ok();
        }
        let Some(pool) = w.pool.as_mut() else { return };
        let Ok((buffer, canvas)) = pool.create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Argb8888) else { return };
        let mut c = Canvas { buf: canvas, width: pw as usize, height: ph as usize, stride: stride as usize };
        self.core.draw((pw, ph), scale, &mut c);
        let surface = layer.wl_surface();
        match w.viewport.as_ref().filter(|_| w.fractional()) {
            Some(vp) => {
                surface.set_buffer_scale(1);
                vp.set_destination(w.logical.0 as i32, w.logical.1 as i32);
            }
            None => surface.set_buffer_scale(w.scale),
        }
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        // The terminal comes from the GPU subsurface when the frame is a
        // DMA-BUF; position it (parent state) before this commit.
        let gpu_rect = w.gpu.as_ref().and(self.core.gpu_frame((pw, ph), scale)).map(|(_, r)| r);
        let surface = surface.clone();
        let layer = layer.clone();
        w.place_gpu_surface(gpu_rect);
        if buffer.attach_to(&surface).is_ok() {
            surface.frame(&w.qh, smithay_client_toolkit::compositor::FrameCallbackData(surface.clone()));
            layer.commit();
            w.frame_pending = Some(Instant::now());
            w.needs_redraw = false;
            if gpu_rect.is_some() {
                w.needs_present = true;
            }
        }
    }
}

pub fn run(mut core_builder: impl FnMut() -> Core, show_now: bool) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
    let (globals, queue) = registry_queue_init::<WlState>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
    let qh = queue.handle();
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| "compositor has no wlr-layer-shell".to_string())?;
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh).ok();
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;

    let mut event_loop: calloop::EventLoop<'static, WlState> = calloop::EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    calloop_wayland_source::WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| format!("wayland source: {e}"))?;

    let (ping, ping_src) = calloop::ping::make_ping().map_err(|e| e.to_string())?;
    ghostty::set_waker(ping);
    let sig_fd = crate::install_signal_handlers().map_err(|e| format!("signal fd: {e}"))?;
    handle
        .insert_source(Generic::new(sig_fd, Interest::READ, Mode::Level), |_, f, st: &mut WlState| {
            let mut buf = [0u8; 8];
            let _ = rustix::io::read(&*f, &mut buf);
            if crate::quit_requested() {
                st.core.quit = true;
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.to_string())?;
    handle
        .insert_source(ping_src, |_, _, st: &mut WlState| {
            if crate::quit_requested() {
                st.core.quit = true;
            }
            st.core.tick(&mut st.win);
        })
        .map_err(|e| e.to_string())?;

    let listener = ipc::listen().map_err(|e| format!("control socket: {e}"))?;
    handle
        .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, l, st: &mut WlState| {
            for _ in 0..16 {
                let Ok((stream, _)) = l.accept() else { break };
                if let Some(cmd) = ipc::read_command(&stream) {
                    let reply = st.core.command(&mut st.win, &cmd);
                    ipc::reply(&stream, &reply);
                }
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.to_string())?;

    let win = WlWin {
        conn: conn.clone(),
        qh: qh.clone(),
        handle: handle.clone(),
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        pool: None,
        ddm: DataDeviceManagerState::bind(&globals, &qh).ok(),
        psm: PrimarySelectionManagerState::bind(&globals, &qh).ok(),
        data_device: None,
        primary_device: None,
        copy_source: None,
        primary_source: None,
        clip_text: String::new(),
        primary_text: String::new(),
        keyboard: None,
        pointer: None,
        xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
        xkb: None,
        repeat: (25, 600),
        repeat_key: None,
        repeat_token: None,
        layer: None,
        configured: false,
        visible: false,
        kb_focus: false,
        logical: (0, 0),
        scale: 1,
        scale120: 0,
        dmabuf: DmabufState::new(&globals, &qh),
        subcompositor,
        gpu: None,
        gpu_probed: false,
        needs_present: false,
        probe: None,
        known_output: None,
        frac_mgr: globals.bind::<WpFractionalScaleManagerV1, WlState, NoEvents>(&qh, 1..=1, NoEvents).ok(),
        viewporter: globals.bind::<WpViewporter, WlState, NoEvents>(&qh, 1..=1, NoEvents).ok(),
        frac: None,
        viewport: None,
        frame_pending: None,
        needs_redraw: false,
        serial: 0,
        clip_results: Vec::new(),
        active_reads: HashMap::new(),
        active_writes: HashMap::new(),
        next_write_id: 1,
        kbd: Kbd::Claiming,
        ondemand_broken: false,
        exclusive_cfg: false,
        kb_left_at: None,
        pointer_inside: false,
        pointer_left_at: None,
    };
    let mut st = WlState { win, core: core_builder() };
    // Zero-copy presentation needs to know the compositor's GPU and formats
    // before the first surface exists (libghostty renders on that GPU).
    let gpu_wanted = std::env::var("GHOSTDROP_GPU").as_deref() != Ok("0")
        && st.win.subcompositor.is_some()
        && st.win.dmabuf.version().is_some_and(|v| v >= 4);
    if gpu_wanted {
        let _ = st.win.dmabuf.get_default_feedback(&qh);
    } else {
        st.win.gpu_probed = true;
    }
    let _ = st.win.conn.flush();
    // Let outputs/seats (and dmabuf feedback) arrive before sizing anything.
    let deadline = Instant::now() + Duration::from_millis(300);
    event_loop.dispatch(Some(Duration::from_millis(50)), &mut st).map_err(|e| e.to_string())?;
    while !st.win.gpu_probed && Instant::now() < deadline {
        event_loop.dispatch(Some(Duration::from_millis(20)), &mut st).map_err(|e| e.to_string())?;
    }
    st.core.prespawn(&mut st.win);
    if show_now {
        st.core.show(&mut st.win);
    }
    while !st.core.quit {
        let deadline = match (st.core.next_deadline(), st.win.next_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timeout = deadline
            .map(|d| d.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or(Duration::from_millis(500))
            .min(Duration::from_millis(500));
        event_loop.dispatch(Some(timeout), &mut st).map_err(|e| format!("Wayland connection lost ({e}); exiting"))?;
        st.core.poll_timers(&mut st.win);
        st.after_dispatch();
        let _ = st.win.conn.flush();
    }
    crate::disarm_signal_fd();
    st.core.tabs.clear();
    ipc::cleanup();
    Ok(())
}

// ---------------------------------------------------------------- handlers

impl CompositorHandler for WlState {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        if self.win.layer.as_ref().is_none_or(|l| l.wl_surface() != surface) {
            return;
        }
        if factor != self.win.scale && factor > 0 {
            self.win.scale = factor;
            self.core.resized(&mut self.win);
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(g) = self.win.gpu.as_mut() {
            if g.sub.as_ref().is_some_and(|s| &s.surface == surface) {
                g.frame_pending = None;
                return;
            }
        }
        if self.win.layer.as_ref().is_some_and(|l| l.wl_surface() == surface) {
            self.win.frame_pending = None;
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for WlState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.win.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for WlState {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.win.layer.as_ref().is_none_or(|l| l != layer) {
            return;
        }
        self.core.hide(&mut self.win);
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, cfg: LayerSurfaceConfigure, _: u32) {
        if self.win.layer.as_ref().is_none_or(|l| l != layer) {
            return;
        }
        let (w, h) = cfg.new_size;
        if let Some(p) = self.win.probe.take() {
            crate::core::trace!("output probe: {w}x{h}");
            if w > 0 && h > 0 {
                self.win.known_output = Some((w as f64, h as f64));
            }
            self.win.kbd = self.win.kbd_for(&p);
            self.win.exclusive_cfg = p.exclusive_keyboard;
            if let Some(layer) = self.win.layer.as_ref() {
                self.win.configure_layer(layer, &p);
                layer.commit();
            }
            return;
        }
        let size = (w.max(1), h.max(1));
        let first = !self.win.configured;
        let changed = size != self.win.logical;
        self.win.logical = size;
        self.win.configured = true;
        self.win.needs_redraw = true;
        if changed || first {
            self.core.resized(&mut self.win);
        }
    }
}

impl SeatHandler for WlState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.win.seat_state
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.ensure_data_devices(qh, &seat);
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        // Seats that existed at startup don't get new_seat; set clipboard up here too.
        self.ensure_data_devices(qh, &seat);
        if cap == Capability::Keyboard && self.win.keyboard.is_none() {
            self.win.keyboard = Some(seat.get_keyboard(qh, KbData));
        }
        if cap == Capability::Pointer && self.win.pointer.is_none() {
            let surface = self.win.compositor.create_surface(qh);
            let shm = self.win.shm.wl_shm().clone();
            self.win.pointer =
                self.win.seat_state.get_pointer_with_theme::<WlState, ()>(qh, &seat, &shm, surface, ThemeSpec::default()).ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            self.win.stop_repeat();
            if let Some(k) = self.win.keyboard.take() {
                k.release();
            }
            if self.win.kb_focus {
                self.win.kb_focus = false;
                self.win.kb_left_at = Some(Instant::now());
                self.core.focus_changed(&mut self.win, false);
            }
        }
        if cap == Capability::Pointer {
            self.win.pointer = None;
            if self.win.pointer_inside {
                self.win.pointer_inside = false;
                self.win.pointer_left_at = Some(Instant::now());
                let mods = self.win.mods();
                self.core.pointer(&mut self.win, PointerInput::Leave, mods);
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl WlState {
    fn ensure_data_devices(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        if self.win.data_device.is_none() {
            if let Some(ddm) = self.win.ddm.as_ref() {
                self.win.data_device = Some(ddm.get_data_device(qh, seat));
            }
        }
        if self.win.primary_device.is_none() {
            if let Some(psm) = self.win.psm.as_ref() {
                self.win.primary_device = Some(psm.get_selection_device(qh, seat));
            }
        }
    }
}

impl Dispatch2<wl_keyboard::WlKeyboard, WlState> for KbData {
    fn event(&self, st: &mut WlState, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &Connection, _: &QueueHandle<WlState>) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if std::env::var_os("GHOSTDROP_TRACE_KEYS").is_some() {
                    eprintln!("keymap size={size}");
                }
                if format == WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                    if let Some(x) = Xkb::from_wayland(&st.win.xkb_ctx, fd, size) {
                        st.win.xkb = Some(x);
                    }
                }
            }
            wl_keyboard::Event::Enter { serial, .. } => {
                if std::env::var_os("GHOSTDROP_TRACE_KEYS").is_some() {
                    eprintln!("keyboard enter");
                }
                st.win.serial = serial;
                st.win.kb_focus = true;
                if st.win.kbd == Kbd::Claiming {
                    if let Some(l) = st.win.layer.as_ref() {
                        l.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                        l.commit();
                        st.win.kbd = Kbd::OnDemand { relaxed_at: Some(Instant::now()) };
                    }
                }
                // With an exclusive layer, sway answers a click on another
                // window with leave+enter in the same instant. A flicker while
                // the pointer is outside us (and didn't just leave, which is
                // focus-follows-mouse on hover) means "clicked elsewhere".
                let flicker = st.win.kb_left_at.take().is_some_and(|t| t.elapsed() < Duration::from_millis(40));
                let outside = !st.win.pointer_inside
                    && st.win.pointer_left_at.is_none_or(|t| t.elapsed() > Duration::from_millis(150));
                st.core.focus_changed(&mut st.win, true);
                if flicker && outside && st.win.kbd == Kbd::Exclusive && st.win.visible {
                    st.core.outside_click(&mut st.win);
                }
            }
            wl_keyboard::Event::Leave { serial, .. } => {
                if std::env::var_os("GHOSTDROP_TRACE_KEYS").is_some() {
                    eprintln!("keyboard leave");
                }
                st.win.serial = serial;
                st.win.kb_focus = false;
                st.win.kb_left_at = Some(Instant::now());
                st.win.stop_repeat();
                if let Kbd::OnDemand { relaxed_at: Some(t) } = st.win.kbd {
                    if t.elapsed() < ONDEMAND_PROBATION {
                        // Focus vanished right after going on-demand: this
                        // compositor won't keep it. Go back to exclusive for
                        // good; the focus-loss debounce hides the bounce.
                        crate::core::trace!("compositor drops on-demand focus; staying exclusive");
                        st.win.ondemand_broken = true;
                        st.win.kbd = Kbd::Exclusive;
                        st.win.kb_left_at = None;
                        if let Some(l) = st.win.layer.as_ref() {
                            l.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                            l.commit();
                        }
                    } else {
                        st.win.kbd = Kbd::OnDemand { relaxed_at: None };
                    }
                }
                if let Some(x) = st.win.xkb.as_mut() {
                    x.reset_compose();
                }
                st.core.focus_changed(&mut st.win, false);
            }
            wl_keyboard::Event::Key { serial, key, state, .. } => {
                st.win.serial = serial;
                let keycode = key + 8;
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                let Some(x) = st.win.xkb.as_mut() else { return };
                let input = x.key(keycode, if pressed { KeyState::Press } else { KeyState::Release });
                let repeats = pressed && x.repeats(keycode);
                if pressed {
                    st.win.stop_repeat();
                    if repeats && st.win.repeat.0 > 0 {
                        start_repeat(st, keycode);
                    }
                } else if st.win.repeat_key == Some(keycode) {
                    st.win.stop_repeat();
                }
                st.core.key(&mut st.win, input);
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(x) = st.win.xkb.as_mut() {
                    x.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => st.win.repeat = (rate, delay),
            _ => {}
        }
    }
}

fn start_repeat(st: &mut WlState, keycode: u32) {
    let (rate, delay) = st.win.repeat;
    let interval = Duration::from_millis((1000 / rate.max(1)) as u64);
    st.win.repeat_key = Some(keycode);
    let timer = calloop::timer::Timer::from_duration(Duration::from_millis(delay.max(0) as u64));
    st.win.repeat_token = st
        .win
        .handle
        .insert_source(timer, move |_, _, st: &mut WlState| {
            if st.win.repeat_key != Some(keycode) || !st.win.kb_focus {
                st.win.repeat_token = None;
                return calloop::timer::TimeoutAction::Drop;
            }
            if let Some(x) = st.win.xkb.as_mut() {
                let input = x.key(keycode, KeyState::Repeat);
                st.core.key(&mut st.win, input);
            }
            if st.win.repeat_key != Some(keycode) || !st.win.kb_focus {
                st.win.repeat_token = None;
                return calloop::timer::TimeoutAction::Drop;
            }
            calloop::timer::TimeoutAction::ToDuration(interval)
        })
        .ok();
}

impl PointerHandler for WlState {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let mods = self.win.mods();
        for e in events {
            let (x, y) = e.position;
            match e.kind {
                PointerEventKind::Enter { serial } => {
                    self.win.serial = serial;
                    self.win.pointer_inside = true;
                    self.core.pointer(&mut self.win, PointerInput::Motion { x, y }, mods);
                }
                PointerEventKind::Leave { .. } => {
                    self.win.pointer_inside = false;
                    self.win.pointer_left_at = Some(Instant::now());
                    self.core.pointer(&mut self.win, PointerInput::Leave, mods)
                }
                PointerEventKind::Motion { .. } => self.core.pointer(&mut self.win, PointerInput::Motion { x, y }, mods),
                PointerEventKind::Press { button, serial, .. } | PointerEventKind::Release { button, serial, .. } => {
                    self.win.serial = serial;
                    let pressed = matches!(e.kind, PointerEventKind::Press { .. });
                    let b = match button {
                        0x110 => Button::Left,
                        0x111 => Button::Right,
                        0x112 => Button::Middle,
                        0x113 => Button::Other(8),
                        0x114 => Button::Other(9),
                        other => Button::Other(other),
                    };
                    self.core.pointer(&mut self.win, PointerInput::Button { button: b, pressed }, mods);
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    let lines = |a: &smithay_client_toolkit::seat::pointer::AxisScroll| -> Option<f64> {
                        if a.value120 != 0 {
                            Some(a.value120 as f64 / 120.0)
                        } else if a.discrete != 0 {
                            Some(a.discrete as f64)
                        } else {
                            None
                        }
                    };
                    let (dx, dy, precise) = match (lines(&horizontal), lines(&vertical)) {
                        (None, None) => (horizontal.absolute, vertical.absolute, true),
                        (h, v) => (h.unwrap_or(0.0), v.unwrap_or(0.0), false),
                    };
                    if dx != 0.0 || dy != 0.0 {
                        self.core.pointer(&mut self.win, PointerInput::Scroll { dx, dy, precise }, mods);
                    }
                }
            }
        }
    }
}

impl ShmHandler for WlState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.win.shm
    }
}

impl DataDeviceHandler for WlState {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &wl_surface::WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for WlState {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

fn write_all_async(win: &mut WlWin, pipe: WritePipe, text: String) {
    let fd: OwnedFd = pipe.into();
    let file = std::fs::File::from(fd);
    let _ = rustix::fs::fcntl_setfl(file.as_fd(), rustix::fs::OFlags::NONBLOCK);
    let bytes = text.into_bytes();
    let mut off = 0usize;
    let wid = win.next_write_id;
    win.next_write_id = win.next_write_id.wrapping_add(1);
    if let Ok(reg) = win.handle.insert_source(Generic::new(file, Interest::WRITE, Mode::Level), move |_, f, st| {
        let f: &mut std::fs::File = unsafe { f.get_mut() };
        loop {
            if off >= bytes.len() {
                st.win.active_writes.remove(&wid);
                return Ok(PostAction::Remove);
            }
            match f.write(&bytes[off..]) {
                Ok(0) => {
                    st.win.active_writes.remove(&wid);
                    return Ok(PostAction::Remove);
                }
                Ok(n) => {
                    off += n;
                    if let Some((_, d)) = st.win.active_writes.get_mut(&wid) {
                        *d = Instant::now() + CLIP_WRITE_STEP_TIMEOUT;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    st.win.active_writes.remove(&wid);
                    return Ok(PostAction::Remove);
                }
            }
        }
    }) {
        win.active_writes.insert(wid, (reg, Instant::now() + CLIP_WRITE_STEP_TIMEOUT));
    }
}

impl DataSourceHandler for WlState {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _mime: String, fd: WritePipe) {
        if self.win.copy_source.as_ref().is_some_and(|s| s.inner() == source) {
            let text = self.win.clip_text.clone();
            write_all_async(&mut self.win, fd, text);
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.win.copy_source.as_ref().is_some_and(|s| s.inner() == source) {
            self.win.copy_source = None;
        }
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl PrimarySelectionDeviceHandler for WlState {
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ZwpPrimarySelectionDeviceV1) {}
}

impl PrimarySelectionSourceHandler for WlState {
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &ZwpPrimarySelectionSourceV1, _: String, pipe: WritePipe) {
        if self.win.primary_source.as_ref().is_some_and(|s| s.inner() == source) {
            let text = self.win.primary_text.clone();
            write_all_async(&mut self.win, pipe, text);
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &ZwpPrimarySelectionSourceV1) {
        if self.win.primary_source.as_ref().is_some_and(|s| s.inner() == source) {
            self.win.primary_source = None;
        }
    }
}

/// User data for protocol objects that send no events.
struct NoEvents;
impl Dispatch2<WpFractionalScaleManagerV1, WlState> for NoEvents {
    fn event(&self, _: &mut WlState, _: &WpFractionalScaleManagerV1, _: <WpFractionalScaleManagerV1 as wayland_client::Proxy>::Event, _: &Connection, _: &QueueHandle<WlState>) {}
}
impl Dispatch2<WpViewporter, WlState> for NoEvents {
    fn event(&self, _: &mut WlState, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &Connection, _: &QueueHandle<WlState>) {}
}
impl Dispatch2<WpViewport, WlState> for NoEvents {
    fn event(&self, _: &mut WlState, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &Connection, _: &QueueHandle<WlState>) {}
}

struct FracData;
impl Dispatch2<WpFractionalScaleV1, WlState> for FracData {
    fn event(&self, st: &mut WlState, proxy: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &Connection, _: &QueueHandle<WlState>) {
        if st.win.frac.as_ref().is_none_or(|f| f != proxy) {
            return;
        }
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if scale != st.win.scale120 && scale > 0 {
                crate::core::trace!("fractional scale {}", scale as f64 / 120.0);
                st.win.scale120 = scale;
                st.core.resized(&mut st.win);
            }
        }
    }
}

impl DmabufHandler for WlState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.win.dmabuf
    }

    fn dmabuf_feedback(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ZwpLinuxDmabufFeedbackV1, feedback: DmabufFeedback) {
        if self.win.gpu_probed {
            return; // later updates can't move libghostty to another GPU
        }
        self.win.gpu_probed = true;
        let mut mods: Vec<u64> = feedback
            .format_table()
            .iter()
            .filter(|f| f.format == DRM_FORMAT_ARGB8888 && f.modifier != DRM_FORMAT_MOD_INVALID)
            .map(|f| f.modifier)
            .collect();
        mods.sort_unstable();
        mods.dedup();
        let node = crate::drm::render_node_for_dev(feedback.main_device());
        crate::core::trace!("gpu: compositor device {node:?}, {} ARGB8888 modifiers", mods.len());
        if let Some(n) = &node {
            ghostty::set_render_device(n);
        }
        ghostty::set_present_modifiers(&mods);
        self.win.gpu = Some(WlGpu { sub: None, buffers: Vec::new(), clock: 0, frame_pending: None });
        self.core.set_gpu_backend(true);
    }

    fn created(&mut self, _: &Connection, _: &QueueHandle<Self>, params: &ZwpLinuxBufferParamsV1, buffer: WlBuffer) {
        let Some(g) = self.win.gpu.as_mut() else {
            buffer.destroy();
            return;
        };
        match g.buffers.iter_mut().find(|b| matches!(&b.1, GpuBuf::Pending { params: p, .. } if p == params)) {
            Some(b) => {
                b.1 = GpuBuf::Ready(buffer);
                self.win.needs_present = true;
            }
            None => buffer.destroy(),
        }
        params.destroy();
    }

    fn failed(&mut self, _: &Connection, _: &QueueHandle<Self>, params: &ZwpLinuxBufferParamsV1) {
        params.destroy();
        self.core.gpu_failed(&mut self.win, "the compositor could not import a frame");
    }

    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlBuffer) {}
}

impl ProvidesRegistryState for WlState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.win.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(WlState);
smithay_client_toolkit::delegate_dispatch2!(WlState);
