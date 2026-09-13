//! Probe a real RDP server's connection phase, without a GUI.
//!
//! Run with `cargo run --example rdp_probe -- <host> [port]`.
//!
//! The unit tests drive scripted frames: they prove the client matches the
//! X.224 layout this project documented, not that it matches a server someone
//! runs. This probe performs the real exchange -- TPKT framing, the connection
//! request with the protocols we would accept, and the server's confirm -- and
//! reports what it negotiated. Credentials are deliberately not part of this
//! probe: a username and password belong to the operator, and the connection
//! phase is what a client can verify without them.

use librustdesk::rdp::connection::{
    Negotiation, SecurityProtocol, connection_request, parse_connection_confirm, parse_tpkt,
};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3389);
    println!("connecting to {host}:{port}");

    let mut stream = TcpStream::connect((host.as_str(), port))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    // Ask for what a client that cannot do CredSSP yet would ask for, so the
    // answer says whether this server insists on NLA.
    let requested = SecurityProtocol::request_default();
    let request = connection_request(requested, "")?;
    stream.write_all(&request)?;
    stream.flush()?;

    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let declared = u16::from_be_bytes([header[2], header[3]]) as usize;
    if declared < 7 || declared > 4096 {
        return Err(format!("implausible TPKT length {declared}").into());
    }
    let mut payload = vec![0u8; declared - 4];
    stream.read_exact(&mut payload)?;
    let mut frame = header.to_vec();
    frame.extend_from_slice(&payload);
    let (total, body) = parse_tpkt(&frame)?;
    if total != declared {
        return Err(format!("TPKT length {total} disagrees with the header {declared}").into());
    }
    let confirmed = parse_connection_confirm(body)?;
    match &confirmed.negotiation {
        Negotiation::Selected(protocol) => {
            println!("negotiated={}", protocol.label());
        }
        Negotiation::NoNegotiationRequired => {
            println!("negotiated=standard RDP security (no negotiation)");
        }
        Negotiation::Refused(failure) => {
            println!("refused={}", failure.label());
            println!("advice={}", failure.advice());
        }
    }
    println!("RESULT=CONNECTION_PHASE_OK");
    Ok(())
}
