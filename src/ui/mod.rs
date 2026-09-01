//! Adwaita components.
//!
//! Every screen is built from stock libadwaita widgets so the app follows the
//! GNOME HIG by construction: <https://developer.gnome.org/hig/>

pub mod browser;
pub mod chooser;
pub mod onboarding;
pub mod qr;
pub mod restore;
pub mod reveal;
pub mod send;
pub mod unlock;
pub mod wallet_page;

/// How long a toast stays on screen, in seconds.
///
/// Two rather than libadwaita's five. Every toast in Sieve confirms something
/// the person has just done — frozen a coin, sent a payment, copied an address
/// — so it is read within a second and is furniture after that, sitting over
/// the content it is describing and inviting a click to dismiss it.
///
/// Nothing here is the *only* record of anything: a payment that was sent is in
/// the activity list, a frozen coin wears a padlock, a raised fee opens the page
/// it made. The toast is a receipt for an action whose result is already on
/// screen, which is what makes a short one safe.
const TOAST_SECONDS: u32 = 2;

/// A toast that does not outstay its welcome.
///
/// Every toast goes through here rather than through `adw::Toast::new`, so the
/// timeout is one decision in one place. Seventeen call sites each setting their
/// own would drift, and the drift would be invisible — a toast that lingers
/// reads as sluggishness rather than as a setting anybody chose.
pub fn toast(message: &str) -> relm4::adw::Toast {
    let toast = relm4::adw::Toast::new(message);
    toast.set_timeout(TOAST_SECONDS);
    toast
}
