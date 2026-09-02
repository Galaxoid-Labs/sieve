//! Sieve — a privacy-focused Bitcoin wallet.

mod about;
mod app;
mod fees;
mod hardware;
mod palette;
mod peers;
mod price;
mod settings;
mod tor;
mod ui;
mod vault;
mod wallet;

use relm4::RelmApp;
use relm4::gtk;

/// Reverse-DNS ID. Must match the .desktop file or GNOME won't associate the
/// window with the app icon.
pub const APP_ID: &str = "com.galaxoidlabs.Sieve";

/// Best-effort process hardening, applied before any secret exists.
///
/// `PR_SET_DUMPABLE(0)` is the one that matters: it stops another process
/// running as the same user from attaching a debugger and reading key material
/// out of memory. Core dumps are disabled for the same reason. `mlockall` is
/// attempted but routinely fails against `RLIMIT_MEMLOCK`; encrypted swap is
/// the reliable answer to secrets reaching disk.
/// Let this process be inspected again, for as long as the value is held.
///
/// `PR_SET_DUMPABLE(0)` does more than stop core dumps: the kernel re-owns
/// `/proc/<pid>` to root, and `xdg-desktop-portal` reads `/proc/<pid>/root` to
/// work out who is calling it. Unable to, it refuses everything — including
/// the file chooser, which then never appears and reports nothing. Every file
/// dialog in Sieve was silently doing nothing, and the portal warnings on
/// every start were this and not the session.
///
/// So it is lifted around a file dialog and put back when the dialog is done.
/// The exposure is a window somebody opened on purpose, during which Sieve is
/// handling public data — labels, descriptors, a PSBT — and no secret is
/// decrypted. Signing, which is the moment that matters, is never inside it.
pub struct Inspectable;

impl Inspectable {
    pub fn while_choosing_a_file() -> Self {
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0) };
        Self
    }
}

impl Drop for Inspectable {
    fn drop(&mut self) {
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    }
}

fn harden() {
    unsafe {
        let no_core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &no_core);

        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            tracing::warn!("PR_SET_DUMPABLE failed; this process is ptrace-attachable");
        }
        if libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) != 0 {
            tracing::debug!("mlockall failed; secrets may reach swap");
        }
    }
}

/// Answer `--version` and `--help` before anything opens a display.
///
/// A packaged build has to be checkable without a session — that is how a
/// container proves the binary it just installed is the one it meant to, and
/// GTK's own argument parsing happens too late and rejects both.
///
/// Returns true when the answer has been given and there is nothing else to do.
fn answered_on_the_command_line() -> bool {
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--version" | "-V" => {
                println!("sieve {}", env!("CARGO_PKG_VERSION"));
                return true;
            }
            "--help" | "-h" => {
                println!(
                    "sieve {}\n{}\n\n\
                     Usage: sieve [--version] [--help]\n\n\
                     Sieve is a window, not a command. Everything it does is done in the \
                     interface;\nthere are no subcommands and nothing to script.\n\n\
                     Wallets and settings live in ~/.local/share/sieve.\n\
                     Set RUST_LOG=sieve=debug for a running commentary.",
                    env!("CARGO_PKG_VERSION"),
                    env!("CARGO_PKG_DESCRIPTION"),
                );
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Sieve's own styling, on top of libadwaita's.
///
/// Held as a constant so that `parses_without_error` can put it through a
/// real CSS parser: a rule GTK cannot parse is dropped in silence, and the
/// only sign of it is something looking slightly wrong on screen.
const GLOBAL_CSS: &str = ".qr-ground { background-color: #ffffff; border-radius: 18px; padding: 6px; } \
     .seed-word { \
       background-color: alpha(currentColor, 0.07); \
       border-radius: 9px; \
       padding: 10px 12px; \
     } \
     .seed-index { opacity: 0.5; font-size: 0.8em; } \
     .seed-word entry { \
       background: none; \
       box-shadow: none; \
       outline: none; \
       min-height: 0; \
       padding: 0; \
     } \
     .seed-word:focus-within { \
       outline: 2px solid @accent_color; \
       outline-offset: -1px; \
     } \
     .die-face { \
       font-size: 1.05em; \
       font-weight: 700; \
       min-width: 48px; \
       min-height: 34px; \
       padding: 2px 6px; \
       border-radius: 10px; \
     } \
     .balance-mark { \
       font-size: 190px; \
       font-weight: 800; \
       color: alpha(@accent_bg_color, 0.20); \
       margin-left: -34px; \
       margin-bottom: -76px; \
       transform: rotate(-14deg); \
     } \
     /* The logo carries its own tilt and its own colour; all this \
        adds is the room around it. */ \
     .welcome-mark { margin-bottom: 10px; } \
     .welcome-name { \
       font-size: 34px; \
       font-weight: 800; \
       letter-spacing: 1px; \
     } \
     .welcome-line { font-size: 1.05em; } \
     /* Under the tagline, and quieter than it: same words at the same \
        weight would make the reader choose between two claims. */ \
     .welcome-note { font-size: 0.85em; font-style: italic; opacity: 0.45; } \
     /* Adwaita dims every row subtitle, which is right everywhere except \
        where the subtitle carries the number a decision turns on. */ \
     .full-contrast .subtitle { opacity: 1; } \
     /* A search bar in page content, without the chrome GTK gives one that \
        docks under a header bar: `searchbar > revealer > box` in Adwaita \
        carries a background colour and a bottom border, and together they \
        draw a box around a field that needs none. */ \
     .bare-search > revealer > box { \
       background: none; \
       border-width: 0; \
       padding: 0; \
     }";

fn main() {
    if answered_on_the_command_line() {
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sieve=info,relm4=warn".into()),
        )
        .init();

    harden();

    let app = RelmApp::new(APP_ID);

    // The one place Sieve hardcodes a colour, and it has to. A QR code needs
    // dark modules on a light ground to scan; inverting it for dark mode looks
    // tidier and fails on plenty of readers. So the code carries its own white
    // ground rather than sitting on the theme's card, which is dark exactly
    // when the code needs light.
    //
    // The radius has to be matched by clipping the picture to it, or the
    // code's own white square fills the corners it cuts away.
    //
    // The balance mark takes the desktop's accent on every chain, which makes
    // the card part of the theme rather than a sticker on it.
    //
    // It used to be tinted per network, to answer "which chain is this?" at a
    // glance. That turned out to be solving a problem the screen does not have
    // — the header names the chain in words directly above the card — while
    // creating one it did: a fixed colour has to work against a light card and
    // a dark one at a fifth of an alpha, and a dark signet purple simply
    // disappeared. A redundant signal is not worth a colour that cannot be
    // chosen well, and this leaves the QR code's white ground as the only
    // hardcoded colour in the program. Kept at a fifth of an alpha so the hue
    // reads the same against a light or a dark card.
    relm4::set_global_css(GLOBAL_CSS);

    // Sieve's own icons, compiled into the binary. Adwaita has no plain
    // vertical arrow — its options are a bare chevron, an arrow welded to a
    // bar, or a diagonal — and "money in" and "money out" deserve the obvious
    // shape.
    //
    // Registered as a resource rather than loaded from a path: a path built
    // from CARGO_MANIFEST_DIR only exists on the machine that compiled it, and
    // does not exist at all inside a sandbox.
    gtk::gio::resources_register_include!("sieve.gresource")
        .expect("the icon resource is compiled into this binary");

    app.run::<app::App>(());
}

#[cfg(test)]
mod tests {
    /// Everything `cargo deb` and `cargo generate-rpm` are told to package.
    ///
    /// These paths are read at release time and nowhere else, so a renamed
    /// icon or a moved rules file fails on a tag — after the version is cut,
    /// in a container, at the point where the fix is another tag. Checking
    /// them here means a rename fails in the same commit that made it.
    #[test]
    fn every_file_the_packaging_installs_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();

        // The metadata blocks and nothing above them: `[dependencies]` names
        // crates, not files.
        let start = manifest
            .find("[package.metadata.deb]")
            .expect("the deb metadata is what builds the .deb");
        let block = &manifest[start..];

        let mut checked = 0;
        for quoted in block.split('"').skip(1).step_by(2) {
            let is_path = quoted.starts_with("data/")
                || quoted.starts_with("packaging/")
                || matches!(quoted, "LICENSE" | "README.md" | "SECURITY.md");
            if !is_path {
                continue;
            }
            assert!(
                root.join(quoted).exists(),
                "the packaging installs {quoted}, which is not there"
            );
            checked += 1;
        }

        // Both tools, every icon size, the desktop entry, the udev rules and
        // the documents. A parse that quietly matched nothing would otherwise
        // pass this test by checking nothing at all.
        assert!(
            checked >= 24,
            "only {checked} paths checked — the metadata is not being read"
        );
    }

    /// A stylesheet GTK cannot parse is dropped rule by rule, in silence — the
    /// only sign is something looking slightly off. `@accent_bg_color` makes
    /// that a live risk: it is defined in libadwaita's stylesheet rather than
    /// in this string, and a name that does not resolve takes its whole
    /// declaration with it.
    #[test]
    #[ignore = "needs a display"]
    fn the_stylesheet_parses_without_error() {
        use relm4::gtk;
        gtk::init().expect("no display");

        let provider = gtk::CssProvider::new();
        let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        provider.connect_parsing_error({
            let errors = errors.clone();
            move |_, section, error| {
                errors.borrow_mut().push(format!("{section}: {error}"));
            }
        });
        provider.load_from_string(super::GLOBAL_CSS);

        let errors = errors.borrow();
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }
}
