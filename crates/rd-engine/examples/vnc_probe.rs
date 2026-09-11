//! Probe a real VNC server with the client in `vnc`, without a GUI.
//!
//! Run with `cargo run --example vnc_probe -- <host> <port> [password]`.
//!
//! This exists because the unit tests use scripted servers: they prove the
//! client matches RFC 6143, not that it matches a server someone actually runs.
//! The probe completes the handshake against a live endpoint and reports what it
//! negotiated, which is the cheapest way to find out that a real server does
//! something the RFC allows but a scripted one never does.

use librustdesk::vnc::VncSession;

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5900);
    let password = args.next();

    println!("connecting to {host}:{port}");
    let result = match password.as_deref() {
        Some(secret) => VncSession::connect_with_password(&host, port, secret),
        None => VncSession::connect_none(&host, port),
    };

    match result {
        Ok(session) => {
            let info = session.info();
            println!("server_version={}", info.version);
            println!("negotiated_version={}", info.negotiated_version);
            println!("security={}", info.security.label());
            println!("framebuffer={}x{}", info.width, info.height);
            println!("name={:?}", info.name);
            println!(
                "pixel_format=bpp{} depth{} true_colour={} rgb_max={}/{}/{}",
                info.format.bits_per_pixel,
                info.format.depth,
                info.format.true_colour,
                info.format.red_max,
                info.format.green_max,
                info.format.blue_max
            );
            println!("encodings={:?}", session.encodings());
            println!("RESULT=HANDSHAKE_OK");
        }
        Err(error) => {
            // A refusal is a result too: it names what the server offered and
            // what this client can do about it.
            println!("error={error}");
            println!("RESULT=HANDSHAKE_FAILED");
            std::process::exit(1);
        }
    }
}
