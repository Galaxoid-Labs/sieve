//! Turning an address into something a phone camera can read.
//!
//! Rendered here, from an address this wallet already holds. No image service
//! is involved, which matters: handing an address to a QR API would tell that
//! service exactly what a block explorer lookup would.

use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib::Bytes;
use relm4::gtk::prelude::Cast;

/// Pixels per QR module. Large enough that scaling to any sensible display
/// size stays crisp rather than blurring the module edges.
const SCALE: usize = 8;
/// Quiet zone in modules. Four is the specification's minimum, and scanners
/// genuinely fail without it.
const QUIET: usize = 4;

/// Render `data` as a black-on-white texture.
///
/// Always dark on light, in both themes. Inverting a QR code for dark mode
/// looks better and scans worse — many readers will not take a light-on-dark
/// code at all — so the code keeps its own white ground and the page puts a
/// card behind it.
pub fn texture(data: &str) -> Option<gdk::Texture> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let modules = code.to_colors();
    let width = (modules.len() as f64).sqrt() as usize;
    if width == 0 || width * width != modules.len() {
        return None;
    }

    let side = (width + QUIET * 2) * SCALE;
    // Three bytes per pixel, RGB.
    let mut pixels = vec![0xFFu8; side * side * 3];

    for (index, module) in modules.iter().enumerate() {
        if !matches!(module, qrcode::Color::Dark) {
            continue;
        }
        let row = index / width;
        let column = index % width;

        for y in 0..SCALE {
            let py = (row + QUIET) * SCALE + y;
            for x in 0..SCALE {
                let px = (column + QUIET) * SCALE + x;
                let offset = (py * side + px) * 3;
                pixels[offset] = 0;
                pixels[offset + 1] = 0;
                pixels[offset + 2] = 0;
            }
        }
    }

    Some(gdk::MemoryTexture::new(
        side as i32,
        side as i32,
        gdk::MemoryFormat::R8g8b8,
        &Bytes::from_owned(pixels),
        side * 3,
    )
    .upcast())
}

/// What to encode for a receive address.
///
/// BIP-21 rather than a bare address: every wallet understands the scheme, and
/// it is what a scanner expects to find.
pub fn payment_uri(address: &str) -> String {
    format!("bitcoin:{address}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_bip21_uri() {
        assert_eq!(payment_uri("bc1qexample"), "bitcoin:bc1qexample");
    }

    #[test]
    fn a_real_address_produces_a_square_code() {
        let code = qrcode::QrCode::new(payment_uri("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")).unwrap();
        let modules = code.to_colors();
        let width = (modules.len() as f64).sqrt() as usize;
        assert_eq!(width * width, modules.len(), "a QR code is square");
        assert!(width >= 21, "version 1 is 21 modules across");
    }
}
