//! Video presentation plumbing for the Flutter bridge.
//!
//! The Flutter frontend renders remote frames through a platform texture rather
//! than the OHOS native Surface, and it requests a texture key per session
//! before starting one. Frame delivery is not wired yet: the lease below
//! deliberately hands the decoder no usable surface so the failure is explicit
//! instead of a silently black texture.

use crate::{rendezvous::RendezvousConfig, viewer::SurfaceLease};
use hbb_common::{
    base64::{Engine as _, engine::general_purpose::STANDARD},
    config::{Config, RS_PUB_KEY, keys::OPTION_KEY},
    sodiumoxide::crypto::sign,
};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicI32, Ordering},
};
use uuid::Uuid;

/// Sentinel meaning "no surface". It is never a valid platform surface id, so a
/// decode attempt fails loudly instead of rendering into an unrelated target.
const NO_SURFACE: u64 = 0;

/// Texture keys the frontend requests per session. They are monotonic so a
/// restarted session never reuses a key the previous one still holds.
static NEXT_TEXTURE_KEY: AtomicI32 = AtomicI32::new(1);

fn leases() -> &'static Mutex<std::collections::HashMap<Uuid, Arc<FlutterTextureLease>>> {
    static LEASES: OnceLock<Mutex<std::collections::HashMap<Uuid, Arc<FlutterTextureLease>>>> =
        OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Next texture key for the frontend to register with its texture registry.
pub fn next_texture_key() -> i32 {
    NEXT_TEXTURE_KEY.fetch_add(1, Ordering::Relaxed)
}

/// A lease that reports the sentinel surface, making the missing Flutter
/// rendering path a hard, observable failure rather than a black frame.
struct FlutterTextureLease {
    texture_key: i32,
}

// Safety: the id is stable for the lifetime of the process-owned lease, which
// is the invariant the trait requires. It intentionally does not name a live
// platform surface, so the decoder refuses to open.
unsafe impl SurfaceLease for FlutterTextureLease {
    fn surface_id(&self) -> u64 {
        NO_SURFACE
    }
}

/// Lease for one session. The registry keeps it alive for the session's
/// lifetime, which is what the trait's longevity requirement needs.
pub fn lease_for(id: Uuid) -> Arc<dyn SurfaceLease> {
    let mut registry = leases().lock().unwrap_or_else(|error| error.into_inner());
    registry
        .entry(id)
        .or_insert_with(|| {
            Arc::new(FlutterTextureLease {
                texture_key: next_texture_key(),
            })
        })
        .clone()
}

pub fn release_lease(id: Uuid) {
    leases()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&id);
}

/// Rendezvous configuration for a peer ID, resolved from the same configuration
/// the rest of the runtime uses. A custom key that fails to decode is an error
/// rather than a silent fallback to the public key, because that would trust the
/// wrong server.
pub fn rendezvous_config(id: String) -> Result<RendezvousConfig, String> {
    let configured_key = Config::get_option(OPTION_KEY);
    let (key_base64, licence_key) = if configured_key.is_empty() {
        (RS_PUB_KEY, RS_PUB_KEY.to_owned())
    } else {
        (configured_key.as_str(), configured_key.clone())
    };
    let bytes = STANDARD
        .decode(key_base64.as_bytes())
        .map_err(|_| "Configured rendezvous trust anchor is invalid".to_owned())?;
    let server_key = sign::PublicKey::from_slice(&bytes)
        .ok_or_else(|| "Configured rendezvous trust anchor is invalid".to_owned())?;
    Ok(RendezvousConfig {
        id,
        rendezvous_server: Config::get_rendezvous_server(),
        server_key,
        licence_key,
        relay_server: None,
        connect_timeout: std::time::Duration::from_secs(45),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_keys_are_unique_per_request() {
        let first = next_texture_key();
        let second = next_texture_key();
        assert_ne!(first, second);
        assert!(second > first);
    }

    #[test]
    fn one_session_keeps_one_lease_across_repeated_start_attempts() {
        let id = Uuid::new_v4();
        let first = lease_for(id);
        let second = lease_for(id);
        assert!(
            Arc::ptr_eq(&first, &second),
            "a session must not churn leases"
        );
        release_lease(id);
        let third = lease_for(id);
        assert!(!Arc::ptr_eq(&first, &third));
        release_lease(id);
    }

    #[test]
    fn the_lease_reports_no_real_surface_so_the_missing_path_is_visible() {
        let id = Uuid::new_v4();
        let lease = lease_for(id);
        // Safety: reading the id is the trait's only requirement.
        assert_eq!(lease.surface_id(), NO_SURFACE);
        release_lease(id);
    }
}
