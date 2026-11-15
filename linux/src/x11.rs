//! X11 backend (i3 and any other X11 window manager): an override-redirect
//! window the WM doesn't manage, so it drops down without any WM rules.
use crate::core::{Button, Core, Cursor, Placement, PointerInput, Pos, Window};
use crate::draw::Canvas;
use crate::ghostty::{self, Clip};
use crate::ipc;
use crate::xkb::{KeyState, Xkb};
use calloop::{generic::Generic, Interest, Mode, PostAction};
use std::collections::{HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::time::Duration;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xkb::{self as xxkb, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::{composite, dri3, present, randr, shm, Event};
use x11rb::protocol::composite::ConnectionExt as _;
use x11rb::protocol::dri3::ConnectionExt as _;
use x11rb::protocol::present::ConnectionExt as _;
use x11rb::utils::RawFdContainer;
use crate::tab::Frame;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;
use xkbcommon::xkb;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        CLIPBOARD,
        PRIMARY,
        UTF8_STRING,
        TARGETS,
        TEXT,
        INCR,
        GHOSTDROP_CLIPBOARD,
        GHOSTDROP_PRIMARY,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        TEXT_PLAIN: b"text/plain",
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        _NET_ACTIVE_WINDOW,
    }
}

struct ShmBuf {
    seg: u32,
    map: memmap2::MmapMut,
    size: usize,
}

/// Zero-copy presentation: DMA-BUF frames become pixmaps (DRI3) that the X
/// server copies into the window on vblank (Present). No CPU copies.
struct Gpu {
    serial: u32,
    /// A Present is outstanding (one at a time; released by CompleteNotify).
    inflight: Option<std::time::Instant>,
    /// Imported buffers, keyed by buffer identity; the renderer cycles a few.
    pixmaps: Vec<(GpuKey, u32, u64)>,
    clock: u64,
    dri3_opcode: u8,
    present_opcode: u8,
    /// Trace one line per new imported buffer (format/modifier).
    logged: HashSet<u64>,
}

type GpuKey = (u64, u32, u32, u32, u64);

impl Gpu {
    /// DRI3 >= 1.2 (modifiers) and Present on this server, plus the DRM
    /// render node of its GPU (so libghostty renders on the same one).
    fn probe(conn: &XCBConnection, root: u32, win: u32, depth: u8) -> Option<(Gpu, Option<String>, Vec<u64>)> {
        if std::env::var("GHOSTDROP_GPU").as_deref() == Ok("0") {
            return None;
        }
        let d = conn.dri3_query_version(1, 2).ok()?.reply().ok()?;
        if (d.major_version, d.minor_version) < (1, 2) {
            crate::core::trace!("gpu: DRI3 {}.{} lacks modifier support", d.major_version, d.minor_version);
            return None;
        }
        conn.present_query_version(1, 2).ok()?.reply().ok()?;
        let dri3_opcode = conn.extension_information(dri3::X11_EXTENSION_NAME).ok()??.major_opcode;
        let present_opcode = conn.extension_information(present::X11_EXTENSION_NAME).ok()??.major_opcode;
        let node = conn
            .dri3_open(root, 0)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| crate::drm::render_node_for_fd(std::os::fd::AsRawFd::as_raw_fd(&r.device_fd)));
        // Layouts the server imports for our window's depth; libghostty
        // allocates its export buffers in one of these (GBM).
        let mods = conn.dri3_get_supported_modifiers(win, depth, 32).ok().and_then(|c| c.reply().ok()).map(|m| {
            crate::core::trace!("gpu: depth {depth} modifiers window={:x?} screen={:x?}", m.window_modifiers, m.screen_modifiers);
            if m.window_modifiers.is_empty() { m.screen_modifiers } else { m.window_modifiers }
        });
        let eid = conn.generate_id().ok()?;
        conn.present_select_input(eid, win, present::EventMask::COMPLETE_NOTIFY).ok()?;
        let gpu = Gpu {
            serial: 0,
            inflight: None,
            pixmaps: Vec::new(),
            clock: 0,
            dri3_opcode,
            present_opcode,
            logged: HashSet::new(),
        };
        Some((gpu, node, mods.unwrap_or_default()))
    }

    fn free_all(&mut self, conn: &XCBConnection) {
        for (_, p, _) in self.pixmaps.drain(..) {
            let _ = conn.free_pixmap(p);
        }
        self.inflight = None;
    }
}

/// Rectangles of a `w`x`h` window outside `(x, y, cw, ch)`.
fn outside(w: u32, h: u32, (x, y, cw, ch): (u32, u32, u32, u32)) -> Vec<(u32, u32, u32, u32)> {
    let (cw, ch) = (cw.min(w.saturating_sub(x)), ch.min(h.saturating_sub(y)));
    [(0, 0, w, y), (0, y + ch, w, h.saturating_sub(y + ch)), (0, y, x, ch), (x + cw, y, w.saturating_sub(x + cw), ch)]
        .into_iter()
        .filter(|r| r.2 > 0 && r.3 > 0)
        .collect()
}

struct IncrRead {
    property: u32,
    data: Vec<u8>,
    tokens: Vec<u64>,
}

pub struct X11Win {
    conn: XCBConnection,
    root: u32,
    win: u32,
    gc: u32,
    depth: u8,
    atoms: Atoms,
    shm: Option<ShmBuf>,
    shm_supported: bool,
    shm_busy: bool,
    visible: bool,
    focused: bool,
    geom: (i32, i32, u32, u32),
    scale: f64,
    needs_redraw: bool,
    xkb_ctx: xkb::Context,
    xkb: Option<Xkb>,
    xkb_device: i32,
    pressed: HashSet<u32>,
    hotkey: Option<(u8, u16)>,
    hotkey_status: String,
    prev_focus: u32,
    clip_text: String,
    primary_text: String,
    owns: HashSet<u32>,
    reads: HashMap<u32, Vec<u64>>,
    tried_string: HashSet<u32>,
    incr: Option<IncrRead>,
    clip_results: Vec<(u64, Option<String>)>,
    cursor_handle: Option<x11rb::cursor::Handle>,
    cursors: HashMap<Cursor, u32>,
    cursor: Option<Cursor>,
    mapped: bool,
    gpu: Option<Gpu>,
    /// A new GPU frame waits to be presented.
    needs_present: bool,
    /// CPU canvas when MIT-SHM is unavailable.
    cpu_buf: Vec<u8>,
    /// Test mode: window redirected offscreen (Composite), never on screen.
    offscreen: bool,
}

pub struct X11State {
    pub win: X11Win,
    pub core: Core,
}

fn x_mods(state: KeyButMask) -> crate::keys::Mods {
    crate::keys::Mods {
        ctrl: state.contains(KeyButMask::CONTROL),
        shift: state.contains(KeyButMask::SHIFT),
        alt: state.contains(KeyButMask::MOD1),
        sup: state.contains(KeyButMask::MOD4),
    }
}

impl X11Win {
    fn screen(&self) -> &Screen {
        &self.conn.setup().roots[0]
    }

    /// Monitor (physical px) to use: the one under the pointer, else primary.
    fn monitor(&self, under_mouse: bool) -> (i32, i32, u32, u32) {
        let fallback = (0, 0, self.screen().width_in_pixels as u32, self.screen().height_in_pixels as u32);
        let Ok(Ok(mons)) = randr::get_monitors(&self.conn, self.root, true).map(|c| c.reply()) else { return fallback };
        if mons.monitors.is_empty() {
            return fallback;
        }
        let pick = if under_mouse {
            let p = self.conn.query_pointer(self.root).ok().and_then(|c| c.reply().ok());
            p.and_then(|p| {
                mons.monitors.iter().find(|m| {
                    let (x, y) = (p.root_x as i32, p.root_y as i32);
                    x >= m.x as i32 && x < m.x as i32 + m.width as i32 && y >= m.y as i32 && y < m.y as i32 + m.height as i32
                })
            })
        } else {
            None
        };
        let m = pick.or_else(|| mons.monitors.iter().find(|m| m.primary)).unwrap_or(&mons.monitors[0]);
        (m.x as i32, m.y as i32, m.width as u32, m.height as u32)
    }

    fn ensure_shm(&mut self, size: usize) {
        if !self.shm_supported || self.shm.as_ref().is_some_and(|s| s.size >= size) {
            return;
        }
        if let Some(old) = self.shm.take() {
            let _ = shm::detach(&self.conn, old.seg);
        }
        let make = || -> Option<ShmBuf> {
            let fd = rustix::fs::memfd_create("ghostdrop-shm", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
            rustix::fs::ftruncate(&fd, size as u64).ok()?;
            let file = std::fs::File::from(fd);
            let map = unsafe { memmap2::MmapMut::map_mut(&file).ok()? };
            let seg = self.conn.generate_id().ok()?;
            let dup = rustix::io::dup(&file).ok()?;
            shm::attach_fd(&self.conn, seg, dup, false).ok()?.check().ok()?;
            Some(ShmBuf { seg, map, size })
        };
        self.shm = make();
        if self.shm.is_none() {
            eprintln!("ghostdrop: MIT-SHM unavailable, using PutImage");
            self.shm_supported = false;
        }
    }

    fn load_keymap(&mut self) {
        let km = xkb::x11::keymap_new_from_device(&self.xkb_ctx, &self.conn, self.xkb_device, xkb::KEYMAP_COMPILE_NO_FLAGS);
        let st = xkb::x11::state_new_from_device(&km, &self.conn, self.xkb_device);
        self.xkb = Some(Xkb::new(&self.xkb_ctx, km, st));
    }

    fn grab_hotkey(&mut self, combo: Option<&crate::keys::KeyCombo>) {
        if let Some((kc, mods)) = self.hotkey.take() {
            for extra in [0u16, u16::from(ModMask::LOCK), u16::from(ModMask::M2), u16::from(ModMask::LOCK) | u16::from(ModMask::M2)] {
                let _ = self.conn.ungrab_key(kc, self.root, ModMask::from(mods | extra));
            }
        }
        let Some(combo) = combo else {
            self.hotkey_status = "none".into();
            return;
        };
        let Some(kc) = self.xkb.as_ref().and_then(|x| x.keycode_for(combo.keysym())) else {
            self.hotkey_status = format!("{}: key not on this keyboard", combo.describe());
            return;
        };
        let mut mods = 0u16;
        if combo.mods.ctrl {
            mods |= u16::from(ModMask::CONTROL);
        }
        if combo.mods.shift {
            mods |= u16::from(ModMask::SHIFT);
        }
        if combo.mods.alt {
            mods |= u16::from(ModMask::M1);
        }
        if combo.mods.sup {
            mods |= u16::from(ModMask::M4);
        }
        let mut ok = true;
        for extra in [0u16, u16::from(ModMask::LOCK), u16::from(ModMask::M2), u16::from(ModMask::LOCK) | u16::from(ModMask::M2)] {
            let r = self
                .conn
                .grab_key(false, self.root, ModMask::from(mods | extra), kc as u8, GrabMode::ASYNC, GrabMode::ASYNC)
                .map(|c| c.check());
            if !matches!(r, Ok(Ok(()))) {
                ok = false;
            }
        }
        if ok {
            self.hotkey = Some((kc as u8, mods));
            self.hotkey_status = format!("{} (grabbed)", combo.describe());
        } else {
            self.hotkey = Some((kc as u8, mods));
            self.hotkey_status = format!(
                "{} is taken by another client (your WM?). Fine if your WM binding runs `ghostdrop`.",
                combo.describe()
            );
            eprintln!("ghostdrop: hotkey {}", self.hotkey_status);
        }
    }

    fn cursor_id(&mut self, c: Cursor) -> u32 {
        if let Some(id) = self.cursors.get(&c) {
            return *id;
        }
        let names: &[&str] = match c {
            Cursor::Default => &["left_ptr", "default"],
            Cursor::Text => &["xterm", "text"],
            Cursor::Pointer => &["hand2", "pointer"],
            Cursor::Crosshair => &["crosshair"],
            Cursor::ResizeH => &["sb_h_double_arrow", "ew-resize"],
            Cursor::ResizeV => &["sb_v_double_arrow", "ns-resize"],
            Cursor::Grab => &["grab", "hand1"],
            Cursor::Grabbing => &["grabbing", "fleur"],
            Cursor::NotAllowed => &["not-allowed", "crossed_circle"],
            Cursor::Wait => &["watch", "wait"],
            Cursor::Hidden => &[],
        };
        let mut id = 0;
        if c == Cursor::Hidden {
            // 1x1 transparent pixmap cursor.
            if let (Ok(pix), Ok(cur)) = (self.conn.generate_id(), self.conn.generate_id()) {
                let _ = self.conn.create_pixmap(1, pix, self.win, 1, 1);
                let _ = self.conn.create_cursor(cur, pix, pix, 0, 0, 0, 0, 0, 0, 0, 0);
                let _ = self.conn.free_pixmap(pix);
                id = cur;
            }
        }
        if let Some(h) = self.cursor_handle.as_ref().filter(|_| c != Cursor::Hidden) {
            for n in names {
                if let Ok(x) = h.load_cursor(&self.conn, n) {
                    if x != 0 {
                        id = x;
                        break;
                    }
                }
            }
        }
        self.cursors.insert(c, id);
        id
    }

    fn send_selection(&mut self, ev: &SelectionRequestEvent) {
        let a = self.atoms;
        let text = if ev.selection == a.CLIPBOARD { &self.clip_text } else { &self.primary_text };
        let property = if ev.property == x11rb::NONE { ev.target } else { ev.property };
        let ok = if !self.owns.contains(&ev.selection) {
            false
        } else if ev.target == a.TARGETS {
            let targets = [a.TARGETS, a.UTF8_STRING, a.TEXT_PLAIN_UTF8, a.TEXT_PLAIN, AtomEnum::STRING.into(), a.TEXT];
            self.conn
                .change_property32(PropMode::REPLACE, ev.requestor, property, AtomEnum::ATOM, &targets)
                .is_ok()
        } else if [a.UTF8_STRING, a.TEXT_PLAIN_UTF8, a.TEXT_PLAIN, a.TEXT, AtomEnum::STRING.into()].contains(&ev.target) {
            let ty = if ev.target == u32::from(AtomEnum::STRING) || ev.target == a.TEXT { a.UTF8_STRING } else { ev.target };
            self.conn
                .change_property8(PropMode::REPLACE, ev.requestor, property, ty, text.as_bytes())
                .is_ok()
        } else {
            false
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: ev.time,
            requestor: ev.requestor,
            selection: ev.selection,
            target: ev.target,
            property: if ok { property } else { x11rb::NONE },
        };
        let _ = self.conn.send_event(false, ev.requestor, EventMask::NO_EVENT, notify);
        let _ = self.conn.flush();
    }

    fn finish_read(&mut self, selection: u32, text: Option<String>) {
        for t in self.reads.remove(&selection).unwrap_or_default() {
            self.clip_results.push((t, text.clone()));
        }
        self.tried_string.remove(&selection);
    }

    fn on_selection_notify(&mut self, ev: &SelectionNotifyEvent) {
        let sel = ev.selection;
        if ev.property == x11rb::NONE {
            // Some clients only offer STRING.
            if ev.target == self.atoms.UTF8_STRING && self.tried_string.insert(sel) {
                let prop = if sel == self.atoms.CLIPBOARD { self.atoms.GHOSTDROP_CLIPBOARD } else { self.atoms.GHOSTDROP_PRIMARY };
                let _ = self.conn.convert_selection(self.win, sel, u32::from(AtomEnum::STRING), prop, x11rb::CURRENT_TIME);
                let _ = self.conn.flush();
                return;
            }
            self.finish_read(sel, None);
            return;
        }
        let Ok(Ok(reply)) = self
            .conn
            .get_property(true, self.win, ev.property, AtomEnum::ANY, 0, u32::MAX / 4)
            .map(|c| c.reply())
        else {
            self.finish_read(sel, None);
            return;
        };
        if reply.type_ == self.atoms.INCR {
            // Large transfer: chunks arrive as PropertyNotify(NewValue).
            let tokens = self.reads.remove(&sel).unwrap_or_default();
            self.incr = Some(IncrRead { property: ev.property, data: Vec::new(), tokens });
            let _ = self.conn.flush();
            return;
        }
        let text = String::from_utf8_lossy(&reply.value).into_owned();
        self.finish_read(sel, Some(text));
    }

    fn on_property_notify(&mut self, ev: &PropertyNotifyEvent) {
        let Some(incr) = self.incr.as_mut() else { return };
        if ev.window != self.win || ev.atom != incr.property || ev.state != Property::NEW_VALUE {
            return;
        }
        let Ok(Ok(reply)) = self
            .conn
            .get_property(true, self.win, incr.property, AtomEnum::ANY, 0, u32::MAX / 4)
            .map(|c| c.reply())
        else {
            return;
        };
        if reply.value.is_empty() {
            let incr = self.incr.take().unwrap();
            let text = String::from_utf8_lossy(&incr.data).into_owned();
            for t in incr.tokens {
                self.clip_results.push((t, Some(text.clone())));
            }
        } else {
            incr.data.extend_from_slice(&reply.value);
        }
        let _ = self.conn.flush();
    }

    /// Compose the whole window into the CPU buffer (SHM or plain).
    fn compose(&mut self, draw: impl FnOnce(&mut Canvas)) -> bool {
        let (w, h) = (self.geom.2, self.geom.3);
        if w == 0 || h == 0 {
            return false;
        }
        let stride = w as usize * 4;
        let size = stride * h as usize;
        self.ensure_shm(size);
        let buf: &mut [u8] = match self.shm.as_mut() {
            Some(s) => &mut s.map[..size],
            None => {
                self.cpu_buf.resize(size, 0);
                &mut self.cpu_buf[..size]
            }
        };
        let mut c = Canvas { buf, width: w as usize, height: h as usize, stride };
        draw(&mut c);
        true
    }

    /// Upload rectangles of the composed buffer (all of it when `None`).
    fn put(&mut self, rects: Option<Vec<(u32, u32, u32, u32)>>) {
        let (w, h) = (self.geom.2, self.geom.3);
        let rects = rects.unwrap_or_else(|| vec![(0, 0, w, h)]);
        let stride = w as usize * 4;
        if let Some(s) = self.shm.as_ref() {
            let seg = s.seg;
            let n = rects.len();
            for (i, (x, y, rw, rh)) in rects.into_iter().enumerate() {
                // Only the last upload asks for a completion event.
                let r = shm::put_image(
                    &self.conn, self.win, self.gc, w as u16, h as u16, x as u16, y as u16, rw as u16, rh as u16, x as i16, y as i16,
                    self.depth, ImageFormat::Z_PIXMAP.into(), i + 1 == n, seg, 0,
                );
                if r.is_ok() && i + 1 == n {
                    self.shm_busy = true;
                }
            }
        } else {
            // Stay under the maximum request size.
            let max = (self.conn.maximum_request_bytes().saturating_sub(64)).max(stride);
            for (x, y, rw, rh) in rects {
                let row_bytes = rw as usize * 4;
                let rows = (max / row_bytes.max(1)).max(1);
                let mut yy = 0usize;
                while yy < rh as usize {
                    let n = rows.min(rh as usize - yy);
                    let mut chunk = Vec::with_capacity(n * row_bytes);
                    for r in 0..n {
                        let o = (y as usize + yy + r) * stride + x as usize * 4;
                        chunk.extend_from_slice(&self.cpu_buf[o..o + row_bytes]);
                    }
                    let _ = self.conn.put_image(
                        ImageFormat::Z_PIXMAP, self.win, self.gc, rw as u16, n as u16, x as i16, (y as usize + yy) as i16, 0, self.depth,
                        &chunk,
                    );
                    yy += n;
                }
            }
        }
        let _ = self.conn.flush();
        self.needs_redraw = false;
    }

    /// Present a DMA-BUF frame at `rect` (physical px). Ok(false) while a
    /// previous present is still pending.
    fn present_gpu(&mut self, frame: &Frame, rect: (u32, u32, u32, u32)) -> Result<bool, String> {
        let Some(gpu) = self.gpu.as_mut() else { return Ok(false) };
        let Some(d) = frame.dmabuf() else { return Ok(true) };
        if let Some(t) = gpu.inflight {
            // A lost CompleteNotify must not stall us forever.
            if t.elapsed() < Duration::from_millis(250) {
                return Ok(false);
            }
        }
        // Wait for a frame that fits (resizes take a frame to catch up).
        if frame.width > rect.2 || frame.height > rect.3 {
            return Ok(true);
        }
        gpu.clock += 1;
        let key: GpuKey = (d.ino, frame.width, frame.height, d.strides[0], d.modifier);
        let pixmap = match gpu.pixmaps.iter_mut().find(|p| p.0 == key) {
            Some(p) => {
                p.2 = gpu.clock;
                p.1
            }
            None => {
                let pixmap = self.conn.generate_id().map_err(|e| e.to_string())?;
                let mut fds = Vec::new();
                for &plane_fd in &d.fds[..d.n_planes] {
                    let fd = rustix::io::dup(unsafe { std::os::fd::BorrowedFd::borrow_raw(plane_fd) }).map_err(|e| e.to_string())?;
                    fds.push(RawFdContainer::from(fd));
                }
                let (s, o) = (d.strides, d.offsets);
                self.conn
                    .dri3_pixmap_from_buffers(
                        pixmap, self.win, frame.width as u16, frame.height as u16, s[0], o[0], s[1], o[1], s[2], o[2], s[3], o[3],
                        self.depth, 32, d.modifier, fds,
                    )
                    .map_err(|e| e.to_string())?
                    .check()
                    .map_err(|e| {
                        format!(
                            "DRI3 PixmapFromBuffers {}x{} depth {} fourcc={:#x} modifier={:#x} planes={} stride={} offset={}: {:?}",
                            frame.width, frame.height, self.depth, d.fourcc, d.modifier, d.n_planes, s[0], o[0],
                            e
                        )
                    })?;
                if gpu.logged.insert(d.ino) {
                    crate::core::trace!(
                        "gpu: imported {}x{} fourcc={:#x} modifier={:#x} planes={} stride={}",
                        frame.width, frame.height, d.fourcc, d.modifier, d.n_planes, s[0]
                    );
                }
                gpu.pixmaps.push((key, pixmap, gpu.clock));
                if gpu.pixmaps.len() > 6 {
                    let (i, _) = gpu.pixmaps.iter().enumerate().min_by_key(|(_, p)| p.2).unwrap();
                    let (_, old, _) = gpu.pixmaps.remove(i);
                    let _ = self.conn.free_pixmap(old);
                }
                pixmap
            }
        };
        gpu.serial = gpu.serial.wrapping_add(1);
        self.conn
            .present_pixmap(
                self.win, pixmap, gpu.serial, 0, 0, rect.0 as i16, rect.1 as i16, 0, 0, 0,
                present::Option::NONE.into(), 0, 0, 0, &[],
            )
            .map_err(|e| e.to_string())?;
        gpu.inflight = Some(std::time::Instant::now());
        let _ = self.conn.flush();
        Ok(true)
    }

    /// Stop using zero-copy presentation (import/present failed).
    fn disable_gpu(&mut self, why: &str) {
        if let Some(mut g) = self.gpu.take() {
            eprintln!("ghostdrop: GPU presentation off ({why}); using CPU frames");
            g.free_all(&self.conn);
        }
        self.needs_present = false;
        self.needs_redraw = true;
    }

    /// Another client holds the keyboard: a screen locker (or a menu).
    /// Showing then would put the terminal above the lock screen.
    fn keyboard_grabbed_elsewhere(&self) -> bool {
        let r = self
            .conn
            .grab_keyboard(false, self.root, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .ok()
            .and_then(|c| c.reply().ok());
        match r {
            Some(r) if r.status == GrabStatus::SUCCESS => {
                let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
                false
            }
            Some(r) => r.status == GrabStatus::ALREADY_GRABBED || r.status == GrabStatus::FROZEN,
            None => false,
        }
    }
}

impl Window for X11Win {
    fn show(&mut self, p: &Placement) {
        if !self.visible && !self.offscreen && self.keyboard_grabbed_elsewhere() {
            eprintln!("ghostdrop: not showing: another client has the keyboard grabbed (screen locked?)");
            return;
        }
        let (mx, my, mw, mh) = self.monitor(p.screen_mouse);
        let s = self.scale;
        let (lw, lh) = p.resolve(mw as f64 / s, mh as f64 / s);
        let w = lw.map_or(mw, |v| (v * s).round() as u32).min(mw);
        let h = lh.map_or(mh, |v| (v * s).round() as u32).min(mh);
        let cx = mx + (mw as i32 - w as i32) / 2;
        let cy = my + (mh as i32 - h as i32) / 2;
        let (x, y) = match p.pos {
            Pos::Top => (cx, my),
            Pos::Bottom => (cx, my + mh as i32 - h as i32),
            Pos::Left => (mx, cy),
            Pos::Right => (mx + mw as i32 - w as i32, cy),
            Pos::Center => (cx, cy),
        };
        let resized = (w, h) != (self.geom.2, self.geom.3);
        crate::core::trace!("x11 show: monitor {mx},{my} {mw}x{mh} -> {x},{y} {w}x{h} (resized={resized})");
        self.geom = (x, y, w, h);
        let _ = self.conn.configure_window(
            self.win,
            &ConfigureWindowAux::new().x(x).y(y).width(w).height(h).stack_mode(StackMode::ABOVE),
        );
        if !self.visible {
            if let Ok(Ok(f)) = self.conn.get_input_focus().map(|c| c.reply()) {
                self.prev_focus = f.focus;
            }
            let _ = self.conn.map_window(self.win);
        }
        self.visible = true;
        self.needs_redraw = true;
        let _ = self.conn.flush();
        if resized {
            self.shm_busy = false;
        }
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.mapped = false;
        let _ = self.conn.unmap_window(self.win);
        // libghostty drops its GPU buffers while hidden; let go of ours too.
        if let Some(g) = self.gpu.as_mut() {
            g.free_all(&self.conn);
        }
        self.needs_present = false;
        // Hand focus back to whoever had it before we appeared.
        if self.focused {
            let target = if self.prev_focus > 1 { self.prev_focus } else { u32::from(InputFocus::POINTER_ROOT) };
            let r = self.conn.set_input_focus(InputFocus::POINTER_ROOT, target, x11rb::CURRENT_TIME).map(|c| c.check());
            if !matches!(r, Ok(Ok(()))) {
                let _ = self.conn.set_input_focus(InputFocus::POINTER_ROOT, InputFocus::POINTER_ROOT, x11rb::CURRENT_TIME);
            }
        }
        self.focused = false;
        self.pressed.clear();
        let _ = self.conn.flush();
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        if self.visible && self.mapped && !self.offscreen {
            let _ = self.conn.configure_window(self.win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            let _ = self.conn.set_input_focus(InputFocus::PARENT, self.win, x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
        }
    }

    fn scale(&self) -> f64 {
        self.scale
    }

    fn size(&self) -> (u32, u32) {
        (self.geom.2, self.geom.3)
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

    fn snapshot(&mut self, path: &str) -> Result<(), String> {
        let (w, h) = (self.geom.2 as u16, self.geom.3 as u16);
        if !self.visible || w == 0 || h == 0 {
            return Err("not visible".into());
        }
        // Offscreen (redirected) windows keep their contents in a pixmap.
        let pix = if self.offscreen {
            let p = self.conn.generate_id().map_err(|e| e.to_string())?;
            self.conn.composite_name_window_pixmap(self.win, p).map_err(|e| e.to_string())?.check().map_err(|e| format!("{e:?}"))?;
            Some(p)
        } else {
            None
        };
        let img = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, pix.unwrap_or(self.win), 0, 0, w, h, !0)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("{e:?}"));
        if let Some(p) = pix {
            let _ = self.conn.free_pixmap(p);
        }
        let img = img?;
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        out.reserve(w as usize * h as usize * 3);
        for px in img.data.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        std::fs::write(path, out).map_err(|e| e.to_string())
    }

    fn set_cursor(&mut self, c: Cursor) {
        if self.cursor == Some(c) {
            return;
        }
        self.cursor = Some(c);
        let id = self.cursor_id(c);
        let _ = self.conn.change_window_attributes(self.win, &ChangeWindowAttributesAux::new().cursor(id));
        let _ = self.conn.flush();
    }

    fn set_clipboard(&mut self, clip: Clip, text: String) {
        let sel = match clip {
            Clip::Standard => {
                self.clip_text = text;
                self.atoms.CLIPBOARD
            }
            Clip::Primary => {
                self.primary_text = text;
                self.atoms.PRIMARY
            }
        };
        let _ = self.conn.set_selection_owner(self.win, sel, x11rb::CURRENT_TIME);
        self.owns.insert(sel);
        let _ = self.conn.flush();
    }

    fn read_clipboard(&mut self, clip: Clip, token: u64) {
        let (sel, prop) = match clip {
            Clip::Standard => (self.atoms.CLIPBOARD, self.atoms.GHOSTDROP_CLIPBOARD),
            Clip::Primary => (self.atoms.PRIMARY, self.atoms.GHOSTDROP_PRIMARY),
        };
        if self.owns.contains(&sel) {
            let t = if sel == self.atoms.CLIPBOARD { self.clip_text.clone() } else { self.primary_text.clone() };
            self.clip_results.push((token, Some(t)));
            return;
        }
        let first = !self.reads.contains_key(&sel);
        self.reads.entry(sel).or_default().push(token);
        if first {
            let _ = self.conn.convert_selection(self.win, sel, self.atoms.UTF8_STRING, prop, x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
        }
    }

    fn backend_name(&self) -> &'static str {
        "x11"
    }

    fn hotkey_status(&self) -> String {
        self.hotkey_status.clone()
    }
}

impl X11State {
    fn handle_event(&mut self, ev: Event) {
        let w = &mut self.win;
        match ev {
            Event::XinputRawButtonPress(e) => {
                if w.visible && !w.offscreen && (1..=3).contains(&e.detail) {
                    let child = w.conn.query_pointer(w.root).ok().and_then(|c| c.reply().ok()).map(|p| p.child);
                    crate::core::trace!("raw button {} child={child:?} ours={}", e.detail, w.win);
                    if child.is_some_and(|c| c != w.win) {
                        self.core.outside_click(&mut self.win);
                    }
                }
            }
            Event::KeyPress(e) if e.event == w.root => {
                if w.hotkey.is_some_and(|(kc, _)| kc == e.detail) {
                    self.core.toggle(&mut self.win);
                }
            }
            Event::KeyPress(e) | Event::KeyRelease(e) => {
                let pressed = matches!(ev, Event::KeyPress(_));
                let kc = e.detail as u32;
                let state = if pressed {
                    if w.pressed.insert(kc) { KeyState::Press } else { KeyState::Repeat }
                } else {
                    w.pressed.remove(&kc);
                    KeyState::Release
                };
                if let Some(x) = w.xkb.as_mut() {
                    let input = x.key(kc, state);
                    self.core.key(&mut self.win, input);
                }
            }
            Event::ButtonPress(e) | Event::ButtonRelease(e) => {
                let pressed = matches!(ev, Event::ButtonPress(_));
                let mods = x_mods(e.state);
                let s = w.scale;
                self.core.pointer(&mut self.win, PointerInput::Motion { x: e.event_x as f64 / s, y: e.event_y as f64 / s }, mods);
                let input = match e.detail {
                    1 => Some(PointerInput::Button { button: Button::Left, pressed }),
                    2 => Some(PointerInput::Button { button: Button::Middle, pressed }),
                    3 => Some(PointerInput::Button { button: Button::Right, pressed }),
                    4 if pressed => Some(PointerInput::Scroll { dx: 0.0, dy: -1.0, precise: false }),
                    5 if pressed => Some(PointerInput::Scroll { dx: 0.0, dy: 1.0, precise: false }),
                    6 if pressed => Some(PointerInput::Scroll { dx: -1.0, dy: 0.0, precise: false }),
                    7 if pressed => Some(PointerInput::Scroll { dx: 1.0, dy: 0.0, precise: false }),
                    8 => Some(PointerInput::Button { button: Button::Other(8), pressed }),
                    9 => Some(PointerInput::Button { button: Button::Other(9), pressed }),
                    _ => None,
                };
                if pressed && !self.win.focused {
                    self.win.focus();
                }
                if let Some(i) = input {
                    self.core.pointer(&mut self.win, i, mods);
                }
            }
            Event::MotionNotify(e) => {
                let s = w.scale;
                self.core.pointer(&mut self.win, PointerInput::Motion { x: e.event_x as f64 / s, y: e.event_y as f64 / s }, x_mods(e.state));
            }
            Event::LeaveNotify(e) => self.core.pointer(&mut self.win, PointerInput::Leave, x_mods(e.state)),
            Event::FocusIn(e) => {
                crate::core::trace!("x11 FocusIn mode={:?} detail={:?}", e.mode, e.detail);
                if e.mode != NotifyMode::GRAB && e.mode != NotifyMode::UNGRAB && e.detail != NotifyDetail::POINTER && w.visible {
                    w.focused = true;
                    self.core.focus_changed(&mut self.win, true);
                }
            }
            Event::FocusOut(e) => {
                crate::core::trace!("x11 FocusOut mode={:?} detail={:?}", e.mode, e.detail);
                if e.mode != NotifyMode::GRAB && e.mode != NotifyMode::UNGRAB && e.detail != NotifyDetail::POINTER && e.detail != NotifyDetail::INFERIOR && w.focused {
                    w.focused = false;
                    w.pressed.clear();
                    self.core.focus_changed(&mut self.win, false);
                }
            }
            Event::MapNotify(e) if e.window == w.win => {
                w.mapped = true;
                w.focus();
                w.needs_redraw = true;
            }
            Event::Expose(e) if e.count == 0 => w.needs_redraw = true,
            Event::ShmCompletion(_) => w.shm_busy = false,
            Event::SelectionRequest(e) => w.send_selection(&e),
            Event::SelectionNotify(e) => w.on_selection_notify(&e),
            Event::SelectionClear(e) => {
                w.owns.remove(&e.selection);
            }
            Event::PropertyNotify(e) => w.on_property_notify(&e),
            Event::XkbStateNotify(e) => {
                if let Some(x) = w.xkb.as_mut() {
                    x.update_mask(
                        u16::from(e.base_mods) as u32,
                        u16::from(e.latched_mods) as u32,
                        u16::from(e.locked_mods) as u32,
                        e.base_group as u32,
                        e.latched_group as u32,
                        u8::from(e.locked_group) as u32,
                    );
                }
            }
            Event::XkbNewKeyboardNotify(_) | Event::XkbMapNotify(_) | Event::MappingNotify(_) => {
                w.load_keymap();
                let combo = self.core.settings.hotkey.clone();
                self.win.grab_hotkey(combo.as_ref());
            }
            Event::PresentCompleteNotify(e) if e.window == w.win => {
                if let Some(g) = w.gpu.as_mut() {
                    if e.serial == g.serial {
                        g.inflight = None;
                    }
                }
            }
            Event::Error(e) => {
                let gpu_err = w.gpu.as_ref().is_some_and(|g| e.major_opcode == g.dri3_opcode || e.major_opcode == g.present_opcode);
                if gpu_err {
                    w.disable_gpu(&format!("{:?}", e.error_kind));
                    self.core.set_gpu_backend(false);
                } else {
                    crate::core::trace!("X error {:?} (major {} minor {})", e.error_kind, e.major_opcode, e.minor_opcode);
                }
            }
            _ => {}
        }
    }

    /// Drain X events. A dead connection ends the daemon instead of spinning
    /// on a forever-readable fd.
    fn pump_events(&mut self) {
        loop {
            match self.win.conn.poll_for_event() {
                Ok(Some(ev)) => self.handle_event(ev),
                Ok(None) => break,
                Err(e) => {
                    eprintln!("ghostdrop: X11 connection lost ({e}); exiting");
                    self.core.quit = true;
                    break;
                }
            }
        }
    }

    fn after_dispatch(&mut self) {
        self.pump_events();
        if self.core.quit {
            return;
        }
        let results = std::mem::take(&mut self.win.clip_results);
        for (t, text) in results {
            self.core.clipboard_result(&mut self.win, t, text);
        }
        if ghostty::has_pending() {
            self.core.process(&mut self.win);
        }
        let w = &mut self.win;
        if !(w.visible && w.mapped) {
            let _ = w.conn.flush();
            return;
        }
        let (size, scale) = ((w.geom.2, w.geom.3), w.scale);
        if w.needs_redraw && !w.shm_busy {
            let core = &mut self.core;
            if w.compose(|c| core.draw(size, scale, c)) {
                // With a GPU frame, upload only what's around the terminal;
                // the terminal itself is presented from the DMA-BUF.
                let gpu = w.gpu.as_ref().and(core.gpu_frame(size, scale)).map(|(f, r)| (r.0, r.1, f.width.min(r.2), f.height.min(r.3)));
                match gpu {
                    Some(r) => {
                        w.put(Some(outside(size.0, size.1, r)));
                        w.needs_present = true;
                    }
                    None => w.put(None),
                }
            }
        }
        if w.needs_present {
            match self.core.gpu_frame(size, scale) {
                None => w.needs_present = false,
                Some((frame, rect)) => match w.present_gpu(frame, rect) {
                    Ok(true) => w.needs_present = false,
                    Ok(false) => {}
                    Err(e) => {
                        w.disable_gpu(&e);
                        self.core.set_gpu_backend(false);
                    }
                },
            }
        }
        let _ = self.win.conn.flush();
    }
}

pub fn run(core: Core, show_now: bool) -> Result<(), String> {
    let (conn, screen_num) = XCBConnection::connect(None).map_err(|e| format!("X11 connect: {e}"))?;
    if screen_num != 0 {
        // Keep it simple: Xinerama/RandR setups are a single screen.
    }
    let atoms = Atoms::new(&conn).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    let screen = conn.setup().roots[screen_num].clone();
    let root = screen.root;

    // XKB: keymap from the server + state change events + detectable autorepeat.
    let (mut major, mut minor, mut base_ev, mut base_err) = (0u16, 0u16, 0u8, 0u8);
    if !xkb::x11::setup_xkb_extension(&conn, 1, 0, xkb::x11::SetupXkbExtensionFlags::NoFlags, &mut major, &mut minor, &mut base_ev, &mut base_err) {
        return Err("X server has no XKB extension".into());
    }
    let device = xkb::x11::get_core_keyboard_device_id(&conn);
    let _ = conn.xkb_per_client_flags(
        xxkb::ID::USE_CORE_KBD.into(),
        xxkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
        xxkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
        xxkb::BoolCtrl::default(),
        xxkb::BoolCtrl::default(),
        xxkb::BoolCtrl::default(),
    );
    let ev_mask = xxkb::EventType::NEW_KEYBOARD_NOTIFY | xxkb::EventType::MAP_NOTIFY | xxkb::EventType::STATE_NOTIFY;
    let _ = conn.xkb_select_events(
        xxkb::ID::USE_CORE_KBD.into(),
        xxkb::EventType::default(),
        ev_mask,
        xxkb::MapPart::default(),
        xxkb::MapPart::default(),
        &xxkb::SelectEventsAux::new(),
    );

    // Raw button presses anywhere (XInput2) to autohide on click-outside:
    // WMs like i3 don't move X focus away from an override-redirect window
    // when you click the window they already consider focused.
    let xi2 = conn.xinput_xi_query_version(2, 2).ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.major_version >= 2);
    if xi2 {
        let _ = conn.xinput_xi_select_events(
            root,
            &[xinput::EventMask { deviceid: xinput::Device::ALL_MASTER.into(), mask: vec![xinput::XIEventMask::RAW_BUTTON_PRESS] }],
        );
    }

    // Scale from Xft.dpi (what GTK/Qt use on X11).
    let db = x11rb::resource_manager::new_from_default(&conn).ok();
    let dpi = db.as_ref().and_then(|d| d.get_value::<u32>("Xft.dpi", "").ok().flatten()).unwrap_or(96);
    let scale = ((dpi as f64 / 96.0) * 4.0).round() / 4.0;
    let cursor_handle = db.as_ref().and_then(|d| x11rb::cursor::Handle::new(&conn, screen_num, d).ok()?.reply().ok());

    // ARGB visual only when the background is translucent (needs a compositor).
    let translucent = core.rt.f64("background-opacity", 1.0) < 1.0;
    let argb = translucent
        .then(|| {
            screen.allowed_depths.iter().find(|d| d.depth == 32).and_then(|d| {
                d.visuals.iter().find(|v| v.class == VisualClass::TRUE_COLOR).map(|v| v.visual_id)
            })
        })
        .flatten();
    let (depth, visual, colormap) = match argb {
        Some(v) => {
            let cm = conn.generate_id().map_err(|e| e.to_string())?;
            conn.create_colormap(ColormapAlloc::NONE, cm, root, v).map_err(|e| e.to_string())?;
            (32u8, v, cm)
        }
        None => (screen.root_depth, screen.root_visual, screen.default_colormap),
    };

    let win = conn.generate_id().map_err(|e| e.to_string())?;
    let bg = core.rt.color("background").unwrap_or([0, 0, 0]);
    let bg_pixel = 0xff00_0000 | ((bg[0] as u32) << 16) | ((bg[1] as u32) << 8) | bg[2] as u32;
    let events = EventMask::KEY_PRESS
        | EventMask::KEY_RELEASE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::POINTER_MOTION
        | EventMask::LEAVE_WINDOW
        | EventMask::FOCUS_CHANGE
        | EventMask::EXPOSURE
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE;
    conn.create_window(
        depth,
        win,
        root,
        0,
        0,
        800,
        400,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(bg_pixel)
            .border_pixel(0)
            .colormap(colormap)
            .event_mask(events),
    )
    .map_err(|e| e.to_string())?;
    let _ = conn.change_property8(PropMode::REPLACE, win, AtomEnum::WM_CLASS, AtomEnum::STRING, b"ghostdrop\0Ghostdrop\0");
    let _ = conn.change_property8(PropMode::REPLACE, win, atoms._NET_WM_NAME, atoms.UTF8_STRING, b"Ghostdrop");
    let _ = conn.change_property32(PropMode::REPLACE, win, atoms._NET_WM_PID, AtomEnum::CARDINAL, &[std::process::id()]);
    let _ = conn.change_property32(PropMode::REPLACE, win, atoms._NET_WM_WINDOW_TYPE, AtomEnum::ATOM, &[atoms._NET_WM_WINDOW_TYPE_DROPDOWN_MENU]);
    let gc = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_gc(gc, win, &CreateGCAux::new()).map_err(|e| e.to_string())?;
    // Hotkey KeyPress events arrive on the root window.
    let shm_supported = conn.extension_information(shm::X11_EXTENSION_NAME).ok().flatten().is_some();
    conn.flush().map_err(|e| e.to_string())?;

    let mut st = X11State {
        win: X11Win {
            conn,
            root,
            win,
            gc,
            depth,
            atoms,
            shm: None,
            shm_supported,
            shm_busy: false,
            visible: false,
            focused: false,
            geom: (0, 0, 800, 400),
            scale,
            needs_redraw: false,
            xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb: None,
            xkb_device: device,
            pressed: HashSet::new(),
            hotkey: None,
            hotkey_status: String::new(),
            prev_focus: 0,
            clip_text: String::new(),
            primary_text: String::new(),
            owns: HashSet::new(),
            reads: HashMap::new(),
            tried_string: HashSet::new(),
            incr: None,
            clip_results: Vec::new(),
            cursor_handle,
            cursors: HashMap::new(),
            cursor: None,
            mapped: false,
            gpu: None,
            needs_present: false,
            cpu_buf: Vec::new(),
            offscreen: false,
        },
        core,
    };
    // Test mode: keep the window offscreen (Composite manual redirect); it
    // renders and presents normally but never appears on screen.
    if std::env::var("GHOSTDROP_X11_OFFSCREEN").as_deref() == Ok("1") {
        use x11rb::protocol::xfixes::ConnectionExt as _;
        let c = &st.win.conn;
        let ok = c.composite_query_version(0, 4).ok().and_then(|r| r.reply().ok()).is_some()
            && c.composite_redirect_window(st.win.win, composite::Redirect::MANUAL).ok().and_then(|r| r.check().ok()).is_some()
            // Empty input shape: click-through, so it can't eat clicks on a
            // desktop someone is using while tests run.
            && c.xfixes_query_version(5, 0).ok().and_then(|r| r.reply().ok()).is_some()
            && c.generate_id().ok().is_some_and(|rid| {
                c.xfixes_create_region(rid, &[]).is_ok()
                    && c.xfixes_set_window_shape_region(st.win.win, x11rb::protocol::shape::SK::INPUT, 0, 0, rid)
                        .ok()
                        .and_then(|r| r.check().ok())
                        .is_some()
            });
        if !ok {
            // Never fall back to a real window when a test asked for none.
            return Err("offscreen test mode unavailable (needs Composite + XFixes)".into());
        }
        st.win.offscreen = true;
        eprintln!("ghostdrop: offscreen test mode on (click-through, no focus)");
    }
    // Zero-copy presentation, rendering on the X server's GPU.
    if let Some((gpu, node, mods)) = Gpu::probe(&st.win.conn, st.win.root, st.win.win, st.win.depth) {
        ghostty::set_present_modifiers(&mods);
        match &node {
            Some(n) => {
                crate::core::trace!("gpu: X server device {n}");
                ghostty::set_render_device(n);
            }
            None => crate::core::trace!("gpu: X server device unknown; default render device"),
        }
        st.win.gpu = Some(gpu);
        st.core.set_gpu_backend(true);
    }
    st.win.load_keymap();
    let combo = st.core.settings.hotkey.clone();
    st.win.grab_hotkey(combo.as_ref());

    let mut event_loop: calloop::EventLoop<'static, X11State> = calloop::EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    let (ping, ping_src) = calloop::ping::make_ping().map_err(|e| e.to_string())?;
    ghostty::set_waker(ping.clone());
    crate::install_signal_handlers(ping);
    handle
        .insert_source(ping_src, |_, _, st: &mut X11State| {
            if crate::quit_requested() {
                st.core.quit = true;
            }
            st.core.tick(&mut st.win);
        })
        .map_err(|e| e.to_string())?;
    let fd = st.win.conn.as_raw_fd();
    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }.try_clone_to_owned().map_err(|e| e.to_string())?;
    handle
        .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, st: &mut X11State| {
            st.pump_events();
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.to_string())?;
    let listener = ipc::listen().map_err(|e| format!("control socket: {e}"))?;
    handle
        .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, l, st: &mut X11State| {
            while let Ok((stream, _)) = l.accept() {
                if let Some(cmd) = ipc::read_command(&stream) {
                    let reply = st.core.command(&mut st.win, &cmd);
                    ipc::reply(&stream, &reply);
                }
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.to_string())?;

    st.core.prespawn(&mut st.win);
    if show_now {
        st.core.show(&mut st.win);
    }
    while !st.core.quit {
        let timeout = st
            .core
            .next_deadline()
            .map(|d| d.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or(Duration::from_millis(500))
            .min(Duration::from_millis(500));
        event_loop.dispatch(Some(timeout), &mut st).map_err(|e| e.to_string())?;
        st.core.poll_timers(&mut st.win);
        st.after_dispatch();
    }
    st.core.tabs.clear();
    ipc::cleanup();
    Ok(())
}
