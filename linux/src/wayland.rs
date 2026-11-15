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
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;
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
use xkbcommon::xkb;

const TEXT_MIMES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];

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
    frame_pending: bool,
    needs_redraw: bool,
    serial: u32,
    clip_results: Vec<(u64, Option<String>)>,
    /// Mapped with exclusive keyboard interactivity to get focus right away;
    /// relaxed to on-demand once focus arrives (so clicking away works).
    relax_keyboard: bool,
}

pub struct WlState {
    pub win: WlWin,
    pub core: Core,
}

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
    fn physical(&self) -> (u32, u32) {
        (self.logical.0 * self.scale as u32, self.logical.1 * self.scale as u32)
    }

    /// Logical size of the output we'll most likely appear on.
    fn output_size(&self) -> Option<(f64, f64)> {
        self.output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .filter_map(|i| i.logical_size.map(|(w, h)| (w as f64, h as f64)))
            .next()
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
        // Exclusive until we have focus (see `relax_keyboard`).
        layer.set_keyboard_interactivity(if p.exclusive_keyboard || !self.kb_focus {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::OnDemand
        });
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
            self.configure_layer(layer, p);
            layer.commit();
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(&self.qh, surface, layer_kind(p), Some("ghostdrop"), None);
        self.configure_layer(&layer, p);
        layer.commit();
        self.layer = Some(layer);
        self.relax_keyboard = !p.exclusive_keyboard;
        self.visible = true;
        self.configured = false;
        self.frame_pending = false;
    }

    fn hide(&mut self) {
        self.stop_repeat();
        self.layer = None;
        self.visible = false;
        self.configured = false;
        self.kb_focus = false;
        self.frame_pending = false;
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn is_focused(&self) -> bool {
        self.kb_focus
    }

    fn scale(&self) -> f64 {
        self.scale as f64
    }

    fn size(&self) -> (u32, u32) {
        self.physical()
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
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
        let _ = self.handle.insert_source(Generic::new(file, Interest::READ, Mode::Level), move |_, f, st| {
            let f: &mut std::fs::File = unsafe { f.get_mut() };
            let mut buf = [0u8; 65536];
            loop {
                match f.read(&mut buf) {
                    Ok(0) => {
                        let text = String::from_utf8_lossy(&data).into_owned();
                        st.core.clipboard_result(&mut st.win, token, Some(text));
                        return Ok(PostAction::Remove);
                    }
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        st.core.clipboard_result(&mut st.win, token, None);
                        return Ok(PostAction::Remove);
                    }
                }
            }
        });
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
        let results = std::mem::take(&mut self.win.clip_results);
        for (token, text) in results {
            self.core.clipboard_result(&mut self.win, token, text);
        }
        if ghostty::has_pending() {
            self.core.process(&mut self.win);
        }
        let w = &mut self.win;
        if !(w.needs_redraw && w.visible && w.configured && !w.frame_pending) {
            return;
        }
        let Some(layer) = w.layer.as_ref() else { return };
        let (pw, ph) = w.physical();
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
        self.core.draw((pw, ph), w.scale as f64, &mut c);
        let surface = layer.wl_surface();
        surface.set_buffer_scale(w.scale);
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        surface.frame(&w.qh, smithay_client_toolkit::compositor::FrameCallbackData(surface.clone()));
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
            w.frame_pending = true;
            w.needs_redraw = false;
        }
    }
}

pub fn run(mut core_builder: impl FnMut() -> Core, show_now: bool) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
    let (globals, queue) = registry_queue_init::<WlState>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
    let qh = queue.handle();
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| "compositor has no wlr-layer-shell".to_string())?;
    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;

    let mut event_loop: calloop::EventLoop<'static, WlState> = calloop::EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    calloop_wayland_source::WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| format!("wayland source: {e}"))?;

    let (ping, ping_src) = calloop::ping::make_ping().map_err(|e| e.to_string())?;
    ghostty::set_waker(ping.clone());
    crate::install_signal_handlers(ping);
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
            while let Ok((stream, _)) = l.accept() {
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
        frame_pending: false,
        needs_redraw: false,
        serial: 0,
        clip_results: Vec::new(),
        relax_keyboard: false,
    };
    let mut st = WlState { win, core: core_builder() };
    // Let outputs/seats arrive before sizing anything.
    event_loop.dispatch(Some(Duration::from_millis(50)), &mut st).map_err(|e| e.to_string())?;
    st.core.prespawn(&mut st.win);
    if show_now {
        st.core.show(&mut st.win);
    }
    while !st.core.quit {
        event_loop.dispatch(Some(Duration::from_millis(500)), &mut st).map_err(|e| e.to_string())?;
        st.after_dispatch();
        let _ = st.win.conn.flush();
    }
    st.core.tabs.clear();
    ipc::cleanup();
    Ok(())
}

// ---------------------------------------------------------------- handlers

impl CompositorHandler for WlState {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        if factor != self.win.scale && factor > 0 {
            self.win.scale = factor;
            self.core.resized(&mut self.win);
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.win.frame_pending = false;
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
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.core.hide(&mut self.win);
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, cfg: LayerSurfaceConfigure, _: u32) {
        let (w, h) = cfg.new_size;
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
        if self.win.data_device.is_none() {
            if let Some(ddm) = self.win.ddm.as_ref() {
                self.win.data_device = Some(ddm.get_data_device(qh, &seat));
            }
        }
        if self.win.primary_device.is_none() {
            if let Some(psm) = self.win.psm.as_ref() {
                self.win.primary_device = Some(psm.get_selection_device(qh, &seat));
            }
        }
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
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
            if let Some(k) = self.win.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            self.win.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
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
                if st.win.relax_keyboard {
                    st.win.relax_keyboard = false;
                    if let Some(l) = st.win.layer.as_ref() {
                        l.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                        l.commit();
                    }
                }
                st.core.focus_changed(&mut st.win, true);
            }
            wl_keyboard::Event::Leave { serial, .. } => {
                if std::env::var_os("GHOSTDROP_TRACE_KEYS").is_some() {
                    eprintln!("keyboard leave");
                }
                st.win.serial = serial;
                st.win.kb_focus = false;
                st.win.stop_repeat();
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
                    self.core.pointer(&mut self.win, PointerInput::Motion { x, y }, mods);
                }
                PointerEventKind::Leave { .. } => self.core.pointer(&mut self.win, PointerInput::Leave, mods),
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

fn write_all_async(handle: &LoopHandle<'static, WlState>, pipe: WritePipe, text: String) {
    let fd: OwnedFd = pipe.into();
    let file = std::fs::File::from(fd);
    let _ = rustix::fs::fcntl_setfl(file.as_fd(), rustix::fs::OFlags::NONBLOCK);
    let bytes = text.into_bytes();
    let mut off = 0usize;
    let _ = handle.insert_source(Generic::new(file, Interest::WRITE, Mode::Level), move |_, f, _| {
        let f: &mut std::fs::File = unsafe { f.get_mut() };
        loop {
            if off >= bytes.len() {
                return Ok(PostAction::Remove);
            }
            match f.write(&bytes[off..]) {
                Ok(0) => return Ok(PostAction::Remove),
                Ok(n) => off += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(PostAction::Continue),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Ok(PostAction::Remove),
            }
        }
    });
}

impl DataSourceHandler for WlState {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _mime: String, fd: WritePipe) {
        if self.win.copy_source.as_ref().is_some_and(|s| s.inner() == source) {
            write_all_async(&self.win.handle, fd, self.win.clip_text.clone());
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
            write_all_async(&self.win.handle, pipe, self.win.primary_text.clone());
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &ZwpPrimarySelectionSourceV1) {
        if self.win.primary_source.as_ref().is_some_and(|s| s.inner() == source) {
            self.win.primary_source = None;
        }
    }
}

impl ProvidesRegistryState for WlState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.win.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(WlState);
smithay_client_toolkit::delegate_dispatch2!(WlState);
