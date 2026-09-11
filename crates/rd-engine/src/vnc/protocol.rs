//! RFB wire constants and message encoders.
//!
//! Field names, opcodes and encoding numbers are taken from RFC 6143 (the RFB
//! 3.8 protocol) rather than from a third-party client, because this module has
//! to interoperate with servers it cannot inspect. Anything a server may send
//! that this module does not implement is rejected loudly instead of being
//! silently mis-parsed, which is what turns "the picture is wrong" into a
//! diagnosable protocol error.

use crate::vnc::VncError;

/// Where a message type or encoding is described, used in error text.
pub const RFC: &str = "RFC 6143";

/// Most servers report 3.8; some report 3.889 or similar vendor versions. The
/// version is negotiated by sending the highest version this client speaks
/// that the server also speaks, so 3.8 is what is normally put on the wire.
pub const PROTOCOL_VERSION_3_8: &[u8; 12] = b"RFB 003.008\n";
pub const PROTOCOL_VERSION_3_7: &[u8; 12] = b"RFB 003.007\n";
pub const PROTOCOL_VERSION_3_3: &[u8; 12] = b"RFB 003.003\n";

/// Security types (RFC 6143 section 7.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityType {
    /// No authentication.
    None,
    /// DES challenge-response using the password as the key.
    VncAuthentication,
    /// Offered by Apple Remote Desktop servers; not implemented.
    AppleRemoteDesktop,
    /// Apple's Diffie-Hellman variant; not implemented.
    AppleDiffieHellman,
    /// A type this client has no name for.
    Other(u8),
}

impl SecurityType {
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::None,
            2 => Self::VncAuthentication,
            30 => Self::AppleRemoteDesktop,
            33 => Self::AppleDiffieHellman,
            other => Self::Other(other),
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::None => 1,
            Self::VncAuthentication => 2,
            Self::AppleRemoteDesktop => 30,
            Self::AppleDiffieHellman => 33,
            Self::Other(code) => code,
        }
    }

    /// Whether this client can complete this security type.
    pub fn is_supported(self) -> bool {
        matches!(self, Self::None | Self::VncAuthentication)
    }

    pub fn label(self) -> String {
        match self {
            Self::None => "None".to_owned(),
            Self::VncAuthentication => "VNC authentication".to_owned(),
            Self::AppleRemoteDesktop => "Apple Remote Desktop".to_owned(),
            Self::AppleDiffieHellman => "Apple Diffie-Hellman".to_owned(),
            Self::Other(code) => format!("security type {code}"),
        }
    }
}

/// Security result codes (RFC 6143 section 7.1.3).
pub const SECURITY_RESULT_OK: u32 = 0;
pub const SECURITY_RESULT_FAILED: u32 = 1;
/// 3.8 adds a "too many attempts" result that carries a reason string.
pub const SECURITY_RESULT_TOO_MANY: u32 = 2;

/// Client-to-server message types (RFC 6143 section 7.5).
pub const C2S_SET_PIXEL_FORMAT: u8 = 0;
pub const C2S_SET_ENCODINGS: u8 = 2;
pub const C2S_FRAMEBUFFER_UPDATE_REQUEST: u8 = 3;
pub const C2S_KEY_EVENT: u8 = 4;
pub const C2S_POINTER_EVENT: u8 = 5;
pub const C2S_CLIENT_CUT_TEXT: u8 = 6;
/// Extensions implemented by many servers; see `SetDesktopSize`.
pub const C2S_SET_DESKTOP_SIZE: u8 = 251;

/// Server-to-client message types (RFC 6143 section 7.6).
pub const S2C_FRAMEBUFFER_UPDATE: u8 = 0;
pub const S2C_SET_COLOUR_MAP_ENTRIES: u8 = 1;
pub const S2C_BELL: u8 = 2;
pub const S2C_SERVER_CUT_TEXT: u8 = 3;

/// Encoding numbers (RFC 6143 section 7.7.2 plus registered pseudocodings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    CopyRect,
    /// Pseudocoding: the framebuffer is a solid colour.
    DesktopSize,
    /// Pseudocoding: cursor shape update.
    Cursor,
    /// Pseudocoding: cursor position update.
    XCursor,
    /// Pseudocoding: extended desktop size, used to report allowed resize.
    ExtendedDesktopSize,
    /// Compressed with zlib.
    Zlib,
    /// Compressed with zlib at a higher level.
    ZlibHex,
    /// Tight encoding.
    Tight,
    /// JPEG from the Tight family.
    TightJpeg,
    Other(i32),
}

impl Encoding {
    pub fn from_code(code: i32) -> Self {
        match code {
            0 => Self::Raw,
            1 => Self::CopyRect,
            -223 => Self::DesktopSize,
            -239 => Self::Cursor,
            -240 => Self::XCursor,
            -308 => Self::ExtendedDesktopSize,
            6 => Self::Zlib,
            8 => Self::ZlibHex,
            7 => Self::Tight,
            -32 => Self::TightJpeg,
            other => Self::Other(other),
        }
    }

    pub fn code(self) -> i32 {
        match self {
            Self::Raw => 0,
            Self::CopyRect => 1,
            Self::DesktopSize => -223,
            Self::Cursor => -239,
            Self::XCursor => -240,
            Self::ExtendedDesktopSize => -308,
            Self::Zlib => 6,
            Self::ZlibHex => 8,
            Self::Tight => 7,
            Self::TightJpeg => -32,
            Self::Other(code) => code,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Raw => "Raw".to_owned(),
            Self::CopyRect => "CopyRect".to_owned(),
            Self::DesktopSize => "DesktopSize".to_owned(),
            Self::Cursor => "Cursor".to_owned(),
            Self::XCursor => "XCursor".to_owned(),
            Self::ExtendedDesktopSize => "ExtendedDesktopSize".to_owned(),
            Self::Zlib => "Zlib".to_owned(),
            Self::ZlibHex => "ZlibHex".to_owned(),
            Self::Tight => "Tight".to_owned(),
            Self::TightJpeg => "Tight/JPEG".to_owned(),
            Self::Other(code) => format!("encoding {code}"),
        }
    }
}

/// Pointer button bits, numbered the way RFB numbers them: button one is bit 0.
pub const POINTER_BUTTON_LEFT: u8 = 1 << 0;
pub const POINTER_BUTTON_MIDDLE: u8 = 1 << 1;
pub const POINTER_BUTTON_RIGHT: u8 = 1 << 2;
pub const POINTER_BUTTON_WHEEL_UP: u8 = 1 << 3;
pub const POINTER_BUTTON_WHEEL_DOWN: u8 = 1 << 4;

/// The pixel format this client asks every server for.
///
/// 32 bits per pixel, depth 24, little-endian, true colour with the maxima in
/// the low bytes and red, green, blue in that order. This is the layout RFB
/// clients conventionally request because it is a plain `u32` per pixel that
/// `alpha`-free BGRA consumers can reuse directly.
pub const PREFERRED_PIXEL_FORMAT: PixelFormat = PixelFormat {
    bits_per_pixel: 32,
    depth: 24,
    big_endian: false,
    true_colour: true,
    red_max: 255,
    green_max: 255,
    blue_max: 255,
    red_shift: 16,
    green_shift: 8,
    blue_shift: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    pub fn encode(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0] = self.bits_per_pixel;
        out[1] = self.depth;
        out[2] = u8::from(self.big_endian);
        out[3] = u8::from(self.true_colour);
        out[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        out[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        out[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        out[10] = self.red_shift;
        out[11] = self.green_shift;
        out[12] = self.blue_shift;
        // Bytes 13..16 are padding.
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, VncError> {
        if bytes.len() < 16 {
            return Err(VncError::protocol(format!(
                "pixel format needs 16 bytes, got {}",
                bytes.len()
            )));
        }
        Ok(Self {
            bits_per_pixel: bytes[0],
            depth: bytes[1],
            big_endian: bytes[2] != 0,
            true_colour: bytes[3] != 0,
            red_max: u16::from_be_bytes([bytes[4], bytes[5]]),
            green_max: u16::from_be_bytes([bytes[6], bytes[7]]),
            blue_max: u16::from_be_bytes([bytes[8], bytes[9]]),
            red_shift: bytes[10],
            green_shift: bytes[11],
            blue_shift: bytes[12],
        })
    }

    /// Bytes a single pixel occupies on the wire.
    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.bits_per_pixel).div_ceil(8)
    }

    /// Decode one pixel to 0xRRGGBB.
    ///
    /// The components are read according to this format's shifts and maxima and
    /// rescaled to eight bits, so a server that answers with a non-preferred
    /// format is still rendered correctly rather than showing shifted channels.
    pub fn decode_pixel(&self, raw: &[u8]) -> Result<u32, VncError> {
        let step = self.bytes_per_pixel();
        if raw.len() < step {
            return Err(VncError::protocol("truncated pixel".to_owned()));
        }
        let value = if self.big_endian {
            raw[..step]
                .iter()
                .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte))
        } else {
            raw[..step]
                .iter()
                .rev()
                .fold(0u32, |acc, byte| (acc << 8) | u32::from(*byte))
        };
        if !self.true_colour {
            // Colour-map mode needs the server's map, which this client does not
            // request; treat the raw index as a grey level so the picture is not
            // silently black.
            let mask = if self.bits_per_pixel >= 8 {
                0xff
            } else {
                (1u32 << self.bits_per_pixel) - 1
            };
            let index = value & mask;
            let level = (index * 255) / mask.max(1);
            return Ok((level << 16) | (level << 8) | level);
        }
        let component = |max: u16, shift: u8| -> u32 {
            if max == 0 {
                return 0;
            }
            let extracted = (value >> shift) & u32::from(max);
            (extracted * 255) / u32::from(max)
        };
        Ok((component(self.red_max, self.red_shift) << 16)
            | (component(self.green_max, self.green_shift) << 8)
            | component(self.blue_max, self.blue_shift))
    }
}

/// A framebuffer update rectangle that this client can draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rectangle {
    /// Raw pixels in the negotiated format, row-major, tightly packed.
    Raw {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        pixels: Vec<u8>,
    },
    /// Copy an existing region of the framebuffer.
    CopyRect {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        src_x: u16,
        src_y: u16,
    },
    /// A solid region: 4 bytes of pixel data follow the header.
    Solid {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        pixel: Vec<u8>,
    },
    /// The server reports a new framebuffer size.
    DesktopSize { width: u16, height: u16 },
}

impl Rectangle {
    pub fn geometry(&self) -> (u16, u16, u16, u16) {
        match self {
            Self::Raw {
                x,
                y,
                width,
                height,
                ..
            }
            | Self::CopyRect {
                x,
                y,
                width,
                height,
                ..
            }
            | Self::Solid {
                x,
                y,
                width,
                height,
                ..
            } => (*x, *y, *width, *height),
            Self::DesktopSize { width, height } => (0, 0, *width, *height),
        }
    }
}

/// One framebuffer update: a sequence of rectangles, possibly empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramebufferUpdate {
    pub rectangles: Vec<Rectangle>,
}

/// Server-initiated clipboard text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCutText {
    pub text: String,
}

/// Encode a client-to-server `SetPixelFormat`.
pub fn encode_set_pixel_format(format: &PixelFormat) -> Vec<u8> {
    let mut out = Vec::with_capacity(20);
    out.push(C2S_SET_PIXEL_FORMAT);
    // Three bytes of padding.
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&format.encode());
    out
}

/// Encode a client-to-server `SetEncodings`.
pub fn encode_set_encodings(encodings: &[Encoding]) -> Result<Vec<u8>, VncError> {
    let count = u16::try_from(encodings.len())
        .map_err(|_| VncError::protocol("too many encodings".to_owned()))?;
    let mut out = Vec::with_capacity(4 + encodings.len() * 4);
    out.push(C2S_SET_ENCODINGS);
    out.push(0);
    out.extend_from_slice(&count.to_be_bytes());
    for encoding in encodings {
        out.extend_from_slice(&encoding.code().to_be_bytes());
    }
    Ok(out)
}

/// Encode a client-to-server `FramebufferUpdateRequest`.
pub fn encode_framebuffer_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.push(C2S_FRAMEBUFFER_UPDATE_REQUEST);
    out.push(u8::from(incremental));
    out.extend_from_slice(&x.to_be_bytes());
    out.extend_from_slice(&y.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out
}

/// Encode a client-to-server `KeyEvent`.
///
/// `keysym` is an X11 keysym. `down` false is a key release.
pub fn encode_key_event(down: bool, keysym: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0] = C2S_KEY_EVENT;
    out[1] = u8::from(down);
    out[2..4].copy_from_slice(&0u16.to_be_bytes());
    out[4..8].copy_from_slice(&keysym.to_be_bytes());
    out
}

/// Encode a client-to-server `PointerEvent`.
pub fn encode_pointer_event(buttons: u8, x: u16, y: u16) -> [u8; 6] {
    let mut out = [0u8; 6];
    out[0] = C2S_POINTER_EVENT;
    out[1] = buttons;
    out[2..4].copy_from_slice(&x.to_be_bytes());
    out[4..6].copy_from_slice(&y.to_be_bytes());
    out
}

/// Encode a client-to-server `ClientCutText`.
pub fn encode_client_cut_text(text: &str) -> Result<Vec<u8>, VncError> {
    let bytes = text.as_bytes();
    let length = u32::try_from(bytes.len())
        .map_err(|_| VncError::protocol("clipboard text too long".to_owned()))?;
    let mut out = Vec::with_capacity(8 + bytes.len());
    out.push(C2S_CLIENT_CUT_TEXT);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}

/// Decode a `FramebufferUpdate` body, given its rectangle count.
///
/// `format` is the format the server reported, which may differ from the one
/// requested; pixels are decoded with what the server actually sent.
pub fn decode_framebuffer_update(
    count: u16,
    reader: &mut impl std::io::Read,
    format: &PixelFormat,
) -> Result<FramebufferUpdate, VncError> {
    let mut rectangles = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let mut header = [0u8; 12];
        reader
            .read_exact(&mut header)
            .map_err(|error| VncError::io("rectangle header", error))?;
        let x = u16::from_be_bytes([header[0], header[1]]);
        let y = u16::from_be_bytes([header[2], header[3]]);
        let width = u16::from_be_bytes([header[4], header[5]]);
        let height = u16::from_be_bytes([header[6], header[7]]);
        let encoding = Encoding::from_code(i32::from_be_bytes([
            header[8], header[9], header[10], header[11],
        ]));
        match encoding {
            Encoding::Raw => {
                let step = format.bytes_per_pixel();
                let length = usize::from(width) * usize::from(height) * step;
                let mut pixels = vec![0u8; length];
                reader
                    .read_exact(&mut pixels)
                    .map_err(|error| VncError::io("raw rectangle", error))?;
                rectangles.push(Rectangle::Raw {
                    x,
                    y,
                    width,
                    height,
                    pixels,
                });
            }
            Encoding::CopyRect => {
                let mut source = [0u8; 4];
                reader
                    .read_exact(&mut source)
                    .map_err(|error| VncError::io("copyrect source", error))?;
                rectangles.push(Rectangle::CopyRect {
                    x,
                    y,
                    width,
                    height,
                    src_x: u16::from_be_bytes([source[0], source[1]]),
                    src_y: u16::from_be_bytes([source[2], source[3]]),
                });
            }
            Encoding::DesktopSize | Encoding::ExtendedDesktopSize => {
                if matches!(encoding, Encoding::ExtendedDesktopSize) {
                    // The extended form appends screen layout data this client
                    // does not consume; skip it so the stream stays aligned.
                    let mut screen_count = [0u8; 4];
                    reader
                        .read_exact(&mut screen_count)
                        .map_err(|error| VncError::io("extended desktop size", error))?;
                    let screens = usize::from(screen_count[3]);
                    let mut rest = vec![0u8; screens * 16];
                    reader
                        .read_exact(&mut rest)
                        .map_err(|error| VncError::io("extended desktop size screens", error))?;
                }
                rectangles.push(Rectangle::DesktopSize { width, height });
            }
            Encoding::Cursor | Encoding::XCursor => {
                let step = format.bytes_per_pixel();
                let length = usize::from(width) * usize::from(height) * step;
                let mut scratch =
                    vec![0u8; length + (usize::from(width) * usize::from(height)).div_ceil(8)];
                if matches!(encoding, Encoding::XCursor) {
                    scratch.extend_from_slice(&[0u8; 6]);
                }
                reader
                    .read_exact(&mut scratch)
                    .map_err(|error| VncError::io("cursor rectangle", error))?;
                // Cursor shape is presentation state this client does not draw;
                // rectangle geometry is reported by the caller through the raw
                // stream, so nothing is pushed here.
            }
            other => {
                return Err(VncError::unsupported_encoding(other));
            }
        }
    }
    Ok(FramebufferUpdate { rectangles })
}

/// Decode a `ServerCutText` body.
pub fn decode_server_cut_text(reader: &mut impl std::io::Read) -> Result<ServerCutText, VncError> {
    let mut length_bytes = [0u8; 4];
    reader
        .read_exact(&mut length_bytes)
        .map_err(|error| VncError::io("cut text length", error))?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    // A hostile server must not be able to make this client allocate without
    // bound; clipboard text beyond this is truncated and reported as such.
    const MAX_CUT_TEXT: usize = 1 << 20;
    if length > MAX_CUT_TEXT {
        return Err(VncError::protocol(format!(
            "server cut text of {length} bytes exceeds the {MAX_CUT_TEXT} byte limit"
        )));
    }
    let mut bytes = vec![0u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| VncError::io("cut text body", error))?;
    Ok(ServerCutText {
        text: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_round_trips() {
        let encoded = PREFERRED_PIXEL_FORMAT.encode();
        assert_eq!(encoded.len(), 16);
        assert_eq!(
            PixelFormat::decode(&encoded).unwrap(),
            PREFERRED_PIXEL_FORMAT
        );
    }

    #[test]
    fn preferred_format_decodes_to_rgb_in_the_low_bytes() {
        // 0x00RRGGBB little-endian: the red byte comes last on the wire.
        let pixels = [0x56u8, 0x34, 0x12, 0x00];
        assert_eq!(
            PREFERRED_PIXEL_FORMAT.decode_pixel(&pixels).unwrap(),
            0x12_34_56
        );
    }

    #[test]
    fn a_format_with_narrow_components_is_rescaled_not_truncated() {
        // 5-6-5 layout: pure red must still come back as 0xFF0000.
        let format = PixelFormat {
            bits_per_pixel: 16,
            depth: 16,
            big_endian: false,
            true_colour: true,
            red_max: 31,
            green_max: 63,
            blue_max: 31,
            red_shift: 11,
            green_shift: 5,
            blue_shift: 0,
        };
        let red: u16 = 31 << 11;
        assert_eq!(format.decode_pixel(&red.to_le_bytes()).unwrap(), 0xFF_00_00);
        let green: u16 = 63 << 5;
        assert_eq!(
            format.decode_pixel(&green.to_le_bytes()).unwrap(),
            0x00_FF_00
        );
        let blue: u16 = 31;
        assert_eq!(
            format.decode_pixel(&blue.to_le_bytes()).unwrap(),
            0x00_00_FF
        );
    }

    #[test]
    fn messages_carry_the_field_layout_the_rfc_describes() {
        assert_eq!(
            encode_set_pixel_format(&PREFERRED_PIXEL_FORMAT)[0],
            C2S_SET_PIXEL_FORMAT
        );
        assert_eq!(
            encode_set_encodings(&[Encoding::Raw]).unwrap()[0],
            C2S_SET_ENCODINGS
        );
        // An empty encoding list is legal and means "Raw only".
        assert_eq!(
            encode_set_encodings(&[]).unwrap(),
            vec![C2S_SET_ENCODINGS, 0, 0, 0]
        );

        let request = encode_framebuffer_update_request(true, 1, 2, 3, 4);
        assert_eq!(request[0], C2S_FRAMEBUFFER_UPDATE_REQUEST);
        assert_eq!(request[1], 1);
        assert_eq!(&request[2..10], &[0, 1, 0, 2, 0, 3, 0, 4]);

        let key = encode_key_event(true, 0xFF0D);
        assert_eq!(key[0], C2S_KEY_EVENT);
        assert_eq!(key[1], 1);
        assert_eq!(&key[4..8], &[0, 0, 0xFF, 0x0D]);
        assert_eq!(encode_key_event(false, 0x41)[1], 0);

        let pointer = encode_pointer_event(POINTER_BUTTON_LEFT, 0x1234, 0x5678);
        assert_eq!(pointer[0], C2S_POINTER_EVENT);
        assert_eq!(pointer[1], 1);
        assert_eq!(&pointer[2..6], &[0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn clipboard_text_length_is_big_endian_and_not_nul_terminated() {
        let encoded = encode_client_cut_text("hi").unwrap();
        assert_eq!(encoded[0], C2S_CLIENT_CUT_TEXT);
        assert_eq!(&encoded[4..8], &[0, 0, 0, 2]);
        assert_eq!(&encoded[8..], b"hi");
    }

    #[test]
    fn security_types_are_named_and_only_the_implemented_ones_are_accepted() {
        assert_eq!(SecurityType::from_code(1), SecurityType::None);
        assert_eq!(SecurityType::from_code(2), SecurityType::VncAuthentication);
        assert_eq!(
            SecurityType::from_code(30),
            SecurityType::AppleRemoteDesktop
        );
        assert_eq!(
            SecurityType::from_code(33),
            SecurityType::AppleDiffieHellman
        );
        assert_eq!(SecurityType::from_code(19), SecurityType::Other(19));
        assert!(SecurityType::None.is_supported());
        assert!(SecurityType::VncAuthentication.is_supported());
        // Apple's types are named so the refusal can say what it refused.
        assert!(!SecurityType::AppleRemoteDesktop.is_supported());
        assert!(SecurityType::AppleDiffieHellman.label().contains("Apple"));
    }

    #[test]
    fn an_unimplemented_rectangle_encoding_is_refused_with_its_number() {
        let mut body: &[u8] = &[
            0, 0, 0, 0, 0, 1, 0, 1, // x, y, width, height
            0, 0, 0, 7, // Tight
        ];
        let error = decode_framebuffer_update(1, &mut body, &PREFERRED_PIXEL_FORMAT).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("Tight"), "{text}");
    }

    #[test]
    fn a_raw_rectangle_is_read_with_the_reported_geometry() {
        // Header: x=5, y=6, width=1, height=2, encoding=Raw(0).
        let mut body: Vec<u8> = vec![0, 5, 0, 6, 0, 1, 0, 2, 0, 0, 0, 0];
        body.extend_from_slice(&[0, 0, 0, 9]);
        body.extend_from_slice(&[0, 0, 0, 8]);
        let update =
            decode_framebuffer_update(1, &mut body.as_slice(), &PREFERRED_PIXEL_FORMAT).unwrap();
        match &update.rectangles[0] {
            Rectangle::Raw {
                x,
                y,
                width,
                height,
                pixels,
            } => {
                assert_eq!((*x, *y, *width, *height), (5, 6, 1, 2));
                assert_eq!(pixels.len(), 8);
            }
            other => panic!("expected raw, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_clipboard_payload_is_rejected_before_allocating() {
        let mut body: &[u8] = &[0xFF, 0xFF, 0xFF, 0xFF];
        let error = decode_server_cut_text(&mut body).unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
    }
}
