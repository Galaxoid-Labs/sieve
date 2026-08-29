//! Sieve — a privacy-focused Bitcoin wallet.

mod app;
mod peers;
mod price;
mod settings;
mod ui;
mod vault;
mod wallet;

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

    // The one place Sieve hardcodes a colour, and it has to. A QR code must be
    // dark modules on a light ground to scan; inverting it for dark mode looks
    // tidier and fails on plenty of readers. So the code carries its own white
    // ground in both themes rather than sitting on the theme's card, which is
    // dark exactly when the code needs light.
    app.set_global_css(
        // No padding: the code carries a four-module quiet zone of its own,
        // which is the specification's requirement, and stacking padding on top
        // of it just shrinks the code.
        ".qr-ground { background-color: #ffffff; border-radius: 12px; padding: 4px; }",
    );

    app.run::<app::App>(());
}
