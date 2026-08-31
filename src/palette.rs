//! The desktop's own colours, where the desktop publishes them.
//!
//! GNOME publishes an accent as one of nine names — blue, teal, green and so
//! on — and libadwaita turns that into `@accent_bg_color`, which everything
//! else derives from. That is what a GTK application normally follows, and
//! Sieve does, for free, because libadwaita does it.
//!
//! Omarchy is different. Its themes carry a full palette in a file, and its
//! GNOME integration applies only the colour scheme, the GTK theme name and
//! the icon theme — never the accent. So a machine themed catppuccin has a
//! purple file manager, a blue-lavender terminal, and stock GNOME blue
//! buttons. This reads the palette the rest of the desktop is already using
//! and hands the accent to libadwaita.
//!
//! Only the accent. A palette is not a theme: libadwaita derives cards,
//! headerbars, dialogs, popovers, sidebars and half a dozen shades from its
//! own background colours, and replacing one of them leaves the rest
//! mismatched. The accent has a single, well-defined role, which is why it can
//! be swapped without unpicking anything.

use std::path::PathBuf;

/// The colours a desktop theme publishes, as far as Sieve reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// What the desktop uses for primary buttons, selection and focus.
    pub accent: String,
}

/// Where Omarchy keeps the theme currently applied, under a state directory.
///
/// A symlink into the theme's own directory, so this follows a theme change
/// without anything being copied. Taking the base as an argument is what lets
/// the "no palette here" case be tested rather than asserted.
fn omarchy_colors(state: &std::path::Path) -> PathBuf {
    state.join("omarchy/current/theme/colors.toml")
}

fn dirs_state() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::BaseDirs::new()
                .map(|d| d.home_dir().join(".local/state"))
                .unwrap_or_else(|| PathBuf::from(".local/state"))
        })
}

/// The directory to watch, so a theme change is noticed.
pub fn watch_dir() -> PathBuf {
    dirs_state().join("omarchy/current")
}

/// Read the desktop's palette, if this desktop publishes one.
///
/// Detected by the file rather than by the distribution: the question is
/// whether there is a palette to read, which stays the right question if
/// somebody runs Omarchy's theming on plain Arch, or if the file is missing on
/// a half-installed Omarchy.
pub fn desktop() -> Option<Palette> {
    desktop_in(&dirs_state())
}

/// The same, under a given state directory.
fn desktop_in(state: &std::path::Path) -> Option<Palette> {
    let text = std::fs::read_to_string(omarchy_colors(state)).ok()?;
    parse(&text)
}

/// `key = "#rrggbb"`, one per line.
///
/// Hand-parsed rather than pulling in a TOML crate for one key: a dependency
/// is a decision, `deny.toml` says so, and this file is four tokens wide.
fn parse(text: &str) -> Option<Palette> {
    let accent = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "accent").then(|| value.trim().trim_matches('"').to_owned())
    })?;

    is_hex(&accent).then_some(Palette { accent })
}

/// `#rrggbb`, and nothing else. What goes into this string is written straight
/// into a stylesheet, so it is checked rather than trusted.
fn is_hex(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

impl Palette {
    /// The stylesheet that hands this accent to libadwaita.
    ///
    /// Only `accent_bg_color` and the text colour that sits on it.
    /// `accent_color`, `theme_selected_bg_color` and the rest are defined by
    /// libadwaita in terms of `@accent_bg_color`, so they follow.
    pub fn css(&self) -> String {
        format!(
            "@define-color accent_bg_color {};\n@define-color accent_fg_color {};\n",
            self.accent,
            self.readable_on_accent(),
        )
    }

    /// Black or white, whichever can be read on this accent.
    ///
    /// libadwaita hardcodes white. That is right for the nine GNOME accents
    /// and wrong for a light one: catppuccin's `#89b4fa` under a white label
    /// is a button nobody can read.
    ///
    /// The threshold is not a guess. White on a background of relative
    /// luminance `l` has a contrast ratio of `1.05 / (l + 0.05)`, which falls
    /// below the 3:1 minimum at `l = 0.30` — and GNOME's lightest accent,
    /// yellow at 0.299, sits immediately under it. Their palette is chosen to
    /// clear the bar with white, so this line keeps every GNOME accent exactly
    /// as libadwaita intends and flips only the ones that would fail.
    fn readable_on_accent(&self) -> &'static str {
        match luminance(&self.accent) {
            Some(l) if l > 0.30 => "rgb(0, 0, 6)",
            _ => "white",
        }
    }
}

/// WCAG relative luminance of a `#rrggbb` colour, 0.0 to 1.0.
fn luminance(hex: &str) -> Option<f64> {
    if !is_hex(hex) {
        return None;
    }
    let channel = |from: usize| {
        let raw = u8::from_str_radix(&hex[from..from + 2], 16).ok()? as f64 / 255.0;
        // sRGB is gamma-encoded; luminance is not.
        Some(if raw <= 0.04045 {
            raw / 12.92
        } else {
            ((raw + 0.055) / 1.055).powf(2.4)
        })
    };
    Some(0.2126 * channel(1)? + 0.7152 * channel(3)? + 0.0722 * channel(5)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop that publishes no palette must be left entirely alone: GNOME
    /// sets its own accent, libadwaita applies it, and Sieve has nothing to
    /// add. Anything else here would be a regression on every machine that is
    /// not this one.
    #[test]
    fn a_desktop_without_a_palette_is_left_alone() {
        let empty = std::env::temp_dir().join(format!("sieve-nopalette-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            desktop_in(&empty).is_none(),
            "found a palette where there is none"
        );

        // And a file that exists but says nothing useful is the same case,
        // rather than a reason to write half a stylesheet.
        let broken = empty.join("omarchy/current/theme");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("colors.toml"), "mode = \"dark\"\n").unwrap();
        assert!(desktop_in(&empty).is_none());

        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn reads_the_accent_out_of_a_theme_file() {
        let file = "mode = \"dark\"\n\naccent = \"#89b4fa\"\nbackground = \"#1e1e2e\"\n";
        assert_eq!(parse(file).unwrap().accent, "#89b4fa");
    }

    #[test]
    fn refuses_anything_that_is_not_a_colour() {
        // Whatever this returns is written into a stylesheet, so a value that
        // is not six hex digits must not get that far.
        for bad in [
            "accent = \"red\"",
            "accent = \"#12345\"",
            "accent = \"#zzzzzz\"",
            "accent = \"#89b4fa; } * { color: red\"",
            "background = \"#1e1e2e\"",
            "",
        ] {
            assert!(parse(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn the_label_can_be_read_on_the_button() {
        let on = |hex: &str| Palette { accent: hex.into() }.readable_on_accent();

        // Every GNOME accent keeps white, as libadwaita intends — including
        // yellow, the lightest of them, which sits a thousandth under the
        // line. If this ever flips, Sieve's buttons would stop matching every
        // other application on a stock GNOME desktop.
        for gnome in [
            "#3584e4", "#2190a4", "#3a944a", "#c88800", "#ed5b00", "#e62d42", "#d56199", "#9141ac",
            "#6f8396",
        ] {
            assert_eq!(on(gnome), "white", "{gnome}");
        }

        // And the light ones, which white cannot be read on. These are real
        // accents from installed themes rather than invented colours.
        for light in [
            "#89b4fa", // catppuccin
            "#82FB9C", // hackerman
            "#dcd7ba", // kanagawa
            "#8bc9eb", // lumon
            "#7fbbb3", // everforest
            "#7daea3", // gruvbox
        ] {
            assert_eq!(on(light), "rgb(0, 0, 6)", "{light}");
        }
    }

    #[test]
    fn the_stylesheet_says_only_what_it_means_to() {
        let css = Palette {
            accent: "#89b4fa".into(),
        }
        .css();
        assert!(css.contains("@define-color accent_bg_color #89b4fa;"));
        // Everything else libadwaita derives from that one colour, so nothing
        // else belongs here.
        assert!(!css.contains("window_bg_color"), "{css}");
        assert_eq!(css.lines().count(), 2);
    }
}
