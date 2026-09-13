//! Automatic encoder bitrate selection.
//!
//! The app offers a refresh rate and no picture-quality setting, so the encoder
//! has to pick a rate budget itself. This module turns the session's shape --
//! resolution, capture rate, codec -- into a bitrate ceiling.
//!
//! Scope, stated plainly: this is not congestion control. It derives from what
//! the session asks the encoder to produce, and the encoder then holds that
//! ceiling for its lifetime, because the native NVENC wrapper fixes its rate at
//! construction and exposes no reconfiguration path. Changing the ceiling while
//! a session runs needs the native encoder layer, not this arithmetic. An
//! operator who knows the path better can still pin a value with `--bitrate`.
//!
//! The coefficients are bits per pixel per frame. They are chosen so the
//! familiar configurations land near the rates they are usually given: 1080p60
//! H.264 near 10 Mbit/s, 1440p60 H.265 near 16 Mbit/s, 1440p120 H.265 near 32
//! Mbit/s. They are telemetry-neutral -- they do not measure the link.

/// Ceiling on any automatic choice, so a huge display cannot ask an encoder for
/// a rate no residential path will carry.
pub const MAX_AUTO_BITRATE: i64 = 100_000_000;

/// Floor, so a tiny display still gets a usable picture.
pub const MIN_AUTO_BITRATE: i64 = 1_000_000;

/// Which encoder the rate is being chosen for. H.265 reaches the same visual
/// quality at roughly two thirds of H.264's rate, and VP9/AV1 are not produced
/// by the controlled host yet, so they fall in with H.264 rather than being
/// given an invented discount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateCodec {
    H264,
    H265,
}

impl BitrateCodec {
    /// Bits per pixel per frame for this codec.
    fn bits_per_pixel(self) -> f64 {
        match self {
            Self::H264 => 0.1,
            Self::H265 => 0.067,
        }
    }
}

/// A bitrate ceiling for one session shape, or `None` when the shape cannot
/// describe any real encoder budget.
///
/// A zero or negative dimension or rate is not a small session, it is a
/// malformed one, and returning a number for it would hide that from the
/// caller.
pub fn auto_bitrate_bps(width: i32, height: i32, fps: u32, codec: BitrateCodec) -> Option<i64> {
    if width <= 0 || height <= 0 || fps == 0 {
        return None;
    }
    let pixels = (width as f64) * (height as f64);
    let raw = pixels * (fps as f64) * codec.bits_per_pixel();
    if !raw.is_finite() {
        return None;
    }
    // Stay inside `i64` before any clamp; the product above cannot overflow at
    // 8K240, but a caller is free to pass larger numbers.
    let clamped = raw
        .min(MAX_AUTO_BITRATE as f64)
        .max(MIN_AUTO_BITRATE as f64);
    Some(clamped.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The coefficients are not measured, so the test pins the property that
    /// matters instead of an exact number: each familiar shape has to land
    /// inside the band that shape is normally encoded at.
    #[test]
    fn known_shapes_land_inside_their_usable_band() {
        let cases = [
            (1920, 1080, 60, BitrateCodec::H264, 6_000_000, 20_000_000),
            (2560, 1440, 60, BitrateCodec::H265, 8_000_000, 25_000_000),
            (2560, 1440, 120, BitrateCodec::H265, 16_000_000, 50_000_000),
            (
                3840,
                2160,
                60,
                BitrateCodec::H265,
                20_000_000,
                MAX_AUTO_BITRATE,
            ),
        ];
        for (width, height, fps, codec, low, high) in cases {
            let actual = auto_bitrate_bps(width, height, fps, codec).unwrap();
            assert!(
                (low..=high).contains(&actual),
                "{width}x{height}@{fps} {codec:?} produced {actual}, outside {low}..={high}"
            );
        }
    }

    #[test]
    fn a_malformed_shape_is_refused_rather_than_guessed() {
        assert_eq!(auto_bitrate_bps(0, 1080, 60, BitrateCodec::H264), None);
        assert_eq!(auto_bitrate_bps(1920, -1, 60, BitrateCodec::H264), None);
        assert_eq!(auto_bitrate_bps(1920, 1080, 0, BitrateCodec::H264), None);
    }

    #[test]
    fn the_ceiling_and_floor_hold() {
        assert_eq!(
            auto_bitrate_bps(16_384, 16_384, 240, BitrateCodec::H265),
            Some(MAX_AUTO_BITRATE)
        );
        assert_eq!(
            auto_bitrate_bps(16, 16, 1, BitrateCodec::H265),
            Some(MIN_AUTO_BITRATE)
        );
    }

    #[test]
    fn h265_spends_less_than_h264_for_the_same_shape() {
        let h264 = auto_bitrate_bps(2560, 1440, 60, BitrateCodec::H264).unwrap();
        let h265 = auto_bitrate_bps(2560, 1440, 60, BitrateCodec::H265).unwrap();
        assert!(h265 < h264);
    }

    #[test]
    fn the_rate_scales_with_the_shape() {
        let base = auto_bitrate_bps(2560, 1440, 60, BitrateCodec::H265).unwrap();
        assert_eq!(
            auto_bitrate_bps(2560, 1440, 120, BitrateCodec::H265),
            Some(base * 2)
        );
        assert!(auto_bitrate_bps(2560, 1440, 30, BitrateCodec::H265).unwrap() < base);
    }
}
