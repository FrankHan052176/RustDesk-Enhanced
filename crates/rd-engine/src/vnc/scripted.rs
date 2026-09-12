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
///
/// The thread is detached rather than joined. Joining it would wait for the
/// client to close, and a test closes its session *after* the server handle goes
/// out of scope -- so joining from `Drop` waits for the teardown that has not
/// happened yet. The client closing its side is what ends the thread, and nothing
/// in a test needs to observe that.
pub struct ScriptedServer {
    pub port: u16,
}

impl ScriptedServer {
    /// Start a server that completes a `None`-security handshake and then paints
    /// `frame`, once.
    pub fn start(frame: ScriptedFrame) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted server");
        let port = listener.local_addr().expect("addr").port();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.set_nodelay(true).expect("nodelay");
            serve(&mut stream, frame);
        });
        Self { port }
    }
}

/// Read exactly `buffer.len()` bytes, waiting out a socket timeout.
///
/// Returns `false` when the peer closes or the overall limit passes. A message
/// may arrive in more than one segment, so this loops rather than assuming one
/// read finishes it. An error that is not a close is retried: the platforms
/// disagree about which kind a timed-out receive is, and this server does not
/// need to tell them apart -- a client that is still working counts as progress,
/// and a client that left is recognised by the zero-length read.
fn read_waiting(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    last_heard: &mut std::time::Instant,
    overall_limit: std::time::Duration,
) -> bool {
    let started = *last_heard;
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return false,
            Ok(read) => {
                filled += read;
                *last_heard = std::time::Instant::now();
            }
            Err(error) if error.kind() != std::io::ErrorKind::Interrupted => {
                if started.elapsed() > overall_limit {
                    return false;
                }
            }
            Err(_) => continue,
        }
    }
    true
}

/// Close in a way the peer reads as a close.
///
/// Dropping the socket is not equivalent: the client saw an abrupt drop as
/// `ConnectionAborted` on Windows rather than a clean end of stream, which turned
/// a finished test into a failed input write.
fn close_orderly(stream: &TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn serve(stream: &mut TcpStream, frame: ScriptedFrame) {
    // Every step below can find the client already gone, which is normal: a test
    // asserting a refusal closes as soon as it has its answer. A server that
    // treats that as an error turns a passing test into a failing one, so each
    // read reports the close and the function returns after closing in a way the
    // peer reads as a close rather than an abrupt drop.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let mut last_heard = std::time::Instant::now();
    let overall_limit = std::time::Duration::from_secs(600);

    // Version exchange. 3.8 keeps the handshake on the simplest path.
    if stream.write_all(b"RFB 003.008\n").is_err() {
        return;
    }
    let mut answer = [0u8; 12];
    if !read_waiting(stream, &mut answer, &mut last_heard, overall_limit) {
        close_orderly(stream);
        return;
    }
    // One security type: None.
    if stream.write_all(&[1, 1]).is_err() {
        return;
    }
    let mut choice = [0u8; 1];
    if !read_waiting(stream, &mut choice, &mut last_heard, overall_limit) {
        close_orderly(stream);
        return;
    }
    if stream.write_all(&0u32.to_be_bytes()).is_err() {
        return;
    }

    // ServerInit with the client's preferred pixel format.
    let mut body = Vec::new();
    body.extend_from_slice(&frame.width.to_be_bytes());
    body.extend_from_slice(&frame.height.to_be_bytes());
    body.extend_from_slice(&crate::vnc::protocol::PREFERRED_PIXEL_FORMAT.encode());
    let name = b"scripted";
    body.extend_from_slice(&(name.len() as u32).to_be_bytes());
    body.extend_from_slice(name);
    if stream.write_all(&body).is_err() {
        return;
    }

    // The client's opening messages: SetPixelFormat, SetEncodings, then a
    // FramebufferUpdateRequest.
    let mut set_pixel_format = [0u8; 20];
    if !read_waiting(
        stream,
        &mut set_pixel_format,
        &mut last_heard,
        overall_limit,
    ) {
        close_orderly(stream);
        return;
    }
    let mut encodings_header = [0u8; 4];
    if !read_waiting(
        stream,
        &mut encodings_header,
        &mut last_heard,
        overall_limit,
    ) {
        close_orderly(stream);
        return;
    }
    let count = u16::from_be_bytes([encodings_header[2], encodings_header[3]]) as usize;
    let mut encodings = vec![0u8; count * 4];
    if !read_waiting(stream, &mut encodings, &mut last_heard, overall_limit) {
        close_orderly(stream);
        return;
    }
    let mut request = [0u8; 10];
    if !read_waiting(stream, &mut request, &mut last_heard, overall_limit) {
        close_orderly(stream);
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

    // Keep reading for as long as the client is there, so its input writes
    // succeed. The server has already done everything it was asked to, so this
    // exists only to stay alive: it drains whatever arrives and stops when the
    // peer closes. Draining rather than parsing is deliberate -- nothing here
    // asserts anything about these bytes, and parsing them is what produced four
    // rounds of platform-specific failures.
    let mut scratch = [0u8; 4096];
    loop {
        match stream.read(&mut scratch) {
            Ok(0) => {
                close_orderly(stream);
                return;
            }
            Ok(_) => {
                last_heard = std::time::Instant::now();
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                // A timed-out receive is not the client leaving; the platforms
                // disagree about which error kind it is, so only a long silence
                // ends the loop.
                if last_heard.elapsed() > std::time::Duration::from_secs(600) {
                    close_orderly(stream);
                    return;
                }
            }
        }
    }
}
