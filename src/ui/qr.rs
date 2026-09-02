//! Turning an address into something a phone camera can read.
//!
//! Rendered here, from an address this wallet already holds. No image service
//! is involved, which matters: handing an address to a QR API would tell that
//! service exactly what a block explorer lookup would.

use relm4::gtk::gdk;
use relm4::gtk::glib::Bytes;
use relm4::gtk::prelude::Cast;
use relm4::gtk::prelude::{TextureExt, TextureExtManual};

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

/// The ₿ of the Bitcoin mark, white on nothing, rasterised ahead of time.
///
/// **A PNG rather than the SVG beside it, and that is not a preference.** GTK
/// decodes PNG itself; SVG it hands to a gdk-pixbuf loader, which is librsvg's
/// and is absent on plenty of machines — this one included, which is how it was
/// found. `Texture::from_bytes` returned `None`, the code drew a white square
/// where the mark should be, and nothing said why. A format GTK decodes on its
/// own cannot fail that way.
///
/// Only the glyph, because only the glyph has a fixed colour. The circle under
/// it changes with the chain and is drawn in `stamp`, so a colour stays an
/// argument rather than becoming three more files. Rendered from
/// `bitcoin-logo.svg` — public domain, vendored, kept beside this as the source
/// it came from — in the same viewBox, so it lands where it belongs over a
/// circle filling the square:
///
/// ```sh
/// rsvg-convert -w 256 -h 256 -o data/icons/hicolor/scalable/apps/bitcoin-mark.png <glyph only>
/// ```
const GLYPH: &[u8] = include_bytes!("../../data/icons/hicolor/scalable/apps/bitcoin-mark.png");

/// How much of the code's width the mark takes.
///
/// A fifth. The centre of a QR code carries no finder or timing pattern, so
/// what a mark covers is only data — and data is what the error correction is
/// for. A fifth of the width is a twenty-fifth of the area, comfortably inside
/// the thirty per cent that level H recovers, and it is the proportion payment
/// apps settled on.
const LOGO_SHARE: usize = 5;

/// The white stroke's width, as a share of the circle's radius.
///
/// A third, which is thick enough to read as a ring rather than as a gap. It
/// is not free: the stroke erases modules exactly as the mark does, so what
/// the error correction has to recover is the *whole* disc out to the outer
/// edge — see `the_mark_covers_a_small_share_of_the_code`, which is written
/// against that outer radius rather than against the circle.
const STROKE_SHARE: f64 = 3.0;

/// The circle's colour, by the chain the address belongs to.
///
/// **This is the one place a hardcoded colour is right**, and it is worth
/// saying why, because the rule everywhere else is the opposite. A QR code
/// needs dark modules on a light ground to scan, so this surface is white in
/// both themes whatever the desktop is doing — which means a colour drawn on
/// it has a *known* background and can be chosen for contrast once. That is
/// exactly what the balance card's mark could not do, sitting on a card that
/// is light or dark depending on the hour.
///
/// Bitcoin's own orange, and mempool.space's purple and green for the test
/// chains, so the association is borrowed from where people already look.
pub fn brand(network: &str) -> &'static str {
    match network {
        "signet" => "#6f1d5d",
        "testnet" | "testnet4" => "#0aab2f",
        // Including an unrecognised one: this is the Bitcoin mark, and orange
        // is what it is.
        _ => "#f7931a",
    }
}

/// Render `data` as a QR code with the Bitcoin mark in the middle.
///
/// Always dark on light, in both themes. Inverting a QR code for dark mode
/// looks better and scans worse — many readers will not take a light-on-dark
/// code at all — so the code keeps its own white ground and the page puts a
/// card behind it.
///
/// **Error correction is level H because of the mark.** The default is M,
/// which recovers fifteen per cent; covering the centre of a code is
/// contiguous damage rather than scattered noise, and Reed-Solomon handles
/// contiguous damage worst. H recovers thirty. It costs a denser code for the
/// same address, which is free here: the canvas is a fixed size and only the
/// module count changes.
pub fn texture(data: &str, network: &str) -> Option<gdk::Texture> {
    let code =
        qrcode::QrCode::with_error_correction_level(data.as_bytes(), qrcode::EcLevel::H).ok()?;
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
    // Whatever does not divide evenly is centred, so the code sits square in
    // the canvas rather than up against one edge.
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

    stamp(&mut pixels, brand(network), margin, drawn);

    Some(
        gdk::MemoryTexture::new(
            CANVAS as i32,
            CANVAS as i32,
            gdk::MemoryFormat::R8g8b8,
            &Bytes::from_owned(pixels),
            CANVAS * 3,
        )
        .upcast(),
    )
}

/// `#rrggbb` as three bytes.
fn rgb(colour: &str) -> Option<(u8, u8, u8)> {
    let hex = colour.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some((
        u8::from_str_radix(&hex[0..2], 16).ok()?,
        u8::from_str_radix(&hex[2..4], 16).ok()?,
        u8::from_str_radix(&hex[4..6], 16).ok()?,
    ))
}

/// Put the mark in the middle of a rendered code.
///
/// The circle is drawn here and the glyph is composited over it, so the colour
/// is an argument rather than three files.
///
/// Silent about failure on purpose: a receive screen with a plain QR code
/// works, and one with no QR code at all does not. Nothing about the
/// decoration is worth that trade.
fn stamp(pixels: &mut [u8], colour: &str, margin: usize, drawn: usize) {
    let side = drawn / LOGO_SHARE;
    if side == 0 {
        return;
    }
    let Some((red, green, blue)) = rgb(colour) else {
        return;
    };

    let radius = side as f64 / 2.0;
    // Clear space around the circle, and it has to be a *circle* too. A square
    // of white behind a round mark leaves four corners of nothing showing
    // against the pattern, which reads as a sticker somebody slapped on rather
    // than as part of the code.
    let ring = (radius / STROKE_SHARE).max(2.0);
    let centre = margin as f64 + drawn as f64 / 2.0;

    // One pass, three bands: white out to the ring, the colour inside the
    // circle, and a feathered pixel at each boundary so neither edge is
    // stepped beside a grid of perfect squares.
    let outer = radius + ring;
    let from = (centre - outer).floor().max(0.0) as usize;
    let to = ((centre + outer).ceil() as usize).min(CANVAS);
    for y in from..to {
        for x in from..to {
            let (dx, dy) = (x as f64 + 0.5 - centre, y as f64 + 0.5 - centre);
            let distance = (dx * dx + dy * dy).sqrt();

            let white = (outer - distance + 0.5).clamp(0.0, 1.0);
            if white <= 0.0 {
                continue;
            }
            let ink = (radius - distance + 0.5).clamp(0.0, 1.0);

            let offset = (y * CANVAS + x) * 3;
            for (channel, value) in [red, green, blue].into_iter().enumerate() {
                let ground = f64::from(pixels[offset + channel]);
                // White first — that is the stroke — then the colour over it.
                let cleared = 255.0 * white + ground * (1.0 - white);
                pixels[offset + channel] =
                    (f64::from(value) * ink + cleared * (1.0 - ink)).round() as u8;
            }
        }
    }

    let Ok(texture) = gdk::Texture::from_bytes(&Bytes::from_static(GLYPH)) else {
        return;
    };
    let (source_width, source_height) = (texture.width() as usize, texture.height() as usize);
    if source_width == 0 || source_height == 0 {
        return;
    }
    // `download` always hands back premultiplied BGRA, whatever the texture
    // was made from.
    let mut glyph = vec![0u8; source_width * source_height * 4];
    texture.download(&mut glyph, source_width * 4);

    // Scaled to the circle, not drawn at whatever size it happens to be. The
    // glyph is rasterised once at 256 and the circle is a fifth of the code, so
    // this is always a reduction — and a reduction by sampling one pixel leaves
    // a thin mark looking broken. Averaging the source square each destination
    // pixel covers is what keeps the strokes even.
    let origin = centre - radius;
    for row in 0..side {
        let y = (origin.round() as usize).saturating_add(row);
        if y >= CANVAS {
            break;
        }
        let sy = (row * source_height) / side;
        let sy_end = (((row + 1) * source_height) / side)
            .max(sy + 1)
            .min(source_height);
        for column in 0..side {
            let x = (origin.round() as usize).saturating_add(column);
            if x >= CANVAS {
                break;
            }
            let sx = (column * source_width) / side;
            let sx_end = (((column + 1) * source_width) / side)
                .max(sx + 1)
                .min(source_width);

            let (mut b, mut g, mut r, mut a, mut count) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for py in sy..sy_end {
                for px in sx..sx_end {
                    let at = (py * source_width + px) * 4;
                    b += u32::from(glyph[at]);
                    g += u32::from(glyph[at + 1]);
                    r += u32::from(glyph[at + 2]);
                    a += u32::from(glyph[at + 3]);
                    count += 1;
                }
            }
            if count == 0 || a == 0 {
                continue;
            }
            let (b, g, r, a) = (b / count, g / count, r / count, a / count);

            let offset = (y * CANVAS + x) * 3;
            // Premultiplied, so the source is already scaled by its own alpha
            // and the ground contributes what is left.
            let over = |src: u32, dst: u8| -> u8 {
                (src + u32::from(dst) * (255 - a) / 255).min(255) as u8
            };
            pixels[offset] = over(r, pixels[offset]);
            pixels[offset + 1] = over(g, pixels[offset + 1]);
            pixels[offset + 2] = over(b, pixels[offset + 2]);
        }
    }
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
    // Only the tests measure a texture; a normal build has no use for this,
    // and importing it at the top made it look unused.
    use relm4::gtk::prelude::TextureExt;

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
            .filter_map(|a| texture(&payment_uri(a), "bitcoin"))
            .map(|t| (t.width(), t.height()))
            .collect();

        assert_eq!(sizes.len(), addresses.len(), "every address must encode");
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "textures differ in size: {sizes:?}"
        );
    }

    /// Each chain gets its own circle, and an unknown one is still Bitcoin.
    #[test]
    fn the_mark_is_coloured_by_chain() {
        assert_eq!(brand("bitcoin"), "#f7931a");
        assert_eq!(brand("signet"), "#6f1d5d");
        assert_eq!(brand("testnet4"), "#0aab2f");
        assert_eq!(brand("testnet"), "#0aab2f");
        // Not a reason to draw no logo: this is the Bitcoin mark.
        assert_eq!(brand(""), "#f7931a");
        assert_eq!(brand("something-new"), "#f7931a");

        // Every colour is a full six-digit hex, because it is substituted
        // straight into an SVG fill and a malformed one is a mark that either
        // vanishes or draws black over the middle of the code.
        for network in ["bitcoin", "signet", "testnet4", ""] {
            let colour = brand(network);
            assert_eq!(colour.len(), 7, "{colour}");
            assert!(colour.starts_with('#'));
            assert!(
                colour[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{colour}"
            );
        }
    }

    /// The mark must stay small enough for the error correction to cover it.
    ///
    /// Level H recovers thirty per cent of a code. What has to be recovered is
    /// the whole white disc — **the stroke counts, because it erases modules
    /// exactly as the circle does**, and measuring only the coloured part is
    /// how a stroke gets thickened until the code stops scanning with nothing
    /// to say it has.
    ///
    /// The bound here is well under thirty, deliberately. Damage in the middle
    /// of a code is contiguous rather than scattered, which is the shape
    /// Reed-Solomon handles worst, and a scanner is working from a camera at an
    /// angle rather than from these exact pixels.
    #[test]
    fn the_mark_covers_a_small_share_of_the_code() {
        // Radius as a share of the code's width, then the stroke on top.
        let radius = 1.0 / (LOGO_SHARE as f64) / 2.0;
        let outer = radius * (1.0 + 1.0 / STROKE_SHARE);
        let area = std::f64::consts::PI * outer * outer;
        assert!(
            area < 0.12,
            "the mark and its stroke cover {:.1}% of the code, which is more \
             than is safe to lose in one piece",
            area * 100.0
        );
    }

    #[test]
    fn a_real_address_produces_a_square_code() {
        let code =
            qrcode::QrCode::new(payment_uri("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")).unwrap();
        let modules = code.to_colors();
        let width = (modules.len() as f64).sqrt() as usize;
        assert_eq!(width * width, modules.len(), "a QR code is square");
        assert!(width >= 21, "version 1 is 21 modules across");
    }
}

/// Icon names Sieve asks the theme for.
///
/// Every one must exist, or GTK silently draws the "missing image" glyph —
/// which is what `emblem-ok-symbolic` did in the coin picker, in the one place
/// meant to say "this is fine". Sieve's own icons are compiled in as resources
/// and are covered by the icon tests beside them.
#[cfg(test)]
mod icon_names {
    /// Names taken from the theme rather than from our own resources.
    const FROM_THEME: &[&str] = &[
        "object-select-symbolic",
        "dialog-warning-symbolic",
        "go-next-symbolic",
        "edit-copy-symbolic",
        "document-edit-symbolic",
        "document-open-symbolic",
        "document-save-symbolic",
        "document-open-recent-symbolic",
        "open-menu-symbolic",
        "view-refresh-symbolic",
        "view-reveal-symbolic",
        "web-browser-symbolic",
        "network-wireless-symbolic",
        "network-offline-symbolic",
        "network-idle-symbolic",
        "changes-prevent-symbolic",
        "system-search-symbolic",
        "channel-secure-symbolic",
    ];

    /// Needs a display and an icon theme, so it is not part of the default
    /// run — but it is the only way to catch a name that does not resolve
    /// before somebody sees a broken glyph.
    #[test]
    #[ignore = "needs a display and an icon theme"]
    fn every_icon_name_resolves() {
        relm4::gtk::init().expect("a display");
        let theme = relm4::gtk::IconTheme::for_display(
            &relm4::gtk::gdk::Display::default().expect("a display"),
        );
        let missing: Vec<&str> = FROM_THEME
            .iter()
            .copied()
            .filter(|name| !theme.has_icon(name))
            .collect();
        assert!(missing.is_empty(), "the icon theme has no {missing:?}");
    }
}

/// Writing a code out to look at.
///
/// There is no way to check a QR code by assertion — whether it *scans* is a
/// question for a camera, and whether it looks right is a question for eyes —
/// so this exists to put one on disk where both can reach it. It is how the
/// mark was got right: the first version drew the glyph at its own 256 pixels
/// instead of scaling it to the circle, and no test would have said so.
///
/// ```sh
/// QR_OUT=/tmp cargo test -- --ignored --nocapture qr_samples
/// ```
#[cfg(test)]
mod samples {
    use super::*;
    use relm4::gtk::prelude::TextureExt;

    #[test]
    #[ignore = "needs a display; writes pictures to look at"]
    fn qr_samples() {
        let Ok(dir) = std::env::var("QR_OUT") else {
            println!("set QR_OUT to a directory to write samples into");
            return;
        };
        relm4::gtk::init().expect("no display");

        // A taproot address, because it is the longest and so the densest
        // code — if the mark is going to crowd anything, it is this.
        let address = "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297";
        for network in ["bitcoin", "signet", "testnet4"] {
            let code = texture(&payment_uri(address), network).expect("a code");
            let path = format!("{dir}/qr-{network}.png");
            code.save_to_png(&path).expect("write");
            println!("wrote {path}");
        }
    }
}
