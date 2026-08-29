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
}

#[derive(Debug)]
pub enum UnlockOutput {
    /// The vault opened. Only the watch-only summary travels — the decrypted
    /// seed never crosses a component boundary.
    Unlocked(Summary),
}

#[derive(Debug)]
pub enum UnlockCmd {
    Finished(Result<Summary, String>),
}

pub struct Unlock {
    paths: Paths,
    busy: bool,
    error: Option<String>,
}

#[relm4::component(pub)]
impl Component for Unlock {
    type Init = Paths;
    type Input = UnlockMsg;
    type Output = UnlockOutput;
    type CommandOutput = UnlockCmd;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Sieve",
                    set_subtitle: "Locked",
                },
            },

            #[wrap(Some)]
            set_content = &adw::StatusPage {
            set_icon_name: Some("channel-secure-symbolic"),
            set_title: "Sieve",
            set_description: Some("Enter your password to unlock this wallet."),

            #[wrap(Some)]
            set_child = &adw::Clamp {
                set_maximum_size: 360,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,

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
            },
        }
    }

    fn init(
        paths: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Unlock { paths, busy: false, error: None };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            UnlockMsg::Submit(passphrase) => {
                if self.busy {
                    return;
                }
                self.busy = true;
                self.error = None;

                let paths = self.paths.clone();
                // Blocking and CPU-bound: goes to the thread pool, not the
                // main loop. `spawn_oneshot_command` cancels on shutdown.
                sender.spawn_oneshot_command(move || {
                    UnlockCmd::Finished(
                        wallet::unlock(passphrase.0.as_bytes(), &paths)
                            .map_err(|e| e.to_string()),
                    )
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        let UnlockCmd::Finished(result) = msg;
        self.busy = false;
        match result {
            Ok(summary) => {
                let _ = sender.output(UnlockOutput::Unlocked(summary));
            }
            Err(message) => self.error = Some(message),
        }
    }
}
