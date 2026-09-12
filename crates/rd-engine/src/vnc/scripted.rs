//! A minimal RFB server, so the session layer can be tested end to end.
//!
//! The unit tests in `vnc::live` call the blit helpers directly. That leaves the
//! part that actually runs in production -- a reader thread pulling rectangles
//! off a socket and handing them to a frontend -- unverified, and the only VNC
//! endpoints available here require a password this project does not have. A
//! scripted server closes that gap: it speaks just enough of RFC 6143 to let a
//! real `VncLiveSession` connect and receive a known picture.
//!
//! It lives in `#[cfg(test)]` on purpose. It is a test double, not a server, and
//! nothing should be able to depend on it as one.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

/// A picture the scripted server paints, as 0xRRGGBB per pixel.
pub struct ScriptedFrame {
    pub width: u16,
    pub height: u16,
    /// Row-major pixels, `width * height` entries.
    pub pixels: Vec<u32>,
}

/// A running scripted server.
pub struct ScriptedServer {
    pub port: u16,
    handle: Option<JoinHandle<()>>,
}

impl ScriptedServer {
    /// Start a server that completes a `None`-security handshake and then paints
    /// `frame`, once.
    pub fn start(frame: ScriptedFrame) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted server");
        let port = listener.local_addr().expect("addr").port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_nodelay(true).expect("nodelay");
            serve(&mut stream, frame);
        });
        Self {
            port,
            handle: Some(handle),
        }
    }

    /// Wait for the scripted server to finish serving one client.
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // The client may have gone away; the test still owns the assertion.
            let _ = handle.join();
        }
    }
}

/// Read exactly `buffer.len()` bytes, waiting out a socket timeout.
///
/// Returns `false` only when the peer is gone or has been silent past
/// `idle_limit`. A message may arrive in more than one segment, and a timeout
/// between segments is not the client finishing, so a timed-out read is retried
/// rather than treated as the end of the conversation. Getting that wrong is what
/// made these tests pass locally and fail elsewhere: the server gave up between
/// two segments and the client's next write hit a broken pipe.
fn read_waiting(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    last_heard: &mut std::time::Instant,
    idle_limit: std::time::Duration,
) -> bool {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return false,
            Ok(read) => {
                filled += read;
                *last_heard = std::time::Instant::now();
            }
            Err(error) if error.kind() != std::io::ErrorKind::Interrupted => {
                if last_heard.elapsed() > idle_limit {
                    return false;
                }
            }
            Err(_) => continue,
        }
    }
    true
}

fn serve(stream: &mut TcpStream, frame: ScriptedFrame) {
    // Version exchange. 3.8 keeps the handshake on the simplest path.
    stream.write_all(b"RFB 003.008\n").expect("banner");
    let mut answer = [0u8; 12];
    if stream.read_exact(&mut answer).is_err() {
        return;
    }
    // One security type: None.
    stream.write_all(&[1, 1]).expect("security list");
    let mut choice = [0u8; 1];
    if stream.read_exact(&mut choice).is_err() {
        return;
    }
    stream
        .write_all(&0u32.to_be_bytes())
        .expect("security result");

    // ServerInit with the client's preferred pixel format.
    let mut body = Vec::new();
    body.extend_from_slice(&frame.width.to_be_bytes());
    body.extend_from_slice(&frame.height.to_be_bytes());
    body.extend_from_slice(&crate::vnc::protocol::PREFERRED_PIXEL_FORMAT.encode());
    let name = b"scripted";
    body.extend_from_slice(&(name.len() as u32).to_be_bytes());
    body.extend_from_slice(name);
    stream.write_all(&body).expect("server init");

    // Client messages: SetPixelFormat (20 bytes), SetEncodings (variable), then
    // FramebufferUpdateRequest (10 bytes).
    let mut set_pixel_format = [0u8; 20];
    if stream.read_exact(&mut set_pixel_format).is_err() {
        return;
    }
    let mut encodings_header = [0u8; 4];
    if stream.read_exact(&mut encodings_header).is_err() {
        return;
    }
    let count = u16::from_be_bytes([encodings_header[2], encodings_header[3]]) as usize;
    let mut encodings = vec![0u8; count * 4];
    if stream.read_exact(&mut encodings).is_err() {
        return;
    }
    let mut request = [0u8; 10];
    if stream.read_exact(&mut request).is_err() {
        return;
    }

    // One FramebufferUpdate: message type, padding, rectangle count, then a
    // single Raw rectangle covering the whole framebuffer.
    let mut update = Vec::new();
    update.push(0); // FramebufferUpdate
    update.push(0); // padding
    update.extend_from_slice(&1u16.to_be_bytes());
    update.extend_from_slice(&0u16.to_be_bytes()); // x
    update.extend_from_slice(&0u16.to_be_bytes()); // y
    update.extend_from_slice(&frame.width.to_be_bytes());
    update.extend_from_slice(&frame.height.to_be_bytes());
    update.extend_from_slice(&0i32.to_be_bytes()); // Raw
    for pixel in &frame.pixels {
        // The preferred format is 32bpp little-endian with the maxima in the low
        // bytes, so the wire order is blue, green, red, padding.
        update.push((pixel & 0xFF) as u8);
        update.push(((pixel >> 8) & 0xFF) as u8);
        update.push(((pixel >> 16) & 0xFF) as u8);
        update.push(0);
    }
    if stream.write_all(&update).is_err() {
        return;
    }
    let _ = stream.flush();

    // Keep reading client messages for as long as the client is there.
    //
    // A server that stops reading makes the client's writes fail with a broken
    // pipe, which looks like an input bug rather than a test-double limitation --
    // and that is what a fixed window caused: it was long enough for the test I
    // wrote it for and too short for a slower machine, so the same test passed
    // here and failed on Windows. The loop therefore ends when the peer closes,
    // not on a timer. The timeout only exists so a client that goes quiet without
    // closing cannot pin the thread; a quiet connection is not an exit.
    // Keep reading client messages for as long as the client is there.
    //
    // A server that stops reading makes the client's writes fail, which is what
    // turned two input tests red on Windows and green here. The loop ends when
    // the peer closes, not on a timer: a fixed window was longer than the test it
    // was written for and shorter than a slower machine takes.
    //
    // Reading with a timeout rather than peeking is deliberate. `peek` on a
    // socket with a read timeout behaves differently across platforms, and a
    // wrong answer there aborts the connection instead of waiting.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    // The server has done everything it was asked to, so a client that now says
    // nothing is satisfied rather than slow: waiting forever would deadlock
    // against a client reading the frame that was already sent.
    // Liveness, not a deadline. The server exits when the peer closes, and the
    // only timer is how long it tolerates silence *after the last thing it
    // heard*: a client that is still working keeps resetting it, so a slow
    // machine is served as long as it needs and a finished test closes the socket
    // and ends the thread at once. A single deadline from the last send was the
    // same machine-dependent timer in a different disguise.
    let idle_limit = std::time::Duration::from_secs(10);
    let mut last_heard = std::time::Instant::now();
    loop {
        let mut header = [0u8; 1];
        if !read_waiting(stream, &mut header, &mut last_heard, idle_limit) {
            return;
        }
        let rest = match header[0] {
            // SetPixelFormat, SetEncodings and FramebufferUpdateRequest.
            0 => 19,
            2 => 3,
            3 => 9,
            // KeyEvent and PointerEvent.
            4 => 7,
            5 => 5,
            // ClientCutText: three padding bytes, then a length and the body.
            6 => {
                let mut prefix = [0u8; 7];
                if !read_waiting(stream, &mut prefix, &mut last_heard, idle_limit) {
                    return;
                }
                let length =
                    u32::from_be_bytes([prefix[3], prefix[4], prefix[5], prefix[6]]) as usize;
                let mut body = vec![0u8; length];
                if !read_waiting(stream, &mut body, &mut last_heard, idle_limit) {
                    return;
                }
                continue;
            }
            _ => return,
        };
        let mut scratch = [0u8; 32];
        if !read_waiting(stream, &mut scratch[..rest], &mut last_heard, idle_limit) {
            return;
        }
    }
}
