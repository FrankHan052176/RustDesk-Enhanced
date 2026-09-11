//! RDP support for the controlling side.
//!
//! Scope, stated the same way the VNC module states it: this is a **client**.
//! The engine's controlled side speaks the RustDesk protocol only, so RDP exists
//! here to connect out to a Windows host that offers it.
//!
//! What is implemented: TPKT framing, the X.224 connection phase, and RDP
//! security negotiation, which is everything up to the point where credentials
//! matter and a refusal still arrives as a named code. See [`connection`].
//!
//! What is not, and why it matters when reading this module: RDP's connection
//! sequence continues with an MCS Connect Initial carrying a GCC conference
//! create request, then the security exchange, licensing, capabilities and the
//! channel join. That is a substantially larger body of protocol than RFB, and
//! none of it is here yet. A session cannot be established with only this
//! module, and it deliberately does not pretend otherwise: nothing in this tree
//! opens an RDP session, so a `rdp://` target is refused rather than half-served.

pub mod connection;

/// Everything that can go wrong while talking to an RDP server.
///
/// Cloneable for the same reason [`crate::vnc::VncError`] is: `ViewerError` is,
/// and both protocols report through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdpError {
    /// The socket failed.
    Io {
        what: &'static str,
        kind: std::io::ErrorKind,
        message: String,
    },
    /// The server sent something this client cannot use. The text names what.
    Protocol(String),
    /// The server refused the connection during negotiation.
    NegotiationRefused(connection::NegotiationFailure),
    /// A part of RDP this client does not implement yet.
    NotImplemented(&'static str),
}

impl RdpError {
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub fn io(what: &'static str, source: std::io::Error) -> Self {
        Self::Io {
            what,
            kind: source.kind(),
            message: source.to_string(),
        }
    }
}

impl std::fmt::Display for RdpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io {
                what,
                kind,
                message,
            } => {
                write!(formatter, "rdp {what} ({kind:?}): {message}")
            }
            Self::Protocol(text) => write!(formatter, "rdp protocol: {text}"),
            Self::NegotiationRefused(failure) => write!(
                formatter,
                "rdp negotiation refused: {} -- {}",
                failure.label(),
                failure.advice()
            ),
            Self::NotImplemented(part) => {
                write!(formatter, "rdp {part} is not implemented in this client")
            }
        }
    }
}

impl std::error::Error for RdpError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unimplemented_part_says_which_part() {
        let error = RdpError::NotImplemented("MCS connection");
        let text = error.to_string();
        assert!(text.contains("MCS connection"), "{text}");
        assert!(text.contains("not implemented"), "{text}");
    }

    #[test]
    fn a_negotiation_refusal_keeps_both_the_code_and_the_advice() {
        let error =
            RdpError::NegotiationRefused(connection::NegotiationFailure::SslRequiredByServer);
        let text = error.to_string();
        assert!(text.contains("SSL required by server"), "{text}");
        // The advice is what makes the failure actionable; losing it would leave
        // a bare code.
        assert!(text.contains("requires TLS"), "{text}");
    }
}
