//! Unlock screen: password in, decrypted vault out.
//!
//! Argon2id at the configured cost takes roughly half a second, which would
//! visibly stall the frame clock, so it runs on the thread pool via
//! `spawn_oneshot_command` and reports back as a `CommandOutput`.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};
use zeroize::Zeroizing;

use crate::wallet::{self, Paths, Summary};

/// The wallet password, with a redacted `Debug`.
///
/// Relm4 traces messages when `RUST_LOG=relm4=trace`, and every message type
/// must implement `Debug`. The derived impl would print the password into the
/// log, so it is written by hand.
pub struct Password(Zeroizing<String>);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

#[derive(Debug)]
pub enum UnlockMsg {
    Submit(Password),
    /// Which wallet this screen is unlocking. Sent before the screen is shown.
    Open {
        paths: Paths,
        name: String,
    },
}

#[derive(Debug)]
pub enum UnlockOutput {
    /// Back to the wallet list.
    /// The vault opened. Only the watch-only summary travels — the decrypted
    /// seed never crosses a component boundary.
    Unlocked { paths: Paths, summary: Summary },
}

#[derive(Debug)]
pub enum UnlockCmd {
    Finished(Result<(Paths, Summary), String>),
}

pub struct Unlock {
    paths: Option<Paths>,
    name: String,
    many_wallets: bool,
    busy: bool,
    error: Option<String>,
}

#[relm4::component(pub)]
impl Component for Unlock {
    type Init = ();
    type Input = UnlockMsg;
    type Output = UnlockOutput;
    type CommandOutput = UnlockCmd;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Sieve",
                    #[watch]
                    set_subtitle: &model.name,
                },
            },

            // Not a StatusPage: its icon and padding are sized for a whole
            // screen, and inside a dialog they push the button out of view.
            #[wrap(Some)]
            set_content = &adw::Clamp {
                set_maximum_size: 360,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,
                    set_margin_all: 18,
                    set_valign: gtk::Align::Center,

                    gtk::Image {
                        set_icon_name: Some("channel-secure-symbolic"),
                        set_pixel_size: 48,
                        set_margin_bottom: 6,
                    },

                    gtk::Label {
                        add_css_class: "dim-label",
                        set_wrap: true,
                        set_justify: gtk::Justification::Center,
                        set_margin_bottom: 6,
                        set_label: "Enter your password to unlock this wallet.",
                    },

                    adw::PreferencesGroup {
                        #[name(password_row)]
                        adw::PasswordEntryRow {
                            set_title: "Password",
                            #[watch]
                            set_sensitive: !model.busy,
                            connect_entry_activated[sender] => move |row| {
                                sender.input(UnlockMsg::Submit(
                                    Password(Zeroizing::new(row.text().to_string()))
                                ));
                            },
                        },
                    },

                    gtk::Button {
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_halign: gtk::Align::Center,
                        #[watch]
                        set_sensitive: !model.busy,
                        #[watch]
                        set_label: if model.busy { "Unlocking…" } else { "Unlock" },
                        connect_clicked[sender, password_row] => move |_| {
                            sender.input(UnlockMsg::Submit(
                                Password(Zeroizing::new(password_row.text().to_string()))
                            ));
                        },
                    },

                    gtk::Label {
                        add_css_class: "error",
                        set_wrap: true,
                        set_justify: gtk::Justification::Center,
                        #[watch]
                        set_visible: model.error.is_some(),
                        #[watch]
                        set_label: model.error.as_deref().unwrap_or_default(),
                    },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Unlock {
            paths: None,
            name: String::new(),
            many_wallets: false,
            busy: false,
            error: None,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            UnlockMsg::Open { paths, name } => {
                self.many_wallets = wallet::list_wallets().len() > 1;
                self.paths = Some(paths);
                self.name = name;
                self.error = None;
                // Emptied for the wallet being opened. A password left in the
                // field belongs to a different wallet, and the one thing worse
                // than typing it again is submitting it without looking —
                // which is what a prefilled box invites.
                widgets.password_row.set_text("");
            }
            UnlockMsg::Submit(passphrase) => {
                let Some(paths) = self.paths.clone() else {
                    return;
                };
                if self.busy {
                    return;
                }
                self.busy = true;
                self.error = None;

                // Blocking and CPU-bound: goes to the thread pool, not the
                // main loop. `spawn_oneshot_command` cancels on shutdown.
                sender.spawn_oneshot_command(move || {
                    // Two shapes of wallet arrive here. One has a vault and
                    // the password decrypts a seed; the other has no keys at
                    // all and the password decrypts a token that exists only
                    // to be decrypted. Both fail the same way on a wrong
                    // password, which is why they can share this screen.
                    let opened = if paths.lock.exists() {
                        wallet::open_locked_watch_only(&paths, passphrase.0.as_bytes())
                    } else {
                        wallet::unlock(passphrase.0.as_bytes(), &paths)
                    };
                    let result = opened
                        .map(|summary| (paths, summary))
                        .map_err(|e| e.to_string());
                    UnlockCmd::Finished(result)
                });
            }
        }

        self.update_view(widgets, sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        let UnlockCmd::Finished(result) = msg;
        self.busy = false;
        match result {
            Ok((paths, summary)) => {
                // Nothing keeps a password that has done its job.
                widgets.password_row.set_text("");
                let _ = sender.output(UnlockOutput::Unlocked { paths, summary });
            }
            Err(message) => self.error = Some(message),
        }

        self.update_view(widgets, sender);
    }
}

#[cfg(test)]
mod tests {
    /// The wallet password must not print itself.
    ///
    /// `UnlockMsg::Submit` carries it, and relm4 formats every input into
    /// `input=?message` before the update runs — so a derived `Debug` here
    /// would write the password that opens the vault into the journal, from
    /// the one screen whose whole job is to take it.
    #[test]
    fn a_password_does_not_print_itself() {
        let password = super::Password(zeroize::Zeroizing::new("correct horse".to_string()));
        assert_eq!(format!("{password:?}"), "Password(<redacted>)");

        let message = super::UnlockMsg::Submit(password);
        let printed = format!("{message:?}");
        assert!(!printed.contains("correct horse"), "{printed}");
    }
}
