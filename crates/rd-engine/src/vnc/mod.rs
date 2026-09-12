//! VNC (RFB) client.
//!
//! Scope: this is a **controlling** client. It connects to a VNC server, reads
//! its framebuffer and sends pointer and keyboard events. It is deliberately not
//! a server, because the enhanced engine's controlled side speaks the RustDesk
//! protocol only -- see the architecture note in the crate README.
//!
//! Split so each part can be read on its own:
//! - [`protocol`] is the wire: constants, message encoders, rectangle decoding.
//! - [`live`] is the running session: socket, reader thread, framebuffer.
//! - [`auth`] is the DES that VNC authentication needs, with the password bit
//!   reversal that trips up most first implementations.
//! - this module is the session: handshake, initialisation, and the event loop
//!   that turns server messages into framebuffer and clipboard state.

pub mod auth;
pub mod live;
pub mod protocol;
/// A minimal RFB server for tests, inside this crate and from a consumer that
/// enables the `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
pub mod scripted;

use std::io::{Read, Write};
use std::net::TcpStream;

use crate::vnc::auth::answer_challenge;
use crate::vnc::protocol::{
    Encoding, FramebufferUpdate, PREFERRED_PIXEL_FORMAT, PixelFormat, SECURITY_RESULT_FAILED,
    SECURITY_RESULT_OK, SECURITY_RESULT_TOO_MANY, SecurityType, decode_framebuffer_update,
    decode_server_cut_text, encode_client_cut_text, encode_framebuffer_update_request,
    encode_key_event, encode_pointer_event, encode_set_encodings, encode_set_pixel_format,
};

/// Everything that can go wrong while talking to a VNC server.
///
/// Cloneable because `ViewerError` is, and a VNC failure travels through it.
/// `std::io::Error` is not `Clone`, so an I/O failure keeps its message and
/// [`std::io::ErrorKind`] instead of the error object; nothing downstream reads
/// the original, and one cloneable error type across both protocols is worth
/// more than the object.
#[derive(Debug, Clone)]
pub enum VncError {
    /// The socket failed.
    Io {
        what: &'static str,
        kind: std::io::ErrorKind,
        message: String,
    },
    /// The server sent something this client cannot use. The text names what.
    Protocol(String),
    /// The server offered only authentication this client does not implement.
    UnsupportedSecurity(Vec<String>),
    /// The server refused the password.
    AuthenticationRejected(String),
    /// The geometry or a rectangle does not fit the reported framebuffer.
    Geometry(String),
}

impl VncError {
    pub fn protocol(message: String) -> Self {
        Self::Protocol(message)
    }

    pub fn io(what: &'static str, source: std::io::Error) -> Self {
        Self::Io {
            what,
            kind: source.kind(),
            message: source.to_string(),
        }
    }

    pub fn geometry(message: String) -> Self {
        Self::Geometry(message)
    }

    pub fn unsupported_encoding(encoding: Encoding) -> Self {
        Self::Protocol(format!(
            "server used the {} encoding, which this client does not implement",
            encoding.label()
        ))
    }
}

impl std::fmt::Display for VncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                what,
                kind,
                message,
            } => {
                write!(formatter, "vnc {what} ({kind:?}): {message}")
            }
            Self::Protocol(text) => write!(formatter, "vnc protocol: {text}"),
            Self::UnsupportedSecurity(offered) => write!(
                formatter,
                "vnc server offers no supported authentication; it offered {}",
                offered.join(", ")
            ),
            Self::AuthenticationRejected(reason) => {
                write!(formatter, "vnc authentication rejected: {reason}")
            }
            Self::Geometry(text) => write!(formatter, "vnc geometry: {text}"),
        }
    }
}

impl std::error::Error for VncError {}

impl std::fmt::Debug for VncSession {
    /// Hand-written because the useful content is the server's identity and the
    /// negotiated settings, not the socket handle.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VncSession")
            .field("server", &self.info.name)
            .field("version", &self.info.version)
            .field("negotiated", &self.info.negotiated_version)
            .field("security", &self.info.security)
            .field("width", &self.info.width)
            .field("height", &self.info.height)
            .field("encodings", &self.encodings)
            .finish()
    }
}

/// What a connected server turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// The version string the server sent, e.g. "003.889".
    pub version: String,
    /// The version this client answered with.
    pub negotiated_version: String,
    /// The security type that was used.
    pub security: SecurityType,
    /// Framebuffer width in pixels.
    pub width: u16,
    /// Framebuffer height in pixels.
    pub height: u16,
    /// The server's name, as it reports it.
    pub name: String,
    /// The pixel format the server will send.
    pub format: PixelFormat,
}

/// A connected VNC session.
pub struct VncSession {
    stream: TcpStream,
    info: ServerInfo,
    encodings: Vec<Encoding>,
}

/// The largest framebuffer this client will accept, as a sanity bound on what a
/// server may allocate through its reported geometry.
const MAX_FRAMEBUFFER_PIXELS: u64 = 16_384 * 16_384;

impl VncSession {
    /// Connect and complete the handshake, without authenticating.
    ///
    /// Only servers offering the `None` security type can be used this way; use
    /// [`Self::connect_with_password`] otherwise.
    pub fn connect_none(host: &str, port: u16) -> Result<Self, VncError> {
        Self::connect_inner(host, port, None)
    }

    /// Connect and authenticate with a password.
    ///
    /// The password is used for VNC authentication when the server offers it,
    /// and ignored when the server offers `None`.
    pub fn connect_with_password(host: &str, port: u16, password: &str) -> Result<Self, VncError> {
        Self::connect_inner(host, port, Some(password.as_bytes()))
    }

    fn connect_inner(host: &str, port: u16, password: Option<&[u8]>) -> Result<Self, VncError> {
        let stream =
            TcpStream::connect((host, port)).map_err(|error| VncError::io("connect", error))?;
        stream
            .set_nodelay(true)
            .map_err(|error| VncError::io("set_nodelay", error))?;
        let mut session = Self {
            stream,
            info: ServerInfo {
                version: String::new(),
                negotiated_version: String::new(),
                security: SecurityType::None,
                width: 0,
                height: 0,
                name: String::new(),
                format: PREFERRED_PIXEL_FORMAT,
            },
            encodings: vec![Encoding::Raw],
        };
        session.handshake(password)?;
        Ok(session)
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// The encodings this client advertised, after initialisation.
    pub fn encodings(&self) -> &[Encoding] {
        &self.encodings
    }

    /// Read the 12-byte version string and answer with the version to use.
    ///
    /// A server announcing a version this client does not know (3.889 is
    /// common) is not an error: the client replies with 3.8 and the server
    /// downgrades, which is what the protocol requires.
    fn handshake(&mut self, password: Option<&[u8]>) -> Result<(), VncError> {
        let mut banner = [0u8; 12];
        self.stream
            .read_exact(&mut banner)
            .map_err(|error| VncError::io("version banner", error))?;
        let text = String::from_utf8_lossy(&banner).into_owned();
        if !text.starts_with("RFB ") || !text.ends_with('\n') {
            return Err(VncError::protocol(format!(
                "server did not send an RFB version banner: {text:?}"
            )));
        }
        // "RFB 003.889\n" -> major 3, minor 889.
        let version = text[4..11].to_owned();
        let numeric = |slice: &str| slice.parse::<u32>().ok();
        let server_major = numeric(&version[0..3]);
        let server_minor = numeric(&version[4..7]);
        let (major, minor) = match (server_major, server_minor) {
            (Some(major), Some(minor)) => (major, minor),
            _ => {
                return Err(VncError::protocol(format!(
                    "unparsable RFB version {version:?}"
                )));
            }
        };
        if major != 3 {
            return Err(VncError::protocol(format!(
                "RFB major version {major} is not supported (this client speaks 3.x)"
            )));
        }
        self.info.version = version.clone();
        // 3.3 is the floor this client implements; everything newer is answered
        // with 3.8, which is the highest version whose handshake is implemented.
        let answer = if minor >= 8 {
            protocol::PROTOCOL_VERSION_3_8
        } else if minor == 7 {
            protocol::PROTOCOL_VERSION_3_7
        } else {
            protocol::PROTOCOL_VERSION_3_3
        };
        self.stream
            .write_all(answer)
            .map_err(|error| VncError::io("version answer", error))?;
        self.info.negotiated_version = String::from_utf8_lossy(answer)
            .trim_start_matches("RFB ")
            .trim_end()
            .to_owned();

        let security = self.negotiate_security(minor, password)?;
        self.info.security = security;
        self.initialise()?;
        Ok(())
    }

    /// Choose and perform a security type.
    fn negotiate_security(
        &mut self,
        server_minor: u32,
        password: Option<&[u8]>,
    ) -> Result<SecurityType, VncError> {
        if server_minor < 7 {
            // 3.3 has a single server-chosen type with no list.
            let mut code = [0u8; 4];
            self.stream
                .read_exact(&mut code)
                .map_err(|error| VncError::io("security type", error))?;
            let raw = u32::from_be_bytes(code);
            let security = SecurityType::from_code(raw as u8);
            if raw != u32::from(security.code()) {
                return Err(VncError::protocol(format!(
                    "server announced the 32-bit security value {raw}, which is not a security type"
                )));
            }
            if !security.is_supported() {
                return Err(VncError::UnsupportedSecurity(vec![security.label()]));
            }
            if security == SecurityType::VncAuthentication {
                self.vnc_authenticate(password)?;
            }
            return Ok(security);
        }

        let mut count = [0u8; 1];
        self.stream
            .read_exact(&mut count)
            .map_err(|error| VncError::io("security count", error))?;
        if count[0] == 0 {
            let reason = self
                .read_reason()
                .unwrap_or_else(|_| "server refused the connection".to_owned());
            return Err(VncError::AuthenticationRejected(reason));
        }
        let mut offered = vec![0u8; usize::from(count[0])];
        self.stream
            .read_exact(&mut offered)
            .map_err(|error| VncError::io("security list", error))?;
        let types: Vec<SecurityType> = offered
            .iter()
            .map(|code| SecurityType::from_code(*code))
            .collect();

        // Prefer password authentication over None: a server that offers both
        // is offering to skip authentication, and silently taking the weaker
        // option would be a security decision made by the client.
        let chosen = if password.is_some() {
            types
                .iter()
                .copied()
                .find(|kind| *kind == SecurityType::VncAuthentication)
        } else {
            types
                .iter()
                .copied()
                .find(|kind| *kind == SecurityType::None)
        };
        let chosen = match chosen {
            Some(kind) => kind,
            None => {
                return Err(VncError::UnsupportedSecurity(
                    types.iter().map(|kind| kind.label()).collect(),
                ));
            }
        };
        self.stream
            .write_all(&[chosen.code()])
            .map_err(|error| VncError::io("security choice", error))?;
        if chosen == SecurityType::VncAuthentication {
            self.vnc_authenticate(password)?;
        } else {
            self.read_security_result()?;
        }
        Ok(chosen)
    }

    /// Perform the DES challenge-response exchange.
    fn vnc_authenticate(&mut self, password: Option<&[u8]>) -> Result<(), VncError> {
        let password = password.unwrap_or(&[]);
        let mut challenge = [0u8; 16];
        self.stream
            .read_exact(&mut challenge)
            .map_err(|error| VncError::io("authentication challenge", error))?;
        let response = answer_challenge(password, &challenge);
        self.stream
            .write_all(&response)
            .map_err(|error| VncError::io("authentication response", error))?;
        self.read_security_result()
    }

    fn read_security_result(&mut self) -> Result<(), VncError> {
        let mut code = [0u8; 4];
        self.stream
            .read_exact(&mut code)
            .map_err(|error| VncError::io("security result", error))?;
        let big_endian = u32::from_be_bytes(code);
        // RFC 6143 defines the result as a 32-bit big-endian word, and every
        // server that follows it sends 0, 1 or 2 to mean success, failure or too
        // many attempts. Some Apple screen-sharing servers instead put failure on
        // the wire as 01 00 00 00, which is the failure code in the other byte
        // order; that value is not a legal big-endian result, so accepting it
        // cannot mask a conforming server's answer. Only that one byte pattern is
        // tolerated -- a general byte-swap fallback would hide real corruption.
        let value = if big_endian == u32::from_le_bytes([0, 0, 0, SECURITY_RESULT_FAILED as u8]) {
            SECURITY_RESULT_FAILED
        } else {
            big_endian
        };
        match value {
            SECURITY_RESULT_OK => Ok(()),
            SECURITY_RESULT_FAILED => Err(VncError::AuthenticationRejected(
                "server rejected the password".to_owned(),
            )),
            SECURITY_RESULT_TOO_MANY => {
                let reason = self
                    .read_reason()
                    .unwrap_or_else(|_| "too many attempts".to_owned());
                Err(VncError::AuthenticationRejected(reason))
            }
            other => Err(VncError::protocol(format!(
                "unknown security result {other} (bytes {code:02x?})"
            ))),
        }
    }

    /// 3.8 sends a length-prefixed reason string after some failures.
    fn read_reason(&mut self) -> Result<String, VncError> {
        let mut length = [0u8; 4];
        self.stream
            .read_exact(&mut length)
            .map_err(|error| VncError::io("reason length", error))?;
        let size = u32::from_be_bytes(length) as usize;
        if size > 4096 {
            return Err(VncError::protocol(format!(
                "server reason string of {size} bytes is implausible"
            )));
        }
        let mut body = vec![0u8; size];
        self.stream
            .read_exact(&mut body)
            .map_err(|error| VncError::io("reason body", error))?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Read `ServerInit` and send the client's pixel format and encodings.
    fn initialise(&mut self) -> Result<(), VncError> {
        let mut header = [0u8; 24];
        self.stream
            .read_exact(&mut header)
            .map_err(|error| VncError::io("server init", error))?;
        let width = u16::from_be_bytes([header[0], header[1]]);
        let height = u16::from_be_bytes([header[2], header[3]]);
        if width == 0 || height == 0 {
            return Err(VncError::geometry(format!(
                "server reports a {width}x{height} framebuffer"
            )));
        }
        if u64::from(width) * u64::from(height) > MAX_FRAMEBUFFER_PIXELS {
            return Err(VncError::geometry(format!(
                "server reports a {width}x{height} framebuffer, beyond the {MAX_FRAMEBUFFER_PIXELS} pixel limit"
            )));
        }
        let format = PixelFormat::decode(&header[4..20])?;
        let name_length =
            u32::from_be_bytes([header[20], header[21], header[22], header[23]]) as usize;
        if name_length > 4096 {
            return Err(VncError::protocol(format!(
                "server name of {name_length} bytes is implausible"
            )));
        }
        let mut name = vec![0u8; name_length];
        self.stream
            .read_exact(&mut name)
            .map_err(|error| VncError::io("server name", error))?;
        self.info.width = width;
        self.info.height = height;
        self.info.name = String::from_utf8_lossy(&name).into_owned();
        // Remember what the server says it will send: a server may answer with a
        // format other than the one requested, and pixels must be decoded with
        // the format that was actually reported.
        self.info.format = format;
        self.stream
            .write_all(&encode_set_pixel_format(&PREFERRED_PIXEL_FORMAT))
            .map_err(|error| VncError::io("set pixel format", error))?;
        self.encodings = vec![Encoding::Raw];
        let encodings = encode_set_encodings(&self.encodings)?;
        self.stream
            .write_all(&encodings)
            .map_err(|error| VncError::io("set encodings", error))?;
        Ok(())
    }

    /// Ask for `incremental` updates of the whole framebuffer, or of one region.
    pub fn request_update(
        &mut self,
        incremental: bool,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    ) -> Result<(), VncError> {
        let request = encode_framebuffer_update_request(incremental, x, y, width, height);
        self.stream
            .write_all(&request)
            .map_err(|error| VncError::io("framebuffer update request", error))
    }

    /// Ask for a full, non-incremental repaint.
    pub fn request_full_update(&mut self) -> Result<(), VncError> {
        let (width, height) = (self.info.width, self.info.height);
        self.request_update(false, 0, 0, width, height)
    }

    /// Read one server message.
    pub fn read_message(&mut self) -> Result<ServerMessage, VncError> {
        let mut kind = [0u8; 1];
        self.stream
            .read_exact(&mut kind)
            .map_err(|error| VncError::io("message type", error))?;
        match kind[0] {
            protocol::S2C_FRAMEBUFFER_UPDATE => {
                let mut header = [0u8; 3];
                self.stream
                    .read_exact(&mut header)
                    .map_err(|error| VncError::io("update header", error))?;
                let count = u16::from_be_bytes([header[1], header[2]]);
                let format = self.info.format;
                let update = decode_framebuffer_update(count, &mut self.stream, &format)?;
                for rectangle in &update.rectangles {
                    if let protocol::Rectangle::DesktopSize { width, height } = rectangle {
                        if *width == 0 || *height == 0 {
                            return Err(VncError::geometry(
                                "server resized the framebuffer to zero".to_owned(),
                            ));
                        }
                        self.info.width = *width;
                        self.info.height = *height;
                    }
                }
                Ok(ServerMessage::FramebufferUpdate(update))
            }
            protocol::S2C_SET_COLOUR_MAP_ENTRIES => {
                // This client always asks for true colour, so a colour map is
                // unexpected; skip it rather than desynchronise the stream.
                let mut header = [0u8; 5];
                self.stream
                    .read_exact(&mut header)
                    .map_err(|error| VncError::io("colour map header", error))?;
                let count = u16::from_be_bytes([header[3], header[4]]) as usize;
                let mut entries = vec![0u8; count * 6];
                self.stream
                    .read_exact(&mut entries)
                    .map_err(|error| VncError::io("colour map entries", error))?;
                Ok(ServerMessage::ColourMapIgnored { entries: count })
            }
            protocol::S2C_BELL => Ok(ServerMessage::Bell),
            protocol::S2C_SERVER_CUT_TEXT => {
                let mut padding = [0u8; 3];
                self.stream
                    .read_exact(&mut padding)
                    .map_err(|error| VncError::io("cut text padding", error))?;
                Ok(ServerMessage::Clipboard(decode_server_cut_text(
                    &mut self.stream,
                )?))
            }
            other => Err(VncError::protocol(format!(
                "server sent unknown message type {other}"
            ))),
        }
    }

    /// Send a pointer position, optionally with buttons held.
    pub fn send_pointer(&mut self, buttons: u8, x: u16, y: u16) -> Result<(), VncError> {
        if x >= self.info.width || y >= self.info.height {
            return Err(VncError::geometry(format!(
                "pointer ({x},{y}) is outside the {}x{} framebuffer",
                self.info.width, self.info.height
            )));
        }
        let message = encode_pointer_event(buttons, x, y);
        self.stream
            .write_all(&message)
            .map_err(|error| VncError::io("pointer event", error))
    }

    /// Send a key press or release as an X11 keysym.
    pub fn send_key(&mut self, down: bool, keysym: u32) -> Result<(), VncError> {
        let message = encode_key_event(down, keysym);
        self.stream
            .write_all(&message)
            .map_err(|error| VncError::io("key event", error))
    }

    /// A second handle to the same socket, for a close path that cannot take the
    /// session lock because the reader holds it.
    pub fn try_clone_socket(&self) -> std::io::Result<TcpStream> {
        self.stream.try_clone()
    }

    /// Shut the connection down so a blocked read returns.
    ///
    /// The reader thread spends its life inside `read`, so a close has to break
    /// that call rather than wait for the server to send something.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// Send clipboard text to the server.
    pub fn send_clipboard(&mut self, text: &str) -> Result<(), VncError> {
        let message = encode_client_cut_text(text)?;
        self.stream
            .write_all(&message)
            .map_err(|error| VncError::io("client cut text", error))
    }
}

/// A message read from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    FramebufferUpdate(FramebufferUpdate),
    /// A colour map arrived, which this client does not need; the count is
    /// reported so the skip is visible rather than silent.
    ColourMapIgnored {
        entries: usize,
    },
    Bell,
    Clipboard(protocol::ServerCutText),
}

#[cfg(test)]
mod live_tests {
    use super::live::{RGBA_BYTES_PER_PIXEL, VncLiveSession};
    use super::scripted::{ScriptedFrame, ScriptedServer};
    use std::time::{Duration, Instant};

    /// A 4x2 picture with four distinguishable colours, so a blit that writes
    /// the wrong channel or the wrong row cannot pass.
    fn sample_frame() -> ScriptedFrame {
        let pixels = vec![
            0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF, // row 0
            0x000000, 0x123456, 0xABCDEF, 0x808080, // row 1
        ];
        ScriptedFrame {
            width: 4,
            height: 2,
            pixels,
        }
    }

    fn wait_for_frame(session: &VncLiveSession) -> Option<(u32, u32, Vec<u8>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(frame) = session.take_frame() {
                return Some(frame);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn a_frame_from_a_server_reaches_the_frontend_as_opaque_rgba() {
        let server = ScriptedServer::start(sample_frame());
        let session = VncLiveSession::open("127.0.0.1", server.port, None, true).expect("open");

        assert_eq!(session.server_name(), "scripted");
        assert_eq!(session.server_version(), "003.008");
        assert_eq!((session.width(), session.height()), (4, 2));

        let (width, height, pixels) = wait_for_frame(&session).expect("a frame within 5s");
        assert_eq!((width, height), (4, 2));
        assert_eq!(pixels.len(), 4 * 2 * RGBA_BYTES_PER_PIXEL);

        let expected = [
            [0xFF, 0x00, 0x00, 0xFF],
            [0x00, 0xFF, 0x00, 0xFF],
            [0x00, 0x00, 0xFF, 0xFF],
            [0xFF, 0xFF, 0xFF, 0xFF],
            [0x00, 0x00, 0x00, 0xFF],
            [0x12, 0x34, 0x56, 0xFF],
            [0xAB, 0xCD, 0xEF, 0xFF],
            [0x80, 0x80, 0x80, 0xFF],
        ];
        for (index, want) in expected.iter().enumerate() {
            let offset = index * RGBA_BYTES_PER_PIXEL;
            assert_eq!(
                &pixels[offset..offset + RGBA_BYTES_PER_PIXEL],
                want,
                "pixel {index} differs"
            );
        }

        let snapshot = session.snapshot();
        assert_eq!(snapshot.phase, "streaming");
        assert_eq!(snapshot.error, None);
        assert_eq!(snapshot.received_units, 1);
        assert_eq!(snapshot.taken_frames, 1);
        assert!(!snapshot.closed);

        session.close();
    }

    #[test]
    fn a_taken_frame_is_not_handed_out_twice() {
        let server = ScriptedServer::start(sample_frame());
        let session = VncLiveSession::open("127.0.0.1", server.port, None, true).expect("open");
        assert!(wait_for_frame(&session).is_some());
        // The server painted once, so there is nothing new to take. Redrawing an
        // unchanged picture at the frontend's polling rate is wasted work.
        assert!(session.take_frame().is_none());
        session.close();
    }

    #[test]
    fn input_is_accepted_and_a_pointer_outside_the_framebuffer_is_refused() {
        let server = ScriptedServer::start(sample_frame());
        let session = VncLiveSession::open("127.0.0.1", server.port, None, true).expect("open");
        assert!(wait_for_frame(&session).is_some());
        session.send_mouse(0, 1, 1).expect("pointer inside");
        session.send_key(true, 0x61).expect("key press");
        session.send_key(false, 0x61).expect("key release");
        let outside = session.send_mouse(0, 99, 99);
        assert!(
            outside.is_err(),
            "a pointer past the framebuffer must be refused"
        );

        session.close();
    }

    #[test]
    fn closing_a_session_ends_the_reader_and_reports_it() {
        let server = ScriptedServer::start(sample_frame());
        let session = VncLiveSession::open("127.0.0.1", server.port, None, true).expect("open");
        assert!(wait_for_frame(&session).is_some());
        session.close();
        // A close is not a failure: the snapshot reports closed with no error.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = session.snapshot();
            if snapshot.closed {
                assert_eq!(snapshot.error, None, "a requested close is not an error");
                break;
            }
            assert!(Instant::now() < deadline, "session did not report closed");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Closing twice is harmless.
        session.close();
    }

    #[test]
    fn the_forced_capture_rate_is_accepted_but_out_of_range_values_are_not() {
        let server = ScriptedServer::start(sample_frame());
        let session = VncLiveSession::open("127.0.0.1", server.port, None, true).expect("open");
        // VNC servers push on change and have no capture rate, so a valid rate is
        // accepted without affecting the stream, and an impossible one is
        // rejected rather than silently ignored.
        assert!(session.set_requested_fps(60).is_ok());
        assert!(session.set_requested_fps(0).is_err());
        assert!(session.set_requested_fps(241).is_err());
        session.close();
    }
}

/// Read exactly `buffer.len()` bytes, or report that the client went away.
///
/// The scripted servers answer one client and assert nothing about what that
/// client sent, so a short read means the test finished and closed the socket.
/// Returning `false` instead of failing keeps the servers from turning a
/// finished test into a panic -- which is what happened on Windows, where
/// `read_exact` reports the close as an error rather than blocking.
fn read_or_eof(stream: &mut TcpStream, buffer: &mut [u8]) -> bool {
    use std::io::Read;
    match stream.read_exact(buffer) {
        Ok(()) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Answer a handshake with a scripted server so the client can be exercised
    /// without a real VNC server. `script` receives the socket after the banner.
    fn run_against_script(
        script: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            stream.set_nodelay(true).expect("nodelay");
            script(stream);
        });
        (format!("127.0.0.1:{port}"), handle)
    }

    /// Write a ServerInit whose pixel format is the preferred one.
    fn write_server_init(stream: &mut TcpStream, width: u16, height: u16, name: &str) {
        let mut body = Vec::new();
        body.extend_from_slice(&width.to_be_bytes());
        body.extend_from_slice(&height.to_be_bytes());
        body.extend_from_slice(&PREFERRED_PIXEL_FORMAT.encode());
        body.extend_from_slice(&(name.len() as u32).to_be_bytes());
        body.extend_from_slice(name.as_bytes());
        stream.write_all(&body).expect("server init");
        stream.flush().expect("flush");
    }

    #[test]
    fn a_vendor_version_is_downgraded_to_3_8_and_the_session_initialises() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"RFB 003.889\n").expect("banner");
            let mut answer = [0u8; 12];
            if !read_or_eof(&mut stream, &mut answer) {
                return;
            }
            assert_eq!(&answer, protocol::PROTOCOL_VERSION_3_8);
            stream.write_all(&[1, 1]).expect("security: None");
            let mut choice = [0u8; 1];
            if !read_or_eof(&mut stream, &mut choice) {
                return;
            }
            assert_eq!(choice[0], 1);
            stream
                .write_all(&SECURITY_RESULT_OK.to_be_bytes())
                .expect("result");
            write_server_init(&mut stream, 1024, 768, "test");
            // Read the pixel format and encodings the client sends.
            let mut set_format = [0u8; 20];
            if !read_or_eof(&mut stream, &mut set_format) {
                return;
            }
            assert_eq!(set_format[0], protocol::C2S_SET_PIXEL_FORMAT);
            let mut encodings_header = [0u8; 4];
            if !read_or_eof(&mut stream, &mut encodings_header) {
                return;
            }
            assert_eq!(encodings_header[0], protocol::C2S_SET_ENCODINGS);
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let session = VncSession::connect_none(host, port.parse().expect("port")).expect("connect");
        let info = session.info();
        assert_eq!(info.version, "003.889");
        assert_eq!(info.negotiated_version, "003.008");
        assert_eq!(info.security, SecurityType::None);
        assert_eq!((info.width, info.height), (1024, 768));
        assert_eq!(info.name, "test");
        assert_eq!(info.format, PREFERRED_PIXEL_FORMAT);
        assert_eq!(session.encodings(), &[Encoding::Raw]);
    }

    #[test]
    fn a_server_offering_only_proprietary_authentication_is_refused_by_name() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"RFB 003.889\n").expect("banner");
            let mut answer = [0u8; 12];
            if !read_or_eof(&mut stream, &mut answer) {
                return;
            }
            // Apple Remote Desktop, Apple Diffie-Hellman, and one unknown type.
            stream.write_all(&[3, 30, 33, 19]).expect("security list");
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let error = VncSession::connect_none(host, port.parse().expect("port")).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("Apple Remote Desktop"), "{text}");
        assert!(text.contains("security type 19"), "{text}");
    }

    #[test]
    fn vnc_authentication_is_chosen_when_a_password_is_given() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"RFB 003.008\n").expect("banner");
            let mut answer = [0u8; 12];
            if !read_or_eof(&mut stream, &mut answer) {
                return;
            }
            // None first, then VNC authentication: the client must not take the
            // unauthenticated option when it has a password.
            stream.write_all(&[2, 1, 2]).expect("security list");
            let mut choice = [0u8; 1];
            if !read_or_eof(&mut stream, &mut choice) {
                return;
            }
            assert_eq!(choice[0], 2, "client chose the weaker security type");
            stream.write_all(&[0u8; 16]).expect("challenge");
            let mut response = [0u8; 16];
            if !read_or_eof(&mut stream, &mut response) {
                return;
            }
            assert_ne!(response, [0u8; 16], "response was not encrypted");
            stream
                .write_all(&SECURITY_RESULT_OK.to_be_bytes())
                .expect("result");
            write_server_init(&mut stream, 640, 480, "auth");
            let mut rest = [0u8; 24];
            let _ = stream.read_exact(&mut rest);
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let session = VncSession::connect_with_password(host, port.parse().expect("port"), "pw")
            .expect("connect");
        assert_eq!(session.info().security, SecurityType::VncAuthentication);
        assert_eq!((session.info().width, session.info().height), (640, 480));
    }

    #[test]
    fn a_rejected_password_surfaces_the_downgrade_choice_not_a_hang() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"RFB 003.008\n").expect("banner");
            let mut answer = [0u8; 12];
            if !read_or_eof(&mut stream, &mut answer) {
                return;
            }
            stream.write_all(&[1, 2]).expect("security list");
            let mut choice = [0u8; 1];
            if !read_or_eof(&mut stream, &mut choice) {
                return;
            }
            stream.write_all(&[0u8; 16]).expect("challenge");
            let mut response = [0u8; 16];
            if !read_or_eof(&mut stream, &mut response) {
                return;
            }
            stream
                .write_all(&SECURITY_RESULT_FAILED.to_be_bytes())
                .expect("result");
            // 3.8 follows a failure with a reason string.
            let reason = b"Authentication failure";
            stream
                .write_all(&(reason.len() as u32).to_be_bytes())
                .expect("reason length");
            stream.write_all(reason).expect("reason");
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let error = VncSession::connect_with_password(host, port.parse().expect("port"), "bad")
            .unwrap_err();
        assert!(
            matches!(error, VncError::AuthenticationRejected(_)),
            "{error}"
        );
    }

    #[test]
    fn a_failure_code_in_the_other_byte_order_is_still_a_refusal() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"RFB 003.008\n").expect("banner");
            let mut answer = [0u8; 12];
            if !read_or_eof(&mut stream, &mut answer) {
                return;
            }
            stream.write_all(&[1, 2]).expect("security list");
            let mut choice = [0u8; 1];
            if !read_or_eof(&mut stream, &mut choice) {
                return;
            }
            stream.write_all(&[0u8; 16]).expect("challenge");
            let mut response = [0u8; 16];
            if !read_or_eof(&mut stream, &mut response) {
                return;
            }
            // Apple screen sharing sends failure as 01 00 00 00.
            stream.write_all(&[0x01, 0x00, 0x00, 0x00]).expect("result");
            let reason = b"Authentication failure";
            stream
                .write_all(&(reason.len() as u32).to_be_bytes())
                .expect("reason length");
            stream.write_all(reason).expect("reason");
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let error =
            VncSession::connect_with_password(host, port.parse().expect("port"), "pw").unwrap_err();
        assert!(
            matches!(error, VncError::AuthenticationRejected(_)),
            "{error}"
        );
    }

    #[test]
    fn a_zero_sized_framebuffer_is_refused_rather_than_allocated() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"RFB 003.008\n").expect("banner");
            let mut answer = [0u8; 12];
            if !read_or_eof(&mut stream, &mut answer) {
                return;
            }
            stream.write_all(&[1, 1]).expect("security list");
            let mut choice = [0u8; 1];
            if !read_or_eof(&mut stream, &mut choice) {
                return;
            }
            stream
                .write_all(&SECURITY_RESULT_OK.to_be_bytes())
                .expect("result");
            write_server_init(&mut stream, 0, 0, "empty");
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let error = VncSession::connect_none(host, port.parse().expect("port")).unwrap_err();
        assert!(matches!(error, VncError::Geometry(_)), "{error}");
    }

    #[test]
    fn a_non_rfb_banner_is_reported_as_such() {
        let (address, server) = run_against_script(|mut stream| {
            stream.write_all(b"HTTP/1.1 200").expect("banner");
        });

        let (host, port) = address.split_once(':').expect("host:port");
        let error = VncSession::connect_none(host, port.parse().expect("port")).unwrap_err();
        assert!(error.to_string().contains("RFB version banner"), "{error}");
    }
}
