//! DRM device helpers: find the render node (`/dev/dri/renderD*`) of the GPU
//! behind a device fd or device number, via sysfs.

/// Render node of the device `fd` refers to (card or render node).
pub fn render_node_for_fd(fd: i32) -> Option<String> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    render_node_for_dev(st.st_rdev)
}

/// Render node of the DRM device with device number `dev`.
pub fn render_node_for_dev(dev: libc::dev_t) -> Option<String> {
    let (maj, min) = (libc::major(dev), libc::minor(dev));
    let dir = format!("/sys/dev/char/{maj}:{min}/device/drm");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("renderD") {
                return Some(format!("/dev/dri/{name}"));
            }
        }
    }
    // No sysfs view: use the node itself if it exists under /dev/dri.
    let link = std::fs::read_link(format!("/sys/dev/char/{maj}:{min}")).ok()?;
    let name = link.file_name()?.to_string_lossy().into_owned();
    let path = format!("/dev/dri/{name}");
    std::path::Path::new(&path).exists().then_some(path)
}
