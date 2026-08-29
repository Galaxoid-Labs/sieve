//! Import an existing wallet.
//!
//! One page rather than a wizard: every choice here interacts with the others —
//! the network changes which birthdays exist, the credential kind changes which
//! fields matter — and a person importing a wallet they already own should be
//! able to see the whole form at once.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};
use zeroize::Zeroizing;

use crate::wallet::accounts::{CredentialKind, ScriptType};
use crate::wallet::{self, Paths, Summary};

const KINDS: [CredentialKind; 4] = [
    CredentialKind::Mnemonic,
    CredentialKind::ExtendedKey,
    CredentialKind::Wif,
    CredentialKind::Descriptor,
];

/// Secret with a redacted `Debug`, so relm4's message tracing cannot print a
/// seed phrase, a key, or a password.
pub struct Secret(Zeroizing<String>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug)]
pub struct Submission {
    kind: CredentialKind,
    credential: Secret,
    bip39_passphrase: Secret,
    password: Secret,
    confirm: Secret,
    network_index: u32,
    birthday_index: u32,
    acknowledged: bool,
}

#[derive(Debug)]
pub enum RestoreMsg {
    KindChanged(u32),
    BirthdayChanged(u32),
    NetworkChanged(u32),
    Submit(Box<Submission>),
    Cancel,
}

#[derive(Debug)]
pub enum RestoreOutput {
    Imported { paths: Paths, summary: Summary },
    Cancelled,
}

#[derive(Debug)]
pub enum RestoreCmd {
    Finished(Result<(Paths, Summary), String>),
}

pub struct Restore {
    kind: CredentialKind,
    network: bdk_wallet::bitcoin::Network,
    birthday_index: u32,
    busy: bool,
    error: Option<String>,
}

/// Networks offered, signet first so the safe option is the default.
fn networks() -> [bdk_wallet::bitcoin::Network; 2] {
    [bdk_wallet::bitcoin::Network::Signet, bdk_wallet::bitcoin::Network::Bitcoin]
}

impl Restore {
    fn is_mainnet(&self) -> bool {
        self.network == bdk_wallet::bitcoin::Network::Bitcoin
    }

    fn credential_title(&self) -> &'static str {
        match self.kind {
            CredentialKind::Mnemonic => "Recovery phrase",
            CredentialKind::ExtendedKey => "Extended private key",
            CredentialKind::Wif => "Private key",
            CredentialKind::Descriptor => "Descriptor or xpub",
        }
    }

    fn credential_hint(&self) -> &'static str {
        match self.kind {
            CredentialKind::Mnemonic => "The 12 or 24 words, separated by spaces",
            CredentialKind::ExtendedKey => "An xprv, tprv or vprv. No recovery phrase needed",
            CredentialKind::Wif => "A single private key in Wallet Import Format",
            CredentialKind::Descriptor => "Watch-only. Sieve will never hold a key for this wallet",
        }
    }

    /// What the import will actually watch.
    fn paths_summary(&self) -> String {
        match self.kind {
            CredentialKind::Descriptor => "As described by the descriptor".into(),
            _ => ScriptType::ALL
                .iter()
                .map(|s| s.label())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// Resolve a choice to a checkpoint on the current network.
    ///
    /// Index 0 is the most recent checkpoint, and the last choice is always the
    /// floor, so "I don't know" is always available and always correct.
    fn birthday_for(&self, index: u32) -> wallet::Checkpoint {
        let all = wallet::checkpoints(self.network);
        all.get(index as usize)
            .copied()
            .unwrap_or_else(|| *all.last().expect("a floor checkpoint exists"))
    }

    /// What the chosen birthday actually resolves to, so the consequence of the
    /// choice is visible before importing.
    fn birthday_label(&self, index: u32) -> String {
        let checkpoint = self.birthday_for(index);
        format!("{} — from block {}", checkpoint.when, checkpoint.height)
    }
}

#[relm4::component(pub)]
impl Component for Restore {
    type Init = ();
    type Input = RestoreMsg;
    type Output = RestoreOutput;
    type CommandOutput = RestoreCmd;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Import a wallet",
                    #[watch]
                    set_subtitle: if model.is_mainnet() { "Bitcoin" } else { "Signet" },
                },
                pack_start = &gtk::Button {
                    set_icon_name: "go-previous-symbolic",
                    set_tooltip_text: Some("Back"),
                    connect_clicked => RestoreMsg::Cancel,
                },
            },

            #[wrap(Some)]
            set_content = &adw::PreferencesPage {

                adw::PreferencesGroup {
                    set_title: "What are you importing?",

                    #[name(kind_row)]
                    adw::ComboRow {
                        set_title: "Type",
                        set_model: Some(&gtk::StringList::new(
                            &KINDS.map(|k| k.label())
                        )),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(RestoreMsg::KindChanged(row.selected()));
                        },
                    },

                    adw::ActionRow {
                        set_title: "Derivation paths searched",
                        #[watch]
                        set_subtitle: &model.paths_summary(),
                        set_subtitle_lines: 2,
                    },
                },

                adw::PreferencesGroup {
                    #[watch]
                    set_title: model.credential_title(),
                    #[watch]
                    set_description: Some(model.credential_hint()),

                    #[name(credential_row)]
                    adw::EntryRow {
                        #[watch]
                        set_title: model.credential_title(),
                    },

                    #[name(bip39_expander)]
                    adw::ExpanderRow {
                        set_title: "My seed has a passphrase",
                        set_subtitle: "Sometimes called a 25th word. Most seeds do not have one — leave this off if you were never asked to choose one.",
                        set_show_enable_switch: true,
                        set_enable_expansion: false,
                        // Only meaningful for a seed, and dangerous to confuse
                        // with the wallet password, so it is hidden otherwise.
                        #[watch]
                        set_visible: model.kind.is_hd(),

                        #[name(bip39_row)]
                        add_row = &adw::PasswordEntryRow {
                            set_title: "BIP-39 passphrase",
                        },
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Network",

                    #[name(network_row)]
                    adw::ComboRow {
                        set_title: "Chain",
                        set_model: Some(&gtk::StringList::new(&["Signet (test coins)", "Bitcoin (real coins)"])),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(RestoreMsg::NetworkChanged(row.selected()));
                        },
                    },

                    #[name(acknowledge_row)]
                    adw::SwitchRow {
                        add_css_class: "warning",
                        set_title: "I understand this software is unreviewed",
                        set_subtitle: "Sieve has had no external security review. Its vault format and key handling are unaudited. Do not import a seed holding money you cannot afford to lose.",
                        #[watch]
                        set_visible: model.is_mainnet(),
                    },
                },

                adw::PreferencesGroup {
                    set_title: "History starts at",
                    set_description: Some(
                        "Sieve scans block filters from here. Choosing a date earlier \
                         than the wallet's first payment is safe but slower; choosing a \
                         later one misses coins."
                    ),

                    #[name(birthday_row)]
                    adw::ComboRow {
                        set_title: "Earliest possible payment",
                        // Fixed choices, so selecting the network never resets
                        // this and this never resets anything else.
                        set_model: Some(&gtk::StringList::new(&[
                            "Recently",
                            "Within about a year",
                            "One to two years ago",
                            "Two to three years ago",
                            "Three or more years ago",
                            "I don't know",
                        ])),
                        set_selected: 1,
                        connect_selected_notify[sender] => move |row| {
                            sender.input(RestoreMsg::BirthdayChanged(row.selected()));
                        },
                    },

                    adw::ActionRow {
                        add_css_class: "dim-label",
                        set_title: "Scanning from",
                        #[watch]
                        set_subtitle: &model.birthday_label(model.birthday_index),
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Choose a password for this wallet",
                    set_description: Some(
                        "New — you are choosing it now. It locks this wallet on this \
                         computer and has nothing to do with your recovery phrase or \
                         your old wallet. Forgetting it costs this copy, not your coins."
                    ),

                    #[name(password_row)]
                    adw::PasswordEntryRow {
                        set_title: "New password",
                    },
                    #[name(confirm_row)]
                    adw::PasswordEntryRow {
                        set_title: "Confirm new password",
                    },
                },

                adw::PreferencesGroup {
                    gtk::Button {
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        set_halign: gtk::Align::Center,
                        #[watch]
                        set_sensitive: !model.busy,
                        #[watch]
                        set_label: if model.busy { "Importing…" } else { "Import wallet" },
                        connect_clicked[
                            sender, kind_row, credential_row, bip39_row, bip39_expander,
                            network_row, birthday_row, password_row, confirm_row,
                            acknowledge_row
                        ] => move |_| {
                            sender.input(RestoreMsg::Submit(Box::new(Submission {
                                kind: KINDS[kind_row.selected() as usize],
                                credential: Secret(Zeroizing::new(
                                    credential_row.text().to_string()
                                )),
                                // Only when the switch is on: an empty field
                                // and "no passphrase" must mean the same thing.
                                bip39_passphrase: Secret(Zeroizing::new(
                                    if bip39_expander.enables_expansion() {
                                        bip39_row.text().to_string()
                                    } else {
                                        String::new()
                                    }
                                )),
                                password: Secret(Zeroizing::new(password_row.text().to_string())),
                                confirm: Secret(Zeroizing::new(confirm_row.text().to_string())),
                                network_index: network_row.selected(),
                                birthday_index: birthday_row.selected(),
                                acknowledged: acknowledge_row.is_active(),
                            })));
                        },
                    },

                    gtk::Label {
                        add_css_class: "error",
                        set_wrap: true,
                        set_justify: gtk::Justification::Center,
                        set_margin_top: 8,
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
        let model = Restore {
            kind: CredentialKind::Mnemonic,
            network: bdk_wallet::bitcoin::Network::Signet,
            birthday_index: 1,
            busy: false,
            error: None,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            RestoreMsg::KindChanged(index) => {
                if let Some(kind) = KINDS.get(index as usize) {
                    self.kind = *kind;
                }
                self.error = None;
            }
            RestoreMsg::BirthdayChanged(index) => {
                self.birthday_index = index;
                self.error = None;
            }
            RestoreMsg::NetworkChanged(index) => {
                self.network = networks()[(index as usize).min(1)];
                self.error = None;
            }
            RestoreMsg::Cancel => {
                let _ = sender.output(RestoreOutput::Cancelled);
            }
            RestoreMsg::Submit(submission) => {
                if self.busy {
                    return;
                }
                let network = networks()[(submission.network_index as usize).min(1)];

                if network == bdk_wallet::bitcoin::Network::Bitcoin && !submission.acknowledged {
                    self.error =
                        Some("Confirm you understand the risk before importing to Bitcoin.".into());
                    return;
                }
                if submission.credential.0.trim().is_empty() {
                    // Judge the submission, not the model: they can disagree if
                    // a row changed after the last view update.
                    self.error = Some(match submission.kind {
                        CredentialKind::Mnemonic => "Enter your recovery phrase.".into(),
                        CredentialKind::Wif => "Enter the private key.".into(),
                        CredentialKind::ExtendedKey => "Enter the extended key.".into(),
                        CredentialKind::Descriptor => "Enter the descriptor.".into(),
                    });
                    return;
                }
                if submission.password.0.len() < 8 {
                    self.error = Some("Use a password of at least 8 characters.".into());
                    return;
                }
                if *submission.password.0 != *submission.confirm.0 {
                    self.error = Some("The two passwords do not match.".into());
                    return;
                }

                let birthday = self.birthday_for(submission.birthday_index);

                self.busy = true;
                self.error = None;

                // A new wallet directory, so importing never disturbs an
                // existing wallet.
                let paths = Paths::for_wallet(&Paths::new_id());
                let created_paths = paths.clone();
                let kind = submission.kind;
                let credential = submission.credential.0.trim().to_owned();
                let bip39 = submission.bip39_passphrase.0.clone();
                let password = submission.password.0.clone();

                // Argon2 and up to four database creations all block.
                sender.spawn_oneshot_command(move || {
                    let bip39 = (!bip39.is_empty()).then(|| bip39.to_string());
                    let result = match kind {
                        CredentialKind::Mnemonic => wallet::create(
                            &credential,
                            password.as_bytes(),
                            &paths,
                            crate::vault::KdfParams::default(),
                            network,
                            birthday,
                            &ScriptType::ALL,
                            ScriptType::NativeSegwit,
                            bip39.as_deref(),
                            None,
                        ),
                        CredentialKind::ExtendedKey => wallet::import_xprv(
                            &credential,
                            password.as_bytes(),
                            &paths,
                            crate::vault::KdfParams::default(),
                            network,
                            birthday,
                            &ScriptType::ALL,
                            ScriptType::NativeSegwit,
                            None,
                        ),
                        CredentialKind::Wif => wallet::import_wif(
                            &credential,
                            password.as_bytes(),
                            &paths,
                            crate::vault::KdfParams::default(),
                            network,
                            birthday,
                            &ScriptType::ALL,
                            ScriptType::NativeSegwit,
                            None,
                        ),
                        CredentialKind::Descriptor => Err(anyhow::anyhow!(
                            "Descriptor import is not wired up yet."
                        )),
                    };
                    RestoreCmd::Finished(
                        result
                            .map(|summary| (created_paths, summary))
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
        let RestoreCmd::Finished(result) = msg;
        self.busy = false;
        match result {
            Ok((paths, summary)) => {
                let _ = sender.output(RestoreOutput::Imported { paths, summary });
            }
            Err(message) => self.error = Some(message),
        }
    }
}
