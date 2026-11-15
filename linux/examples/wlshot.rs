//! Test helper: capture the first Wayland output to a PPM file using
//! wlr-screencopy (for compositors without grim around).
//!
//!   cargo run --release --example wlshot -- out.ppm
use std::os::fd::AsFd;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

#[derive(Default)]
struct St {
    shm_formats: Vec<(wl_shm::Format, u32, u32, u32)>,
    buffer_done: bool,
    y_invert: bool,
    ready: bool,
    failed: bool,
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for St {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(wl_output::WlOutput, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, ZwlrScreencopyManagerV1);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for St {
    fn event(st: &mut Self, _: &ZwlrScreencopyFrameV1, ev: zwlr_screencopy_frame_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use zwlr_screencopy_frame_v1::Event;
        match ev {
            Event::Buffer { format: WEnum::Value(f), width, height, stride } => st.shm_formats.push((f, width, height, stride)),
            Event::BufferDone => st.buffer_done = true,
            Event::Flags { flags: WEnum::Value(f) } => st.y_invert = f.contains(zwlr_screencopy_frame_v1::Flags::YInvert),
            Event::Ready { .. } => st.ready = true,
            Event::Failed => st.failed = true,
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "wlshot.ppm".into());
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<St>(&conn)?;
    let qh = queue.handle();
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ())?;
    let output: wl_output::WlOutput = globals.bind(&qh, 1..=4, ())?;
    let manager: ZwlrScreencopyManagerV1 = globals.bind(&qh, 1..=3, ())?;
    let frame = manager.capture_output(0, &output, &qh, ());
    let mut st = St::default();
    while st.shm_formats.is_empty() || (manager.version() >= 3 && !st.buffer_done) {
        queue.blocking_dispatch(&mut st)?;
        if st.failed {
            return Err("screencopy failed".into());
        }
    }
    // Bytes per pixel and where R, G, B sit in memory (DRM formats are
    // little-endian: XRGB8888 is B,G,R,X; BGR888 is R,G,B; ...).
    use wl_shm::Format as F;
    let layout = |f: F| -> Option<(usize, [usize; 3])> {
        Some(match f {
            F::Xrgb8888 | F::Argb8888 => (4, [2, 1, 0]),
            F::Xbgr8888 | F::Abgr8888 => (4, [0, 1, 2]),
            F::Rgb888 => (3, [2, 1, 0]),
            F::Bgr888 => (3, [0, 1, 2]),
            _ => return None,
        })
    };
    let (format, w, h, stride) = *st
        .shm_formats
        .iter()
        .find(|f| layout(f.0).is_some())
        .ok_or_else(|| format!("no supported screencopy format in {:?}", st.shm_formats))?;
    let (bpp, [ri, gi, bi]) = layout(format).unwrap();
    let size = (stride * h) as usize;
    let fd = rustix::fs::memfd_create("wlshot", rustix::fs::MemfdFlags::CLOEXEC)?;
    rustix::fs::ftruncate(&fd, size as u64)?;
    let file = std::fs::File::from(fd);
    let map = unsafe { memmap2::MmapMut::map_mut(&file)? };
    let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, &qh, ());
    frame.copy(&buffer);
    while !st.ready {
        queue.blocking_dispatch(&mut st)?;
        if st.failed {
            return Err("screencopy copy failed".into());
        }
    }
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        let row = if st.y_invert { h - 1 - y } else { y };
        for px in map[(row * stride) as usize..][..w as usize * bpp].chunks_exact(bpp) {
            ppm.extend_from_slice(&[px[ri], px[gi], px[bi]]);
        }
    }
    std::fs::write(&out, ppm)?;
    eprintln!("{out}: {w}x{h} {format:?}");
    Ok(())
}
