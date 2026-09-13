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
use librustdesk::vnc::live::{RGBA_BYTES_PER_PIXEL, VncLiveSession};

fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5900);
    let password = args.next();
    // `--live` additionally drives the session: request a repaint, wait for the
    // framebuffer and report what arrived. That is the part the scripted unit
    // tests cannot exercise, because they have no real server to paint it.
    let live = std::env::args().any(|value| value == "--live");

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
            drop(session);

            if live {
                let live_session = VncLiveSession::open(&host, port, password.as_deref(), true);
                match live_session {
                    Ok(session) => {
                        println!("live_server={:?}", session.server_name());
                        println!("live_security={}", session.security_label());
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                        let mut frame = None;
                        while std::time::Instant::now() < deadline {
                            if let Some(taken) = session.take_frame() {
                                frame = Some(taken);
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        match frame {
                            Some((width, height, pixels)) => {
                                let expected =
                                    (width as usize) * (height as usize) * RGBA_BYTES_PER_PIXEL;
                                println!("live_frame={width}x{height} bytes={}", pixels.len());
                                assert_eq!(
                                    pixels.len(),
                                    expected,
                                    "frame size does not match geometry"
                                );
                                // A picture that is entirely one colour is what a
                                // blank or all-black capture looks like, and it
                                // would also be what a mis-decoded frame looks
                                // like, so report the distinct colours seen.
                                let mut distinct = std::collections::HashSet::new();
                                for pixel in pixels.chunks_exact(RGBA_BYTES_PER_PIXEL).take(4096) {
                                    distinct.insert([pixel[0], pixel[1], pixel[2]]);
                                }
                                println!("live_frame_distinct_colours_sampled={}", distinct.len());
                                println!("RESULT=LIVE_FRAME_OK");
                            }
                            None => {
                                let snapshot = session.snapshot();
                                println!("live_snapshot={snapshot:?}");
                                println!("RESULT=LIVE_FRAME_TIMEOUT");
                            }
                        }
                    }
                    Err(error) => {
                        println!("live_error={error}");
                        println!("RESULT=LIVE_FAILED");
                    }
                }
            }
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
