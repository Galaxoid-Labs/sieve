//! Adwaita components.
//!
//! Every screen is built from stock libadwaita widgets so the app follows the
//! GNOME HIG by construction: <https://developer.gnome.org/hig/>

pub mod browser;
pub mod chooser;
pub mod onboarding;
pub mod phrase;
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

/// Give an `adw::EntryRow` a way out.
///
/// Adwaita's apply button commits an edit and there is no counterpart to
/// abandon one, so every field in Sieve that edits something already saved was
/// a one-way door: the exits were saving something or leaving the screen. That
/// is worst on the rows that *replace* a display line while they are open,
/// where leaving the screen is the only exit at all — but it is wrong
/// everywhere, because a field holding a half-typed name has no way back to the
/// name it started with.
///
/// `cancel` is what "back" means for that row, and every caller does the same
/// two things in it: put the saved value back, and close the field if it opens.
/// Restoring is not optional — a cancel that closed the row and kept the typing
/// would put the abandoned text back on screen the next time it opened.
///
/// `close_tooltip` adds a visible button for the rows that open and shut. A
/// settings row that is always on screen gets Escape alone: there is nothing to
/// close, and a permanent ✕ beside a preference reads as "remove this setting".
///
/// **`cancel` returns whether it had anything to cancel**, and that answer
/// decides whether Escape is swallowed. A row that swaps a display line for a
/// field always has something to do — shut the field — and must swallow it, or
/// the same keypress would also close the dialog the row is sitting in. A
/// settings row with nothing typed into it has nothing to undo, and swallowing
/// there would break the one thing Escape is for in a preferences window.
pub fn cancellable_edit(
    row: &relm4::adw::EntryRow,
    close_tooltip: Option<&str>,
    cancel: impl Fn() -> bool + Clone + 'static,
) {
    use relm4::adw::prelude::*;
    use relm4::gtk;

    if let Some(tooltip) = close_tooltip {
        let button = gtk::Button::from_icon_name("window-close-symbolic");
        button.set_tooltip_text(Some(tooltip));
        button.set_valign(gtk::Align::Center);
        button.add_css_class("flat");
        let cancel = cancel.clone();
        button.connect_clicked(move |_| {
            cancel();
        });
        row.add_prefix(&button);
    }

    let escape = gtk::EventControllerKey::new();
    escape.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape && cancel() {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    row.add_controller(escape);
}
