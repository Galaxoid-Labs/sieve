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
    /// Which way the theme runs, where it says. This is the desktop's own
    /// statement of light or dark, and a fresher one than the copy it leaves
    /// in GNOME's settings — see `follow_desktop_scheme`.
    pub dark: Option<bool>,
    /// The surfaces, once mapped into the order Adwaita draws for.
    pub surfaces: Option<Surfaces>,
}

/// The surfaces a theme is built from, and the text that sits on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surfaces {
    /// The ordinary window.
    pub window: String,
    /// Behind a list or a document. Adwaita puts this *away* from the raised
    /// surfaces: darker than the window in a dark theme, level with it in a
    /// light one.
    pub view: String,
    /// Header bars, sidebars, dialogs and popovers — the surfaces that stand
    /// apart from the window. Lighter than it in a dark theme, darker in a
    /// light one, which is how Adwaita does it in both.
    pub raised: String,
    /// Text on all of them.
    pub text: String,
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
    let value = |wanted: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == wanted).then(|| value.trim().trim_matches('"').to_owned())
        })
    };

    let accent = value("accent").filter(|hex| is_hex(hex))?;
    let dark = match value("mode").as_deref() {
        Some("dark") => Some(true),
        Some("light") => Some(false),
        _ => None,
    };
    Some(Palette {
        accent,
        dark,
        surfaces: dark.and_then(|dark| surfaces(&value, dark)),
    })
}

/// The surfaces, mapped according to which way the theme runs.
///
/// The palette's *names* cannot be trusted across both modes — in
/// catppuccin-latte `lighter_background` is `#dce0e8` against a `#eff1f5`
/// background, so the "lighter" one is darker. They describe a dark theme's
/// structure. The *ordering* is reliable in both, and it is what Adwaita cares
/// about: a raised surface stands apart from the window, which means lighter
/// in a dark theme and darker in a light one.
fn surfaces(value: &impl Fn(&str) -> Option<String>, dark: bool) -> Option<Surfaces> {
    let hex = |key: &str| value(key).filter(|hex| is_hex(hex));
    let window = hex("background")?;

    // Adwaita draws shadows and separators assuming the window sits between
    // its view and its raised surfaces. Rather than refuse a theme whose
    // colours do not arrive in that order, they are clamped into it — which is
    // never wrong and is sometimes the only sensible reading:
    //
    //   vantablack   background #000000, dark_background #090909
    //                nothing can be darker than black, so the view is the
    //                window and the theme still applies.
    //   solitude     lighter_background equal to background
    //                a deliberately flat look, and equal is not out of order.
    let darker = |a: String, b: &str| match (luminance(&a), luminance(b)) {
        (Some(x), Some(y)) if y < x => b.to_owned(),
        _ => a,
    };
    let lighter = |a: String, b: &str| match (luminance(&a), luminance(b)) {
        (Some(x), Some(y)) if y > x => b.to_owned(),
        _ => a,
    };

    let found = if dark {
        Surfaces {
            view: darker(hex("dark_background")?, &window),
            raised: lighter(hex("lighter_background")?, &window),
            window,
            text: hex("foreground")?,
        }
    } else {
        // A light theme's palette holds nothing lighter than its background,
        // so the window is also the view — as in Adwaita, where the two are a
        // percent apart — and the raised surfaces step down from it.
        Surfaces {
            view: window.clone(),
            raised: darker(hex("dark_background")?, &window),
            window,
            text: hex("foreground")?,
        }
    };

    // Clamping can leave a theme whose colours are wholly inverted with no
    // depth at all. Adwaita's own surfaces are better than a flat one.
    let flat = found.view == found.window && found.raised == found.window;
    (!flat).then_some(found)
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
    ///
    /// `dark` is the colour scheme actually in force, which is the person's
    /// Appearance choice rather than the desktop theme's own mode. When those
    /// disagree — Light chosen while the desktop theme is dark — only the
    /// accent is taken, and libadwaita's own light surfaces stand.
    pub fn css(&self, dark: bool) -> String {
        let mut css = format!(
            "@define-color accent_bg_color {};\n@define-color accent_fg_color {};\n",
            self.accent,
            self.readable_on_accent(),
        );

        // Card is deliberately absent: libadwaita defines it as white at 8%,
        // an overlay that works on whatever is behind it. Naming a colour for
        // it would only make it wrong.
        if let Some(s) = self.surfaces.as_ref().filter(|_| self.dark == Some(dark)) {
            for (name, colour) in [
                ("window_bg_color", &s.window),
                ("view_bg_color", &s.view),
                ("headerbar_bg_color", &s.raised),
                ("sidebar_bg_color", &s.raised),
                ("secondary_sidebar_bg_color", &s.view),
                ("dialog_bg_color", &s.raised),
                ("popover_bg_color", &s.raised),
                ("thumbnail_bg_color", &s.raised),
            ] {
                css.push_str(&format!("@define-color {name} {colour};\n"));
            }
            for name in [
                "window_fg_color",
                "view_fg_color",
                "headerbar_fg_color",
                "sidebar_fg_color",
                "secondary_sidebar_fg_color",
                "dialog_fg_color",
                "popover_fg_color",
                "thumbnail_fg_color",
            ] {
                css.push_str(&format!("@define-color {name} {};\n", s.text));
            }
        }
        css
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
        let on = |hex: &str| {
            Palette {
                accent: hex.into(),
                dark: None,
                surfaces: None,
            }
            .readable_on_accent()
        };

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
    fn an_accent_alone_touches_nothing_else() {
        let css = Palette {
            accent: "#89b4fa".into(),
            dark: None,
            surfaces: None,
        }
        .css(true);
        assert!(css.contains("@define-color accent_bg_color #89b4fa;"));
        // With no surfaces, the only thing said is the accent — libadwaita
        // derives the rest from it, and every background stays Adwaita's.
        assert!(!css.contains("window_bg_color"), "{css}");
        assert_eq!(css.lines().count(), 2);
    }

    #[test]
    fn a_light_theme_maps_the_other_way_up() {
        // catppuccin-latte, where `lighter_background` (#dce0e8) is *darker*
        // than `background` (#eff1f5) — the names describe a dark theme. The
        // ordering is what is trusted: a raised surface steps away from the
        // window, which is downwards in a light theme.
        let latte = "mode = \"light\"\naccent = \"#1e66f5\"\nbackground = \"#eff1f5\"\n\
                     dark_background = \"#e3e4e8\"\nlighter_background = \"#dce0e8\"\n\
                     foreground = \"#4c4f69\"\n";
        let css = parse(latte).unwrap().css(false);

        assert!(
            css.contains("@define-color window_bg_color #eff1f5;"),
            "{css}"
        );
        assert!(
            css.contains("@define-color headerbar_bg_color #e3e4e8;"),
            "{css}"
        );
        assert!(
            css.contains("@define-color window_fg_color #4c4f69;"),
            "{css}"
        );
        // Nothing in a light palette is lighter than its background, so the
        // view is the window — as in Adwaita, where the two are a percent
        // apart.
        assert!(
            css.contains("@define-color view_bg_color #eff1f5;"),
            "{css}"
        );
    }

    #[test]
    fn surfaces_are_left_alone_when_the_scheme_disagrees() {
        // A dark desktop theme under a light colour scheme, or the reverse:
        // taking the surfaces would put one theme's backgrounds under the
        // other's text. The accent is a hue and survives either way.
        let catppuccin = "mode = \"dark\"\naccent = \"#89b4fa\"\nbackground = \"#1e1e2e\"\n\
                          dark_background = \"#161622\"\nlighter_background = \"#313244\"\n\
                          foreground = \"#cdd6f4\"\n";
        let palette = parse(catppuccin).unwrap();

        assert!(palette.css(true).contains("window_bg_color"));
        let disagreeing = palette.css(false);
        assert!(!disagreeing.contains("window_bg_color"), "{disagreeing}");
        assert!(disagreeing.contains("accent_bg_color #89b4fa"));
    }

    #[test]
    fn a_dark_theme_maps_its_surfaces_in_adwaitas_order() {
        let catppuccin = "mode = \"dark\"\naccent = \"#89b4fa\"\nbackground = \"#1e1e2e\"\n\
                          dark_background = \"#161622\"\nlighter_background = \"#313244\"\n\
                          foreground = \"#cdd6f4\"\n";
        let css = parse(catppuccin).unwrap().css(true);

        assert!(
            css.contains("@define-color window_bg_color #1e1e2e;"),
            "{css}"
        );
        assert!(
            css.contains("@define-color view_bg_color #161622;"),
            "{css}"
        );
        assert!(
            css.contains("@define-color headerbar_bg_color #313244;"),
            "{css}"
        );
        assert!(
            css.contains("@define-color window_fg_color #cdd6f4;"),
            "{css}"
        );

        // The card is an overlay in libadwaita — white at 8%, which works on
        // whatever is behind it. Naming a colour for it would only make it
        // wrong.
        assert!(!css.contains("card_bg_color"), "{css}");
    }

    #[test]
    fn a_theme_states_which_way_it_runs() {
        // This is what the colour scheme is taken from, so it is read whether
        // or not the surfaces map: Omarchy copies the mode into GNOME's
        // settings as a separate step, and that copy has been seen stale.
        let light = "mode = \"light\"\naccent = \"#1e66f5\"\n";
        assert_eq!(parse(light).unwrap().dark, Some(false));

        let dark = "mode = \"dark\"\naccent = \"#89b4fa\"\n";
        assert_eq!(parse(dark).unwrap().dark, Some(true));

        // No mode, so no opinion — and the desktop's own setting is used.
        let quiet = "accent = \"#89b4fa\"\n";
        assert_eq!(parse(quiet).unwrap().dark, None);
    }

    #[test]
    fn surfaces_are_clamped_into_adwaitas_order() {
        // Themes do not always order their surfaces the way Adwaita needs, and
        // sometimes cannot: vantablack's background is #000000, so its
        // dark_background is necessarily lighter. Clamping keeps the theme
        // rather than refusing it, and can never render the app inside out.
        let vantablack = "mode = \"dark\"\naccent = \"#8d8d8d\"\nbackground = \"#000000\"\n\
                          dark_background = \"#090909\"\nlighter_background = \"#1a1a1a\"\n\
                          foreground = \"#d0d0d0\"\n";
        let surfaces = parse(vantablack).unwrap().surfaces.unwrap();
        assert_eq!(surfaces.window, "#000000");
        assert_eq!(surfaces.view, "#000000", "nothing is darker than black");
        assert_eq!(surfaces.raised, "#1a1a1a");
    }

    #[test]
    fn a_theme_with_no_depth_left_after_clamping_is_refused() {
        // Every surface inverted, so the clamp flattens them onto the window.
        // Adwaita's surfaces carry more than a single flat colour would.
        let inverted = "mode = \"dark\"\naccent = \"#89b4fa\"\nbackground = \"#161622\"\n\
                        dark_background = \"#313244\"\nlighter_background = \"#101019\"\n\
                        foreground = \"#cdd6f4\"\n";
        let palette = parse(inverted).unwrap();
        assert_eq!(palette.accent, "#89b4fa");
        assert!(palette.surfaces.is_none());
    }
}
