//! RDP connection initialisation: TPKT, X.224 and the RDP negotiation phase.
//!
//! This is everything that happens before credentials are used: the client say
//! what it can do, the server answers with what it will accept, and a failure
//! arrives as a named code. No credentials, no encryption, no session.
//!
//! Field layout comes from MS-RDPBCGR 2.2.1 (the connection sequence) and 2.2.1.1
//! (the negotiation structures), not from a third-party client, because the
//! server is what has to accept the bytes.
//!
//! Deliberately absent, and named so the absence is visible rather than implied:
//! MCS, the security exchange, licensing, capabilities, and every channel. The
//! connection sequence continues with an MCS Connect Initial, which is where an
//! implementation of RDP starts to dwarf an implementation of VNC.

use crate::rdp::RdpError;

/// X.224 connection request, the first thing a client sends.
pub const X224_CR: u8 = 0xE0;
/// X.224 connection confirm, its answer.
pub const X224_CC: u8 = 0xD0;
/// X.224 data, used for everything after the connection phase.
pub const X224_DT: u8 = 0xF0;

/// RDP negotiation request/response type.
pub const RDP_NEG_REQ: u8 = 0x01;
pub const RDP_NEG_RSP: u8 = 0x02;
pub const RDP_NEG_FAILURE: u8 = 0x03;

/// The security protocols a client can request (MS-RDPBCGR 2.2.1.1.1).
///
/// These are a bitmask. `StandardRdp` is what a modern server expects; `Tls`
/// alone fails against servers that require the standard RDP security
/// negotiation to have happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityProtocol(pub u32);

impl SecurityProtocol {
    /// Standard RDP security: the server's own encryption, no TLS.
    pub const RDP: Self = Self(0x0000_0000);
    /// TLS, with the server's certificate.
    pub const TLS: Self = Self(0x0000_0001);
    /// Credential Security Support Provider, which moves credentials inside TLS.
    pub const HYBRID: Self = Self(0x0000_0002);
    /// HYBRID plus the server's redirection guidance.
    pub const HYBRID_EX: Self = Self(0x0000_0008);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }

    /// The protocol to request: the strongest the client implements.
    ///
    /// HYBRID_EX is requested alongside HYBRID because servers choose from the
    /// set, and requesting only the newer one fails against older servers while
    /// requesting only the older one gives up redirection when it is available.
    pub fn request_default() -> Self {
        Self(Self::TLS.0 | Self::HYBRID.0 | Self::HYBRID_EX.0)
    }

    pub fn label(self) -> String {
        if self.0 == 0 {
            return "standard RDP security".to_owned();
        }
        let mut parts = Vec::new();
        if self.0 & Self::TLS.0 != 0 {
            parts.push("TLS");
        }
        if self.0 & Self::HYBRID.0 != 0 {
            parts.push("hybrid");
        }
        if self.0 & Self::HYBRID_EX.0 != 0 {
            parts.push("hybrid-ex");
        }
        if self.0 & !(Self::TLS.0 | Self::HYBRID.0 | Self::HYBRID_EX.0) != 0 {
            parts.push("unknown flags");
        }
        parts.join("+")
    }
}

/// Why a server refused the connection (MS-RDPBCGR 2.2.1.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiationFailure {
    /// The server requires a protocol this client did not offer.
    SslRequiredByServer,
    /// The server requires the client to have offered a protocol at all.
    SslNotAllowedByServer,
    WrongSecurityProtocol,
    /// A client certificate the server demanded was not supplied.
    ClientCertificateRequired,
    /// A code this client has no name for.
    Unknown(u32),
}

impl NegotiationFailure {
    pub fn from_code(code: u32) -> Self {
        match code {
            1 => Self::SslRequiredByServer,
            2 => Self::SslNotAllowedByServer,
            3 => Self::WrongSecurityProtocol,
            5 => Self::ClientCertificateRequired,
            other => Self::Unknown(other),
        }
    }

    /// What the operator should do about it.
    ///
    /// A refusal code alone does not tell anyone whether the server is
    /// misconfigured or the client offered the wrong thing, so the text says
    /// which side has the setting.
    pub fn advice(self) -> &'static str {
        match self {
            Self::SslRequiredByServer => "the server requires TLS and this client did not offer it",
            Self::SslNotAllowedByServer => {
                "the server forbids TLS, which this client always requests"
            }
            Self::WrongSecurityProtocol => "the server rejected every protocol this client offered",
            Self::ClientCertificateRequired => {
                "the server requires a client certificate, which this client has no store for"
            }
            Self::Unknown(_) => "the server refused the connection for an unnamed reason",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::SslRequiredByServer => "SSL required by server".to_owned(),
            Self::SslNotAllowedByServer => "SSL not allowed by server".to_owned(),
            Self::WrongSecurityProtocol => "wrong security protocol".to_owned(),
            Self::ClientCertificateRequired => "client certificate required".to_owned(),
            Self::Unknown(code) => format!("unknown negotiation failure {code}"),
        }
    }
}

/// What the server answered during negotiation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Negotiation {
    /// The server chose a protocol from what was offered.
    Selected(SecurityProtocol),
    /// The server supports no negotiation and will use standard RDP security.
    NoNegotiationRequired,
    /// The server refused, with a reason.
    Refused(NegotiationFailure),
}

/// The result of a successful connection phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfirmed {
    pub negotiation: Negotiation,
}

/// Wrap a payload in a TPKT header (RFC 905 / MS-RDPBCGR 2.2.1).
///
/// The length field covers the header itself, which is the detail that makes an
/// off-by-four here show up as a server that never answers.
pub fn tpkt(payload: &[u8]) -> Result<Vec<u8>, RdpError> {
    let total = payload
        .len()
        .checked_add(4)
        .ok_or_else(|| RdpError::protocol("TPKT payload length overflow"))?;
    let length = u16::try_from(total)
        .map_err(|_| RdpError::protocol(format!("TPKT payload of {total} bytes exceeds 65535")))?;
    let mut out = Vec::with_capacity(total);
    // Version 3, reserved, then the big-endian length.
    out.push(3);
    out.push(0);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Split a TPKT header off, returning the declared total length and the payload.
pub fn parse_tpkt(frame: &[u8]) -> Result<(usize, &[u8]), RdpError> {
    if frame.len() < 4 {
        return Err(RdpError::protocol(format!(
            "TPKT header needs 4 bytes, got {}",
            frame.len()
        )));
    }
    if frame[0] != 3 {
        return Err(RdpError::protocol(format!(
            "TPKT version {} is not 3",
            frame[0]
        )));
    }
    let length = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
    if length < 4 {
        return Err(RdpError::protocol(format!(
            "TPKT length {length} is shorter than its own header"
        )));
    }
    if length > frame.len() {
        return Err(RdpError::protocol(format!(
            "TPKT declares {length} bytes but only {} are present",
            frame.len()
        )));
    }
    Ok((length, &frame[4..length]))
}

/// Build the X.224 connection request with an RDP negotiation request.
///
/// `cookie` is the routing token a load balancer may need; an empty cookie
/// produces a request without a routing token.
pub fn connection_request(requested: SecurityProtocol, cookie: &str) -> Result<Vec<u8>, RdpError> {
    // X.224 CR: length indicator, type, destination reference, source reference,
    // class option. The length indicator counts every byte after itself, so it is
    // fixed up at the end.
    let mut body = Vec::new();
    body.push(0); // length indicator, patched below
    body.push(X224_CR);
    body.extend_from_slice(&0u16.to_be_bytes()); // destination reference
    body.extend_from_slice(&0u16.to_be_bytes()); // source reference
    body.push(0); // class option

    // RDP Negotiation Request: type, flags, length, then the requested protocols.
    body.push(RDP_NEG_REQ);
    body.push(0); // flags
    body.extend_from_slice(&8u16.to_le_bytes());
    body.extend_from_slice(&requested.0.to_le_bytes());

    if !cookie.is_empty() {
        if cookie.chars().any(char::is_control) {
            return Err(RdpError::protocol("cookie contains control characters"));
        }
        // The cookie is terminated with CR LF and is not length-prefixed.
        body.extend_from_slice(cookie.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    let indicator = u8::try_from(body.len() - 1)
        .map_err(|_| RdpError::protocol("X.224 connection request is too long"))?;
    body[0] = indicator;
    tpkt(&body)
}

/// The parser's state after a connection confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfirm {
    pub negotiation: Negotiation,
}

/// Parse an X.224 connection confirm, including the negotiation result.
pub fn parse_connection_confirm(payload: &[u8]) -> Result<ConnectionConfirm, RdpError> {
    if payload.len() < 7 {
        return Err(RdpError::protocol(format!(
            "connection confirm needs at least 7 bytes, got {}",
            payload.len()
        )));
    }
    if payload[1] != X224_CC {
        return Err(RdpError::protocol(format!(
            "expected an X.224 connection confirm (0x{X224_CC:02x}), got 0x{:02x}",
            payload[1]
        )));
    }
    // Sockets 2.2.1.2: after the fixed part, an optional negotiation structure.
    let rest = &payload[7..];
    if rest.is_empty() {
        return Ok(ConnectionConfirm {
            negotiation: Negotiation::NoNegotiationRequired,
        });
    }
    let kind = rest[0];
    if rest.len() < 8 {
        return Err(RdpError::protocol(format!(
            "negotiation structure is {} bytes, expected at least 8",
            rest.len()
        )));
    }
    let length = usize::from(u16::from_le_bytes([rest[2], rest[3]]));
    if length < 8 || length > rest.len() {
        return Err(RdpError::protocol(format!(
            "negotiation structure declares {length} bytes, {} are present",
            rest.len()
        )));
    }
    match kind {
        RDP_NEG_RSP => {
            let selected =
                SecurityProtocol(u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]));
            Ok(ConnectionConfirm {
                negotiation: Negotiation::Selected(selected),
            })
        }
        RDP_NEG_FAILURE => {
            let code = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
            Ok(ConnectionConfirm {
                negotiation: Negotiation::Refused(NegotiationFailure::from_code(code)),
            })
        }
        other => Err(RdpError::protocol(format!(
            "unexpected negotiation structure type {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tpkt_frame_declares_its_own_length() {
        let framed = tpkt(&[0xAA, 0xBB]).unwrap();
        assert_eq!(framed, vec![3, 0, 0, 6, 0xAA, 0xBB]);
        let (length, payload) = parse_tpkt(&framed).unwrap();
        assert_eq!(length, 6);
        assert_eq!(payload, &[0xAA, 0xBB]);
    }

    #[test]
    fn a_truncated_or_mislabelled_tpkt_frame_is_refused() {
        assert!(parse_tpkt(&[3, 0, 0]).is_err(), "short header");
        assert!(parse_tpkt(&[4, 0, 0, 4]).is_err(), "wrong version");
        assert!(
            parse_tpkt(&[3, 0, 0, 2]).is_err(),
            "length inside the header"
        );
        // Declares more than was read, which is how a short read is caught.
        assert!(parse_tpkt(&[3, 0, 0, 99, 1, 2]).is_err());
    }

    #[test]
    fn a_connection_request_carries_the_negotiation_request() {
        let request = connection_request(SecurityProtocol::request_default(), "").unwrap();
        // TPKT header, then X.224 CR with a length indicator covering the rest.
        assert_eq!(request[0], 3);
        let declared = usize::from(u16::from_be_bytes([request[2], request[3]]));
        assert_eq!(declared, request.len());
        assert_eq!(request[4], (declared - 5) as u8, "X.224 length indicator");
        assert_eq!(request[5], X224_CR);
        // The negotiation request starts at the class option's end.
        let neg = &request[11..];
        assert_eq!(neg[0], RDP_NEG_REQ);
        assert_eq!(u16::from_le_bytes([neg[2], neg[3]]), 8);
        let requested = u32::from_le_bytes([neg[4], neg[5], neg[6], neg[7]]);
        assert_eq!(requested & SecurityProtocol::TLS.0, SecurityProtocol::TLS.0);
        assert_eq!(
            requested & SecurityProtocol::HYBRID.0,
            SecurityProtocol::HYBRID.0
        );
    }

    #[test]
    fn the_default_request_offers_every_protocol_this_client_implements() {
        let requested = SecurityProtocol::request_default();
        assert!(requested.contains(SecurityProtocol::TLS));
        assert!(requested.contains(SecurityProtocol::HYBRID));
        assert!(requested.contains(SecurityProtocol::HYBRID_EX));
        // Standard RDP security is the absence of flags; it cannot be asked for
        // alongside the others, so it must not appear here.
        assert!(!requested.contains(SecurityProtocol::RDP));
        assert!(requested.label().contains("TLS"));
        assert!(requested.label().contains("hybrid"));
    }

    /// The negotiation request is pinned as literal bytes, built from the
    /// structure's field order rather than from this encoder, so a wrong field
    /// order or a wrong endianness fails here even though the parser would
    /// happily read whatever the encoder produced.
    #[test]
    fn the_negotiation_request_is_laid_out_field_by_field() {
        let request = connection_request(SecurityProtocol::HYBRID, "").unwrap();
        let negotiation = &request[11..];
        let expected: Vec<u8> = vec![
            0x01, // type: RDP_NEG_REQ
            0x00, // flags
            0x08, 0x00, // length: 8, little-endian
            0x02, 0x00, 0x00, 0x00, // requested protocols: HYBRID, little-endian
        ];
        assert_eq!(negotiation, &expected[..], "negotiation request layout");
        // The RDP layer's own numbers are little-endian, unlike TPKT's length
        // and the X.224 fields, which are big-endian. Mixing them up is the
        // classic way this phase fails silently.
        assert_eq!(
            u16::from_be_bytes([request[2], request[3]]),
            request.len() as u16,
            "TPKT length is big-endian"
        );
    }

    #[test]
    fn a_cookie_is_terminated_with_crlf_and_control_characters_are_refused() {
        let with_cookie =
            connection_request(SecurityProtocol::request_default(), "token123").unwrap();
        let text = String::from_utf8_lossy(&with_cookie);
        assert!(text.ends_with("token123\r\n"), "{text:?}");
        // A cookie with a newline would let the peer decide where it ends.
        assert!(connection_request(SecurityProtocol::request_default(), "a\nb").is_err());
    }

    #[test]
    fn a_selected_protocol_is_read_back() {
        // X.224 CC fixed part, then an RDP_NEG_RSP selecting hybrid.
        let mut payload = vec![0x13, X224_CC, 0, 0, 0, 0, 0];
        payload.push(RDP_NEG_RSP);
        payload.push(0);
        payload.extend_from_slice(&8u16.to_le_bytes());
        payload.extend_from_slice(&SecurityProtocol::HYBRID.0.to_le_bytes());
        let confirm = parse_connection_confirm(&payload).unwrap();
        assert_eq!(
            confirm.negotiation,
            Negotiation::Selected(SecurityProtocol::HYBRID)
        );
    }

    #[test]
    fn an_absent_negotiation_structure_means_standard_rdp_security() {
        let payload = vec![0x13, X224_CC, 0, 0, 0, 0, 0];
        let confirm = parse_connection_confirm(&payload).unwrap();
        assert_eq!(confirm.negotiation, Negotiation::NoNegotiationRequired);
    }

    #[test]
    fn a_refusal_names_the_server_requirement_and_what_the_client_did() {
        let mut payload = vec![0x13, X224_CC, 0, 0, 0, 0, 0];
        payload.push(RDP_NEG_FAILURE);
        payload.push(0);
        payload.extend_from_slice(&8u16.to_le_bytes());
        // 3 = wrong security protocol.
        payload.extend_from_slice(&3u32.to_le_bytes());
        let confirm = parse_connection_confirm(&payload).unwrap();
        match confirm.negotiation {
            Negotiation::Refused(failure) => {
                assert_eq!(failure, NegotiationFailure::WrongSecurityProtocol);
                assert!(failure.advice().contains("rejected every protocol"));
                assert_eq!(failure.label(), "wrong security protocol");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_named_failure_codes_are_the_ones_the_protocol_defines() {
        assert_eq!(
            NegotiationFailure::from_code(1),
            NegotiationFailure::SslRequiredByServer
        );
        assert_eq!(
            NegotiationFailure::from_code(2),
            NegotiationFailure::SslNotAllowedByServer
        );
        assert_eq!(
            NegotiationFailure::from_code(5),
            NegotiationFailure::ClientCertificateRequired
        );
        assert_eq!(
            NegotiationFailure::from_code(99),
            NegotiationFailure::Unknown(99)
        );
        // Every code must be actionable, including the unnamed one.
        for code in 1..=6 {
            assert!(!NegotiationFailure::from_code(code).advice().is_empty());
        }
    }

    #[test]
    fn a_response_that_is_not_a_connection_confirm_is_refused() {
        let payload = vec![0x13, X224_CR, 0, 0, 0, 0, 0];
        let error = parse_connection_confirm(&payload).unwrap_err();
        assert!(error.to_string().contains("connection confirm"), "{error}");
    }

    #[test]
    fn a_negotiation_structure_longer_than_the_frame_is_refused() {
        // The length field is read before the body, so a server that claims more
        // than it sent must be caught rather than read past.
        let mut payload = vec![0x13, X224_CC, 0, 0, 0, 0, 0];
        payload.push(RDP_NEG_RSP);
        payload.push(0);
        payload.extend_from_slice(&64u16.to_le_bytes()); // declares 64, 8 present
        payload.extend_from_slice(&SecurityProtocol::TLS.0.to_le_bytes());
        let error = parse_connection_confirm(&payload).unwrap_err();
        assert!(error.to_string().contains("declares 64"), "{error}");
    }

    #[test]
    fn a_negotiation_structure_shorter_than_its_own_header_is_refused() {
        let mut payload = vec![0x13, X224_CC, 0, 0, 0, 0, 0];
        payload.push(RDP_NEG_RSP);
        payload.push(0);
        payload.extend_from_slice(&4u16.to_le_bytes()); // a 4-byte structure cannot hold a code
        payload.extend_from_slice(&SecurityProtocol::TLS.0.to_le_bytes());
        let error = parse_connection_confirm(&payload).unwrap_err();
        assert!(error.to_string().contains("declares 4"), "{error}");
    }
}
