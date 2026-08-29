//! Turning an address into something a phone camera can read.
//!
//! Rendered here, from an address this wallet already holds. No image service
//! is involved, which matters: handing an address to a QR API would tell that
//! service exactly what a block explorer lookup would.

use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib::Bytes;
use relm4::gtk::prelude::{Cast, TextureExt};

/// Every code is rendered onto a canvas of this side, whatever version it is.
///
/// A longer address needs more modules, and sizing the image to the modules
/// makes the widget's natural size change with the address — which moves the
/// layout every time the address type changes. A fixed canvas with the code
/// centred in it keeps the picture identical and lets only the density vary.
const CANVAS: usize = 512;
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

    // Whole pixels per module, so edges stay sharp; whatever is left over
    // becomes a wider quiet zone rather than a fractional module.
    let total = width + QUIET * 2;
    let scale = (CANVAS / total).max(1);
    let drawn = total * scale;
    let margin = (CANVAS - drawn) / 2;

    // Three bytes per pixel, RGB.
    let mut pixels = vec![0xFFu8; CANVAS * CANVAS * 3];

    for (index, module) in modules.iter().enumerate() {
        if !matches!(module, qrcode::Color::Dark) {
            continue;
        }
        let row = index / width;
        let column = index % width;

        for y in 0..scale {
            let py = margin + (row + QUIET) * scale + y;
            for x in 0..scale {
                let px = margin + (column + QUIET) * scale + x;
                let offset = (py * CANVAS + px) * 3;
                pixels[offset] = 0;
                pixels[offset + 1] = 0;
                pixels[offset + 2] = 0;
            }
        }
    }

    Some(gdk::MemoryTexture::new(
        CANVAS as i32,
        CANVAS as i32,
        gdk::MemoryFormat::R8g8b8,
        &Bytes::from_owned(pixels),
        CANVAS * 3,
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
    fn every_address_type_renders_the_same_size() {
        // The whole point: a taproot address needs a denser code than a legacy
        // one, and if the texture grew with it the layout moved every time the
        // address type changed.
        let addresses = [
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2",
            "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
            "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
            "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297",
        ];

        let sizes: Vec<(i32, i32)> = addresses
            .iter()
            .filter_map(|a| texture(&payment_uri(a)))
            .map(|t| (t.width(), t.height()))
            .collect();

        assert_eq!(sizes.len(), addresses.len(), "every address must encode");
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "textures differ in size: {sizes:?}"
        );
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
