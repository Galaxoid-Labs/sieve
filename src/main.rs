//! Sieve — a privacy-focused Bitcoin wallet.

mod app;
mod peers;
mod fees;
mod price;
mod settings;
mod ui;
mod vault;
mod wallet;

use relm4::gtk;
use relm4::gtk::prelude::*;
use relm4::RelmApp;

/// Reverse-DNS ID. Must match the .desktop file or GNOME won't associate the
/// window with the app icon.
const APP_ID: &str = "com.jdavis.Sieve";

/// Best-effort process hardening, applied before any secret exists.
///
/// `PR_SET_DUMPABLE(0)` is the one that matters: it stops another process
/// running as the same user from attaching a debugger and reading key material
/// out of memory. Core dumps are disabled for the same reason. `mlockall` is
/// attempted but routinely fails against `RLIMIT_MEMLOCK`; encrypted swap is
/// the reliable answer to secrets reaching disk.
fn harden() {
    unsafe {
        let no_core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &no_core);

        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 {
            tracing::warn!("PR_SET_DUMPABLE failed; this process is ptrace-attachable");
        }
        if libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) != 0 {
            tracing::debug!("mlockall failed; secrets may reach swap");
        }
    }
}

fn main() {
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
    app.set_global_css(
        ".qr-ground { background-color: #ffffff; border-radius: 18px; padding: 6px; } \
         .seed-word { \
           background-color: alpha(currentColor, 0.07); \
           border-radius: 9px; \
           padding: 10px 12px; \
         } \
         .seed-index { opacity: 0.5; font-size: 0.8em; }",
    );

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
