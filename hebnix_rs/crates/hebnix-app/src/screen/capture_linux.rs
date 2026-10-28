//! linux-port: capture of the RL window through X11, on a worker thread.
//!
//! Proton runs Rocket League as an X11 client, so this covers X11 desktops
//! (i3, Cinnamon, XFCE, ...) and also Wayland desktops, where RL sits on
//! XWayland and XWayland still hands out its own windows' pixels. Like the
//! Windows build it reads RL's own window, so the overlay drawn on top never
//! shows up in it. Frames come through MIT-SHM when the server has it, else
//! a plain GetImage. A game on Proton's native Wayland driver has no X
//! window, so screen reading is unavailable there.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageFormat, MapState, Window};
use x11rb::rust_connection::RustConnection;

use super::{Frame, IDLE_STOP, Service};

/// how often a missing window is looked for again, and a dead connection retried
const FIND_EVERY: Duration = Duration::from_secs(1);
const RECONNECT_EVERY: Duration = Duration::from_secs(5);
/// smaller windows (launchers, IME helpers) are never the game
const MIN_SIDE: u16 = 200;

/// a SysV shared memory segment attached to the X server
struct ShmSegment {
    seg: shm::Seg,
    addr: *mut u8,
    size: usize,
}

// the mapping is only touched from the worker thread that owns it
unsafe impl Send for ShmSegment {}

impl ShmSegment {
    fn new(conn: &RustConnection, size: usize) -> Option<Self> {
        unsafe {
            let id = libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600);
            if id < 0 {
                return None;
            }
            let addr = libc::shmat(id, std::ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
                return None;
            }
            let seg = conn.generate_id().ok();
            let attached = seg.is_some_and(|seg| {
                // not read-only: ShmGetImage has the server write into it
                conn.shm_attach(seg, id as u32, false)
                    .ok()
                    .and_then(|cookie| cookie.check().ok())
                    .is_some()
            });
            // marked for removal now; the kernel frees it once both sides detach
            libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            if !attached {
                libc::shmdt(addr);
                return None;
            }
            Some(Self {
                seg: seg?,
                addr: addr as *mut u8,
                size,
            })
        }
    }

    fn detach(self, conn: &RustConnection) {
        let _ = conn.shm_detach(self.seg);
        let _ = conn.flush();
        // Drop does the shmdt
    }
}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        unsafe {
            libc::shmdt(self.addr as *const libc::c_void);
        }
    }
}

struct Capture {
    conn: RustConnection,
    root: Window,
    /// the RL window, with its size when last captured
    window: Option<(Window, u16, u16)>,
    found_at: Instant,
    has_shm: bool,
    shm: Option<ShmSegment>,
    warned_format: bool,
}

impl Capture {
    fn connect() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = conn
            .setup()
            .roots
            .get(screen)
            .ok_or("no X screen")?
            .root;
        let has_shm = conn
            .extension_information(shm::X11_EXTENSION_NAME)
            .ok()
            .flatten()
            .is_some();
        Ok(Self {
            conn,
            root,
            window: None,
            found_at: Instant::now() - FIND_EVERY,
            has_shm,
            shm: None,
            warned_format: false,
        })
    }

    fn text_property(&self, window: Window, property: u32) -> String {
        self.conn
            .get_property(false, window, property, AtomEnum::ANY, 0, 1024)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
            .unwrap_or_default()
    }

    /// the largest viewable window that looks like RL, anywhere in the tree
    /// (bare XWayland has no window manager list to ask)
    fn find_window(&self) -> Option<(Window, u16, u16)> {
        let mut best: Option<(Window, u16, u16)> = None;
        let mut stack = vec![self.root];
        while let Some(parent) = stack.pop() {
            let Some(tree) = self.conn.query_tree(parent).ok().and_then(|c| c.reply().ok())
            else {
                continue;
            };
            for child in tree.children {
                stack.push(child);
                let viewable = self
                    .conn
                    .get_window_attributes(child)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_some_and(|attrs| attrs.map_state == MapState::VIEWABLE);
                if !viewable {
                    continue;
                }
                let Some(geometry) = self.conn.get_geometry(child).ok().and_then(|c| c.reply().ok())
                else {
                    continue;
                };
                if geometry.width < MIN_SIDE || geometry.height < MIN_SIDE {
                    continue;
                }
                let class = self.text_property(child, AtomEnum::WM_CLASS.into());
                let title = self.text_property(child, AtomEnum::WM_NAME.into());
                if !is_rl_window(&class, &title) {
                    continue;
                }
                let area = geometry.width as u32 * geometry.height as u32;
                if best.is_none_or(|(_, w, h)| area > w as u32 * h as u32) {
                    best = Some((child, geometry.width, geometry.height));
                }
            }
        }
        best
    }

    /// bits per pixel the server uses for `depth` in ZPixmap images
    fn bits_per_pixel(&self, depth: u8) -> Option<u8> {
        self.conn
            .setup()
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .map(|format| format.bits_per_pixel)
    }

    /// one frame, Ok(None) when there is no RL window to read
    fn grab(&mut self) -> Result<Option<Frame>, String> {
        if self.window.is_none() && self.found_at.elapsed() >= FIND_EVERY {
            self.found_at = Instant::now();
            self.window = self.find_window();
        }
        let Some((window, _, _)) = self.window else {
            return Ok(None);
        };
        let Some(geometry) = self.conn.get_geometry(window).ok().and_then(|c| c.reply().ok())
        else {
            // window closed; look again next time
            self.window = None;
            return Ok(None);
        };
        let (width, height) = (geometry.width, geometry.height);
        self.window = Some((window, width, height));
        if self.bits_per_pixel(geometry.depth) != Some(32) {
            if !self.warned_format {
                self.warned_format = true;
                return Err(format!(
                    "the Rocket League window uses an unsupported pixel format (depth {})",
                    geometry.depth
                ));
            }
            return Ok(None);
        }
        let size = width as usize * height as usize * 4;

        let mut bgra = if self.has_shm {
            match self.grab_shm(window, width, height, size) {
                Some(bytes) => bytes,
                None => {
                    // e.g. a remote display; stay on the plain path from now on
                    self.has_shm = false;
                    self.grab_plain(window, width, height)?
                }
            }
        } else {
            self.grab_plain(window, width, height)?
        };
        if bgra.len() < size {
            return Err("short image from the X server".into());
        }
        bgra.truncate(size);
        // depth 24 leaves the X byte undefined
        for pixel in bgra.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Ok(Some(Frame {
            width: width as u32,
            height: height as u32,
            bgra,
            offset_x: 0,
            offset_y: 0,
            captured_at: Instant::now(),
        }))
    }

    fn grab_shm(&mut self, window: Window, width: u16, height: u16, size: usize) -> Option<Vec<u8>> {
        if self.shm.as_ref().is_none_or(|segment| segment.size < size) {
            if let Some(old) = self.shm.take() {
                old.detach(&self.conn);
            }
            self.shm = Some(ShmSegment::new(&self.conn, size)?);
        }
        let segment = self.shm.as_ref()?;
        self.conn
            .shm_get_image(
                window,
                0,
                0,
                width,
                height,
                !0,
                ImageFormat::Z_PIXMAP.into(),
                segment.seg,
                0,
            )
            .ok()?
            .reply()
            .ok()?;
        let bytes = unsafe { std::slice::from_raw_parts(segment.addr, size) };
        Some(bytes.to_vec())
    }

    fn grab_plain(&self, window: Window, width: u16, height: u16) -> Result<Vec<u8>, String> {
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, !0)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        Ok(reply.data)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(segment) = self.shm.take() {
            segment.detach(&self.conn);
        }
    }
}

/// same rule as the SDK's window matching: a Heroic/Wine class, the Steam
/// app id, or the game's own title (non-Steam shortcuts get a random app id)
fn is_rl_window(class: &str, title: &str) -> bool {
    let class = class.to_lowercase();
    let title = title.to_lowercase();
    class.contains("rocketleague")
        || class.contains("steam_app_252950")
        || title.starts_with("rocket league (")
}

pub(super) fn run_worker(svc: &'static Service) {
    let mut capture: Option<Capture> = None;
    let mut connect_at = Instant::now();
    let mut warned_connect = false;
    let mut last_grab = Instant::now() - Duration::from_secs(60);

    loop {
        if svc.idle_for() > IDLE_STOP {
            drop(capture.take());
            svc.clear_frame();
            svc.worker_running.store(false, Ordering::Release);
            // a request between the idle check and the store above saw the
            // worker as running and didn't spawn one, so pick it up here
            if svc.idle_for() <= IDLE_STOP && !svc.worker_running.swap(true, Ordering::AcqRel) {
                continue;
            }
            return;
        }

        if capture.is_none() && Instant::now() >= connect_at {
            match Capture::connect() {
                Ok(c) => {
                    capture = Some(c);
                    warned_connect = false;
                }
                Err(e) => {
                    if !warned_connect {
                        warned_connect = true;
                        svc.log(&format!(
                            "[Screen] No X display to read Rocket League from ({e}). Screen reading needs the game running on X11 or XWayland."
                        ));
                    }
                    connect_at = Instant::now() + RECONNECT_EVERY;
                }
            }
        }

        if last_grab.elapsed() >= svc.frame_interval() {
            last_grab = Instant::now();
            if let Some(c) = capture.as_mut() {
                match c.grab() {
                    Ok(Some(frame)) => svc.publish(frame),
                    Ok(None) => svc.clear_frame(),
                    Err(e) => {
                        svc.log(&format!("[Screen] Could not read the Rocket League window: {e}"));
                        svc.clear_frame();
                        if c.conn.flush().is_err() {
                            // the X connection died; reconnect later
                            capture = None;
                            connect_at = Instant::now() + RECONNECT_EVERY;
                        }
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::{Capture, is_rl_window};

    /// needs Rocket League open on X11/XWayland: cargo test live_capture -- --ignored
    /// HEBNIX_SCREEN_TEST_OUT=<file.png> also saves the frame
    #[test]
    #[ignore = "needs a running Rocket League window"]
    fn live_capture_reads_the_game_window() {
        let mut capture = Capture::connect().expect("X display");
        let mut frame = None;
        for _ in 0..30 {
            if let Some(f) = capture.grab().expect("grab") {
                frame = Some(f);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let frame = frame.expect("found the Rocket League window");
        let start = std::time::Instant::now();
        let runs = 20;
        for _ in 0..runs {
            capture.grab().expect("grab").expect("frame");
        }
        let per_frame = start.elapsed() / runs;
        let center = frame.pixel(frame.width as i32 / 2, frame.height as i32 / 2);
        let average = frame.average(0, 0, frame.width as i32, frame.height as i32);
        eprintln!(
            "{}x{} shm={} {:?}/frame center={center:?} average={average:?}",
            frame.width, frame.height, capture.has_shm, per_frame
        );
        assert!(!frame.is_black(), "frame is all black");
        if let Ok(path) = std::env::var("HEBNIX_SCREEN_TEST_OUT") {
            let rgba: Vec<u8> = frame
                .bgra
                .chunks_exact(4)
                .flat_map(|p| [p[2], p[1], p[0], 255])
                .collect();
            image::RgbaImage::from_raw(frame.width, frame.height, rgba)
                .unwrap()
                .save(path)
                .unwrap();
        }
    }

    #[test]
    fn matches_rocket_league_windows_only() {
        assert!(is_rl_window(
            "steam_app_3461409850\0steam_app_3461409850\0",
            "Rocket League (64-bit, DX11, Cooked)"
        ));
        assert!(is_rl_window("rocketleague.exe\0rocketleague.exe\0", ""));
        assert!(is_rl_window("steam_app_252950\0steam_app_252950\0", ""));
        assert!(!is_rl_window("firefox\0firefox\0", "Rocket League - YouTube"));
    }
}
