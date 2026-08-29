//! Unlock screen: passphrase in, decrypted vault out.
//!
//! Argon2id at the configured cost takes roughly half a second, which would
//! visibly stall the frame clock, so it runs on the thread pool via
//! `spawn_oneshot_command` and reports back as a `CommandOutput`.

use std::path::PathBuf;

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};
use zeroize::Zeroizing;

use crate::vault;

/// Passphrase with a redacted `Debug`.
///
/// Relm4 traces messages when `RUST_LOG=relm4=trace`, and every message type
/// must implement `Debug`. The derived impl would print the passphrase into the
/// log, so it is written by hand.
pub struct Passphrase(Zeroizing<String>);

impl std::fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Passphrase(<redacted>)")
    }
}

#[derive(Debug)]
pub enum UnlockMsg {
    Submit(Passphrase),
}

#[derive(Debug)]
pub enum UnlockOutput {
    /// The vault opened. The decrypted material is deliberately *not* carried
    /// in this message — it goes to the signer worker, not through the UI tree.
    Unlocked,
}

#[derive(Debug)]
pub enum UnlockCmd {
    Finished(Result<(), String>),
}

pub struct Unlock {
    vault_path: PathBuf,
    busy: bool,
    error: Option<String>,
}

#[relm4::component(pub)]
impl Component for Unlock {
    type Init = PathBuf;
    type Input = UnlockMsg;
    type Output = UnlockOutput;
    type CommandOutput = UnlockCmd;

    view! {
        adw::StatusPage {
            set_icon_name: Some("channel-secure-symbolic"),
            set_title: "Sieve",
            set_description: Some("Enter your passphrase to unlock this wallet."),

            #[wrap(Some)]
            set_child = &adw::Clamp {
                set_maximum_size: 360,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 12,

                    adw::PreferencesGroup {
                        #[name(password_row)]
                        adw::PasswordEntryRow {
                            set_title: "Passphrase",
                            #[watch]
                            set_sensitive: !model.busy,
                            connect_entry_activated[sender] => move |row| {
                                sender.input(UnlockMsg::Submit(
                                    Passphrase(Zeroizing::new(row.text().to_string()))
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
                                Passphrase(Zeroizing::new(password_row.text().to_string()))
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
        vault_path: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Unlock { vault_path, busy: false, error: None };
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

                let path = self.vault_path.clone();
                // Blocking and CPU-bound: goes to the thread pool, not the
                // main loop. `spawn_oneshot_command` cancels on shutdown.
                sender.spawn_oneshot_command(move || {
                    let result = std::fs::read(&path)
                        .map_err(|e| format!("Cannot read the vault: {e}"))
                        .and_then(|blob| {
                            vault::open(&blob, passphrase.0.as_bytes())
                                .map_err(|e| e.to_string())
                        })
                        // TODO: hand the decrypted seed to the signer worker
                        // instead of dropping it here.
                        .map(|seed| drop(seed));
                    UnlockCmd::Finished(result)
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
            Ok(()) => {
                let _ = sender.output(UnlockOutput::Unlocked);
            }
            Err(message) => self.error = Some(message),
        }
    }
}
