//! Opening a link, on a desktop that may or may not have a portal.
//!
//! Shared because getting it wrong is silent: `UriLauncher` goes through the
//! OpenURI portal, which is the right route under a sandbox and the only one
//! that works there — but it needs a portal backend answering, and plenty of
//! desktops have none. When it fails it fails quietly, and a button that does
//! nothing reads as a broken app.
//!
//! So: portal, then `xdg-open`, then hand over the link and say so.

use relm4::gtk;
use relm4::gtk::prelude::*;

/// Open `url`, falling back until something works.
///
/// `report` is called only when every route failed, with a message to show;
/// the link is on the clipboard by then.
pub fn open(url: &str, parent: &impl IsA<gtk::Widget>, report: impl Fn(String) + 'static) {
    let window = parent.as_ref().root().and_downcast::<gtk::Window>();
    let url = url.to_string();

    gtk::UriLauncher::new(&url).launch(
        window.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Err(e) = result else { return };
            tracing::warn!(%e, "portal could not open the browser; trying xdg-open");

            if std::process::Command::new("xdg-open").arg(&url).spawn().is_ok() {
                return;
            }
            tracing::warn!("xdg-open failed too");

            // Nothing could open it, so hand over the link rather than doing
            // nothing visible at all.
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&url);
            }
            report("Could not open your browser — link copied".into());
        },
    );
}
