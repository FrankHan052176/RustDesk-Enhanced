//! A live VNC session: the socket, its reader thread, and the state a frontend
//! reads.
//!
//! [`crate::vnc::VncSession`] is the protocol: it performs the handshake and
//! exposes synchronous reads and writes. A session has to survive while a
//! frontend polls it, so this layer owns the connection behind a lock, runs the
//! read loop on its own thread, and keeps the two things a frontend actually
//! wants:
//!
//! - a **framebuffer**, as RGBA8888, because a VNC server sends raw pixels and
//!   there is no decoder to hand a texture to;
//! - a **snapshot**, in the same shape the RustDesk viewer reports, so the
//!   session facade can describe either backend without special cases.
//!
//! Threading: the socket is shared behind a mutex, so a reader thread and input
//! calls from other threads cannot interleave writes. The read loop holds that
//! lock only while it is waiting for a message, which means input can block
//! until the next server message arrives. That is acceptable for a first
//! implementation and is called out here rather than left to be discovered: the
//! alternative is splitting the socket into read/write halves.

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use crate::vnc::protocol::{Encoding, PixelFormat, Rectangle};
use crate::vnc::{ServerMessage, VncError, VncSession};

/// Pixel layout this session hands to a frontend.
///
/// RGBA8888 with unpremultiplied alpha set to opaque, because the ArkTS side
/// consumes RGBA and a VNC server has no alpha channel to preserve.
pub const RGBA_BYTES_PER_PIXEL: usize = 4;

/// Shared state between the reader thread and the frontend.
#[derive(Debug, Default)]
struct Shared {
    /// RGBA8888, `width * height * 4` bytes, or empty before the first update.
    framebuffer: Vec<u8>,
    /// Set when a new framebuffer is available and not yet taken.
    frame_ready: bool,
    /// Clipboard text the server sent and the frontend has not read.
    clipboard: Option<String>,
    /// A fatal error from the reader thread, including the reason.
    failure: Option<String>,
}

/// Counters and flags a snapshot reports.
#[derive(Debug, Default)]
struct Counters {
    received_updates: AtomicU64,
    received_rectangles: AtomicU64,
    /// Frames handed to the frontend, which is what "rendered" means here.
    taken_frames: AtomicU64,
}

/// A connected VNC session.
pub struct VncLiveSession {
    /// The writing side, used only for client-to-server messages.
    ///
    /// It is a separate handle to the same socket as the reader's, and that is
    /// what makes input work at all. An earlier version kept one handle behind a
    /// mutex so the two sides could not interleave; because the reader blocks in
    /// `read` while holding that mutex, every pointer and key event then waited
    /// for the server to send something, which for an idle VNC desktop is
    /// never. Input appeared to hang rather than to fail, which is worse.
    /// `TcpStream` is safe to clone for this: the socket keeps one send queue,
    /// and writes of these fixed, short messages do not interleave.
    writer: TcpStream,
    /// The reading side, owned by the reader thread.
    inner: Arc<Mutex<VncSession>>,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    counters: Arc<Counters>,
    closed: Arc<AtomicBool>,
    keyboard_allowed: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
    /// The server's name, reported as the peer's hostname.
    name: String,
    version: String,
    security: String,
    /// The pixel format the server said it would send.
    format: PixelFormat,
    reader: Mutex<Option<JoinHandle<()>>>,
}

/// What the session reports about itself, in the shared snapshot vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VncSnapshot {
    pub phase: String,
    pub error: Option<String>,
    pub width: i32,
    pub height: i32,
    pub received_units: u64,
    pub taken_frames: u64,
    pub keyboard_allowed: bool,
    pub clipboard_allowed: bool,
    pub closed: bool,
}

impl VncLiveSession {
    /// Connect, complete the handshake, and start reading.
    pub fn open(
        host: &str,
        port: u16,
        password: Option<&str>,
        allow_input: bool,
    ) -> Result<Self, VncError> {
        let session = match password {
            Some(secret) => VncSession::connect_with_password(host, port, secret)?,
            None => VncSession::connect_none(host, port)?,
        };
        let info = session.info().clone();
        // Taken before the session is wrapped: the reader will hold the mutex for
        // as long as it is waiting, so the close path cannot go through it.
        let writer = session
            .try_clone_socket()
            .map_err(|error| VncError::io("clone socket", error))?;
        // The server may answer with a format other than the one requested, and
        // 16-bit or big-endian servers are common, so pixels are decoded with
        // what it reported rather than assumed to be the requested layout.
        let format = info.format;
        let width = u32::from(info.width);
        let height = u32::from(info.height);
        let session = Self {
            writer,
            inner: Arc::new(Mutex::new(session)),
            shared: Arc::new((Mutex::new(Shared::default()), Condvar::new())),
            counters: Arc::new(Counters::default()),
            closed: Arc::new(AtomicBool::new(false)),
            keyboard_allowed: AtomicBool::new(allow_input),
            width: AtomicU32::new(width),
            height: AtomicU32::new(height),
            format,
            name: info.name.clone(),
            version: info.version.clone(),
            security: info.security.label(),
            reader: Mutex::new(None),
        };
        // A VNC server sends nothing until asked, so ask for a full repaint
        // before the reader thread can block on a socket that stays silent.
        {
            let mut guard = lock(&session.inner);
            guard.request_full_update()?;
        }
        let handle = session.spawn_reader();
        *lock(&session.reader) = Some(handle);
        Ok(session)
    }

    pub fn server_name(&self) -> &str {
        &self.name
    }

    pub fn server_version(&self) -> &str {
        &self.version
    }

    pub fn security_label(&self) -> &str {
        &self.security
    }

    pub fn width(&self) -> u16 {
        self.width.load(Ordering::Acquire) as u16
    }

    pub fn height(&self) -> u16 {
        self.height.load(Ordering::Acquire) as u16
    }

    /// Current state, in the vocabulary the session facade reports.
    pub fn snapshot(&self) -> VncSnapshot {
        let shared = lock(&self.shared.0);
        let closed = self.closed.load(Ordering::Acquire);
        let failure = shared.failure.clone();
        VncSnapshot {
            phase: if closed {
                "closed".to_owned()
            } else if failure.is_some() {
                "failed".to_owned()
            } else if shared.frame_ready || !shared.framebuffer.is_empty() {
                "streaming".to_owned()
            } else {
                "connected".to_owned()
            },
            error: failure,
            width: i32::from(self.width()),
            height: i32::from(self.height()),
            received_units: self.counters.received_updates.load(Ordering::Relaxed),
            taken_frames: self.counters.taken_frames.load(Ordering::Relaxed),
            keyboard_allowed: self.keyboard_allowed.load(Ordering::Acquire),
            // VNC has no permission negotiation: the server decides what it
            // accepts, so both directions are reported as available and a
            // refusal surfaces as a socket error instead.
            clipboard_allowed: true,
            closed,
        }
    }

    /// The newest framebuffer, or `None` when nothing new arrived.
    ///
    /// Taking a frame clears the ready flag, so a frontend that polls at its own
    /// rate does not redraw an unchanged picture.
    pub fn take_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (lock_guard, _) = &*self.shared;
        let mut shared = lock(lock_guard);
        if !shared.frame_ready || shared.framebuffer.is_empty() {
            return None;
        }
        shared.frame_ready = false;
        self.counters.taken_frames.fetch_add(1, Ordering::Relaxed);
        Some((
            u32::from(self.width()),
            u32::from(self.height()),
            shared.framebuffer.clone(),
        ))
    }

    /// Clipboard text the server sent, consuming it.
    pub fn take_clipboard(&self) -> Option<String> {
        let mut shared = lock(&self.shared.0);
        shared.clipboard.take()
    }

    /// Send pointer state. Coordinates are framebuffer pixels.
    pub fn send_mouse(&self, buttons: u8, x: u16, y: u16) -> Result<(), VncError> {
        let (width, height) = (self.width(), self.height());
        if x >= width || y >= height {
            return Err(VncError::geometry(format!(
                "pointer ({x},{y}) is outside the {width}x{height} framebuffer"
            )));
        }
        write_all(
            &self.writer,
            &crate::vnc::protocol::encode_pointer_event(buttons, x, y),
        )
    }

    /// Send a key press or release as an X11 keysym.
    pub fn send_key(&self, down: bool, keysym: u32) -> Result<(), VncError> {
        write_all(
            &self.writer,
            &crate::vnc::protocol::encode_key_event(down, keysym),
        )
    }

    /// Send clipboard text to the server.
    pub fn send_clipboard(&self, text: &str) -> Result<(), VncError> {
        let message = crate::vnc::protocol::encode_client_cut_text(text)?;
        write_all(&self.writer, &message)
    }

    /// Ask for a full repaint, which is how a VNC session refreshes.
    pub fn refresh(&self) -> Result<(), VncError> {
        let (width, height) = (self.width(), self.height());
        let request =
            crate::vnc::protocol::encode_framebuffer_update_request(false, 0, 0, width, height);
        write_all(&self.writer, &request)
    }

    /// Change the forced capture rate.
    ///
    /// A VNC server pushes updates when the screen changes; it has no capture
    /// rate to set. The rate is kept because the session vocabulary has one, and
    /// it is accepted rather than refused so a frontend that always sends it is
    /// not broken -- but it changes nothing on the wire, which
    /// [`VncSnapshot::phase`] and the telemetry make visible instead of
    /// pretending otherwise.
    pub fn set_requested_fps(&self, fps: u32) -> Result<(), VncError> {
        if !(1..=240).contains(&fps) {
            return Err(VncError::protocol(format!("requested fps {fps}")));
        }
        Ok(())
    }

    /// Stop reading and close the socket.
    ///
    /// The reader thread may be blocked in `read`, so it is not joined here:
    /// dropping the socket is what unblocks it, and joining would make a close
    /// depend on the server's timing.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // Shut down through the separate handle: taking the session mutex here
        // would wait for the reader, and the reader is waiting for this shutdown.
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }

    fn spawn_reader(&self) -> JoinHandle<()> {
        let inner = Arc::clone(&self.inner);
        let shared = Arc::clone(&self.shared);
        let counters = Arc::clone(&self.counters);
        let closed = Arc::clone(&self.closed);
        let width = self.width.load(Ordering::Acquire);
        let height = self.height.load(Ordering::Acquire);
        let format = self.format;
        thread::Builder::new()
            .name("vnc-reader".to_owned())
            .spawn(move || {
                let mut framebuffer =
                    vec![0u8; (width as usize) * (height as usize) * RGBA_BYTES_PER_PIXEL];
                let mut current = (width as usize, height as usize);
                while !closed.load(Ordering::Acquire) {
                    let message = {
                        let mut session = lock(&inner);
                        session.read_message()
                    };
                    match message {
                        Ok(message) => match message {
                            ServerMessage::FramebufferUpdate(update) => {
                                let (framebuffer_width, framebuffer_height) = current;
                                let resized = apply_rectangles(
                                    &mut framebuffer,
                                    framebuffer_width,
                                    framebuffer_height,
                                    &format,
                                    &update.rectangles,
                                );
                                if let Some((new_width, new_height)) = resized {
                                    current = (new_width, new_height);
                                }
                                counters.received_updates.fetch_add(1, Ordering::Relaxed);
                                counters
                                    .received_rectangles
                                    .fetch_add(update.rectangles.len() as u64, Ordering::Relaxed);
                                let mut state = lock(&shared.0);
                                state.framebuffer = take_buffer(&mut framebuffer);
                                state.frame_ready = true;
                                shared.1.notify_all();
                            }
                            ServerMessage::Clipboard(text) => {
                                let mut state = lock(&shared.0);
                                state.clipboard = Some(text.text);
                                shared.1.notify_all();
                            }
                            // A bell or a colour map neither changes the picture
                            // nor needs reporting: both are already accounted
                            // for in the protocol layer.
                            ServerMessage::Bell | ServerMessage::ColourMapIgnored { .. } => {}
                        },
                        Err(error) => {
                            let mut state = lock(&shared.0);
                            // A close requested by the frontend is not a failure.
                            if !closed.load(Ordering::Acquire) {
                                state.failure = Some(error.to_string());
                            }
                            shared.1.notify_all();
                            closed.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            })
            .expect("spawn vnc reader")
    }
}

impl Drop for VncLiveSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Apply rectangles to an RGBA framebuffer, returning a new size if the server
/// resized it.
fn apply_rectangles(
    framebuffer: &mut Vec<u8>,
    width: usize,
    height: usize,
    format: &PixelFormat,
    rectangles: &[Rectangle],
) -> Option<(usize, usize)> {
    let mut size: Option<(usize, usize)> = None;
    for rectangle in rectangles {
        match rectangle {
            Rectangle::Raw {
                x,
                y,
                width: rw,
                height: rh,
                pixels,
            } => {
                blit_raw(framebuffer, width, height, format, *x, *y, *rw, *rh, pixels);
            }
            Rectangle::CopyRect {
                x,
                y,
                width: rw,
                height: rh,
                src_x,
                src_y,
            } => {
                copy_rect(framebuffer, width, height, *x, *y, *rw, *rh, *src_x, *src_y);
            }
            Rectangle::Solid {
                x,
                y,
                width: rw,
                height: rh,
                pixel,
            } => {
                fill_solid(framebuffer, width, height, format, *x, *y, *rw, *rh, pixel);
            }
            Rectangle::DesktopSize {
                width: new_width,
                height: new_height,
            } => {
                // A resized framebuffer invalidates every pixel, so it is
                // reallocated rather than stretched.
                size = Some((usize::from(*new_width), usize::from(*new_height)));
            }
        }
    }
    if let Some((new_width, new_height)) = size {
        *framebuffer = vec![0u8; new_width * new_height * RGBA_BYTES_PER_PIXEL];
    }
    size
}

/// Move the accumulated framebuffer into the shared slot, leaving a fresh
/// working buffer behind so the reader keeps its allocation.
fn take_buffer(working: &mut Vec<u8>) -> Vec<u8> {
    let mut taken = Vec::new();
    std::mem::swap(&mut taken, working);
    *working = vec![0u8; taken.len()];
    taken
}

#[allow(clippy::too_many_arguments)]
fn blit_raw(
    framebuffer: &mut [u8],
    width: usize,
    height: usize,
    format: &PixelFormat,
    x: u16,
    y: u16,
    rect_width: u16,
    rect_height: u16,
    pixels: &[u8],
) {
    let step = format.bytes_per_pixel().max(1);
    for row in 0..usize::from(rect_height) {
        let target_y = usize::from(y) + row;
        if target_y >= height {
            break;
        }
        for column in 0..usize::from(rect_width) {
            let target_x = usize::from(x) + column;
            if target_x >= width {
                continue;
            }
            let source = (row * usize::from(rect_width) + column) * step;
            if source + step > pixels.len() {
                return;
            }
            let target = (target_y * width + target_x) * RGBA_BYTES_PER_PIXEL;
            if target + RGBA_BYTES_PER_PIXEL > framebuffer.len() {
                return;
            }
            // Decode with the server's own format: reading the bytes as RGB in a
            // fixed order swaps red and blue on any little-endian server, which
            // is most of them.
            let rgb = match format.decode_pixel(&pixels[source..]) {
                Ok(value) => value,
                Err(_) => return,
            };
            framebuffer[target] = ((rgb >> 16) & 0xFF) as u8;
            framebuffer[target + 1] = ((rgb >> 8) & 0xFF) as u8;
            framebuffer[target + 2] = (rgb & 0xFF) as u8;
            framebuffer[target + 3] = 0xFF;
        }
    }
}

fn copy_rect(
    framebuffer: &mut [u8],
    width: usize,
    height: usize,
    x: u16,
    y: u16,
    rect_width: u16,
    rect_height: u16,
    src_x: u16,
    src_y: u16,
) {
    let row_bytes = usize::from(rect_width) * RGBA_BYTES_PER_PIXEL;
    // Copy through a staging buffer so overlapping regions do not smear.
    let mut staging = vec![0u8; row_bytes * usize::from(rect_height)];
    for row in 0..usize::from(rect_height) {
        let source_y = usize::from(src_y) + row;
        if source_y >= height {
            break;
        }
        let source_start = (source_y * width + usize::from(src_x)) * RGBA_BYTES_PER_PIXEL;
        let source_end = source_start + row_bytes;
        if source_end > framebuffer.len() {
            break;
        }
        staging[row * row_bytes..(row + 1) * row_bytes]
            .copy_from_slice(&framebuffer[source_start..source_end]);
    }
    for row in 0..usize::from(rect_height) {
        let target_y = usize::from(y) + row;
        if target_y >= height {
            break;
        }
        let target_start = (target_y * width + usize::from(x)) * RGBA_BYTES_PER_PIXEL;
        let target_end = target_start + row_bytes;
        if target_end > framebuffer.len() {
            break;
        }
        framebuffer[target_start..target_end]
            .copy_from_slice(&staging[row * row_bytes..(row + 1) * row_bytes]);
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_solid(
    framebuffer: &mut [u8],
    width: usize,
    height: usize,
    format: &PixelFormat,
    x: u16,
    y: u16,
    rect_width: u16,
    rect_height: u16,
    pixel: &[u8],
) {
    let Ok(rgb) = format.decode_pixel(pixel) else {
        return;
    };
    for row in 0..usize::from(rect_height) {
        let target_y = usize::from(y) + row;
        if target_y >= height {
            break;
        }
        for column in 0..usize::from(rect_width) {
            let target_x = usize::from(x) + column;
            if target_x >= width {
                continue;
            }
            let target = (target_y * width + target_x) * RGBA_BYTES_PER_PIXEL;
            if target + RGBA_BYTES_PER_PIXEL > framebuffer.len() {
                return;
            }
            framebuffer[target] = ((rgb >> 16) & 0xFF) as u8;
            framebuffer[target + 1] = ((rgb >> 8) & 0xFF) as u8;
            framebuffer[target + 2] = (rgb & 0xFF) as u8;
            framebuffer[target + 3] = 0xFF;
        }
    }
}

/// Write a complete client message.
fn write_all(stream: &TcpStream, bytes: &[u8]) -> Result<(), VncError> {
    use std::io::Write;
    let mut stream = stream;
    stream
        .write_all(bytes)
        .map_err(|error| VncError::io("client message", error))
}

/// Lock a mutex, treating poisoning as recoverable: a panic in one path must not
/// make the session permanently unusable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The encodings this client advertises. Raw only: every server must support it,
/// and the compressed families each need their own decoder.
pub fn supported_encodings() -> Vec<Encoding> {
    vec![Encoding::Raw]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The format every server is asked for, used by the blit tests.
    const PREFERRED: PixelFormat = crate::vnc::protocol::PREFERRED_PIXEL_FORMAT;

    #[test]
    fn a_raw_rectangle_lands_as_opaque_rgba_at_the_reported_position() {
        let mut framebuffer = vec![0u8; 2 * 2 * RGBA_BYTES_PER_PIXEL];
        // One pixel, three bytes of 0xRRGGBB.
        // Little-endian 32bpp with red at shift 16: 0x112233 is 33 22 11 00.
        let pixels = vec![0x33, 0x22, 0x11, 0x00];
        blit_raw(&mut framebuffer, 2, 2, &PREFERRED, 1, 1, 1, 1, &pixels);
        let target = (1 * 2 + 1) * RGBA_BYTES_PER_PIXEL;
        assert_eq!(&framebuffer[target..target + 4], &[0x11, 0x22, 0x33, 0xFF]);
        // The untouched pixel stays clear.
        assert_eq!(&framebuffer[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn a_rectangle_larger_than_the_framebuffer_is_clipped_not_panicking() {
        let mut framebuffer = vec![0u8; 1 * 1 * RGBA_BYTES_PER_PIXEL];
        let pixels = vec![3u8, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8];
        blit_raw(&mut framebuffer, 1, 1, &PREFERRED, 0, 0, 2, 2, &pixels);
        assert_eq!(&framebuffer[0..4], &[1, 2, 3, 0xFF]);
    }

    #[test]
    fn copy_rect_does_not_smear_when_the_regions_overlap() {
        let mut framebuffer = vec![0u8; 3 * 1 * RGBA_BYTES_PER_PIXEL];
        for index in 0..3 {
            let offset = index * RGBA_BYTES_PER_PIXEL;
            framebuffer[offset] = (index as u8) + 1;
            framebuffer[offset + 3] = 0xFF;
        }
        // Copy pixels 0..2 onto 1..3, which overlaps.
        copy_rect(&mut framebuffer, 3, 1, 1, 0, 2, 1, 0, 0);
        assert_eq!(framebuffer[0], 1);
        assert_eq!(framebuffer[4], 1);
        assert_eq!(framebuffer[8], 2);
    }

    #[test]
    fn a_solid_rectangle_fills_with_opaque_pixels() {
        let mut framebuffer = vec![0u8; 2 * 1 * RGBA_BYTES_PER_PIXEL];
        fill_solid(
            &mut framebuffer,
            2,
            1,
            &PREFERRED,
            0,
            0,
            2,
            1,
            &[0xEF, 0xCD, 0xAB, 0x00],
        );
        assert_eq!(&framebuffer[0..4], &[0xAB, 0xCD, 0xEF, 0xFF]);
        assert_eq!(&framebuffer[4..8], &[0xAB, 0xCD, 0xEF, 0xFF]);
    }

    #[test]
    fn a_desktop_size_rectangle_reallocates_the_framebuffer() {
        let mut framebuffer = vec![0u8; 2 * 2 * RGBA_BYTES_PER_PIXEL];
        let rectangles = vec![Rectangle::DesktopSize {
            width: 3,
            height: 4,
        }];
        let resized = apply_rectangles(&mut framebuffer, 2, 2, &PREFERRED, &rectangles);
        assert_eq!(resized, Some((3, 4)));
        assert_eq!(framebuffer.len(), 3 * 4 * RGBA_BYTES_PER_PIXEL);
    }

    #[test]
    fn the_advertised_encodings_are_the_ones_the_decoder_can_read() {
        // Raw is mandatory for every server; anything else would need a decoder
        // this module does not have, and the protocol layer refuses the rest by
        // name rather than mis-reading them.
        assert_eq!(supported_encodings(), vec![Encoding::Raw]);
    }
}
