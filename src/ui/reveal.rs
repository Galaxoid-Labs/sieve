//! Showing the recovery phrase again.
//!
//! The phrase is displayed once when a wallet is created, and the moment
//! someone is most likely to put off copying twelve words down is exactly that
//! one. This is the way back to it.
//!
//! The vault is the only place the phrase exists, so revealing it means
//! decrypting the vault, which means the password. That is not a gate bolted
//! on for ceremony — there is no other way to get the words.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};
use zeroize::Zeroizing;

use super::onboarding::SeedWord;
use crate::wallet::Paths;

/// The wallet password, with a redacted `Debug`.
pub struct Password(Zeroizing<String>);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// What the vault turned out to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// A recovery phrase, shown as numbered chips.
    Phrase,
    /// Something else this wallet was imported from — a key, an extended key.
    /// Shown as it is, because that is what would have to be written down.
    Secret,
}

impl Held {
    /// What the vault turned out to hold.
    ///
    /// Nothing on disk records which kind of credential a wallet was made
    /// from, so it is read off the plaintext. A BIP-39 phrase is a valid word
    /// count of nothing but letters; a key or a descriptor is neither.
    fn of(secret: &str) -> Self {
        let words = secret.split_whitespace().count();
        let letters_only = secret
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c.is_whitespace());
        if matches!(words, 12 | 15 | 18 | 21 | 24) && letters_only {
            Held::Phrase
        } else {
            Held::Secret
        }
    }
}

#[derive(Debug)]
pub enum RevealMsg {
    /// Entering the screen: forget anything from last time.
    Prepare(Box<Paths>),
    Submit(Password),
    /// Leaving: drop the phrase rather than leave it in memory behind a
    /// screen nobody is looking at.
    Clear,
}

#[derive(Debug)]
pub enum RevealCmd {
    Opened(Result<String, String>),
}

pub struct Reveal {
    paths: Option<Paths>,
    words: FactoryVecDeque<SeedWord>,
    /// The credential itself, held only while it is on screen.
    shown: Option<Zeroizing<String>>,
    held: Held,
    busy: bool,
    error: Option<String>,
}

impl Reveal {
    fn revealed(&self) -> bool {
        self.shown.is_some()
    }

    /// What is on screen, when it is not a phrase.
    fn secret_text(&self) -> String {
        self.shown.as_deref().cloned().unwrap_or_default()
    }
}

#[relm4::component(pub)]
impl Component for Reveal {
    type Init = ();
    type Input = RevealMsg;
    type Output = ();
    type CommandOutput = RevealCmd;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Recovery phrase",
                },
            },

            #[wrap(Some)]
            set_content = &gtk::ScrolledWindow {
                adw::Clamp {
                    set_maximum_size: 460,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 18,
                        set_margin_all: 18,
                        set_valign: gtk::Align::Center,

                        // Asking.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            #[watch]
                            set_visible: !model.revealed(),

                            gtk::Label {
                                add_css_class: "dim-label",
                                set_wrap: true,
                                set_justify: gtk::Justification::Center,
                                set_label: "Anyone who reads these words can spend this \
                                            wallet's coins. Make sure nobody is looking.",
                            },

                            adw::PreferencesGroup {
                                #[name(password_row)]
                                adw::PasswordEntryRow {
                                    set_title: "Password",
                                    #[watch]
                                    set_sensitive: !model.busy,
                                    connect_entry_activated[sender] => move |row| {
                                        sender.input(RevealMsg::Submit(
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
                                set_label: if model.busy { "Unlocking…" } else { "Show phrase" },
                                connect_clicked[sender, password_row] => move |_| {
                                    sender.input(RevealMsg::Submit(
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

                        // Showing.
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 18,
                            #[watch]
                            set_visible: model.revealed(),

                            #[local_ref]
                            word_grid -> gtk::FlowBox {
                                set_selection_mode: gtk::SelectionMode::None,
                                set_row_spacing: 8,
                                set_column_spacing: 8,
                                set_homogeneous: true,
                                set_min_children_per_line: 2,
                                set_max_children_per_line: 3,
                                #[watch]
                                set_visible: model.held == Held::Phrase,
                            },

                            // Not every wallet was made from a phrase. One
                            // imported from a key has that key in the vault
                            // instead, and that is what would have to be
                            // written down.
                            gtk::Label {
                                add_css_class: "monospace",
                                set_wrap: true,
                                set_wrap_mode: gtk::pango::WrapMode::WordChar,
                                set_selectable: true,
                                set_justify: gtk::Justification::Center,
                                #[watch]
                                set_visible: model.held == Held::Secret,
                                #[watch]
                                set_label: &model.secret_text(),
                            },

                            gtk::Label {
                                add_css_class: "dim-label",
                                set_wrap: true,
                                set_justify: gtk::Justification::Center,
                                #[watch]
                                set_label: if model.held == Held::Phrase {
                                    "Write these down on paper and keep them somewhere safe. \
                                     They are the only way to recover this wallet."
                                } else {
                                    "This wallet was imported from a key rather than a \
                                     recovery phrase, so a key is what there is to show. \
                                     Keep a copy on paper: it is the only way to recover it."
                                },
                            },
                        },
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
        let words = FactoryVecDeque::builder().launch_default().detach();
        let model = Reveal {
            paths: None,
            words,
            shown: None,
            held: Held::Phrase,
            busy: false,
            error: None,
        };
        let word_grid = model.words.widget();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            RevealMsg::Prepare(paths) => {
                self.paths = Some(*paths);
                self.shown = None;
                self.error = None;
                self.busy = false;
                self.words.guard().clear();
            }

            RevealMsg::Clear => {
                self.shown = None;
                self.error = None;
                self.words.guard().clear();
            }

            RevealMsg::Submit(password) => {
                let Some(paths) = self.paths.clone() else {
                    return;
                };
                if self.busy {
                    return;
                }
                self.busy = true;
                self.error = None;

                // Argon2 again, so off the main thread again.
                sender.spawn_oneshot_command(move || {
                    let result = std::fs::read(&paths.vault)
                        .map_err(|e| format!("Cannot read the wallet file: {e}"))
                        .and_then(|blob| {
                            crate::vault::open(&blob, password.0.as_bytes())
                                .map_err(|e| e.to_string())
                        })
                        .and_then(|secret| {
                            String::from_utf8(secret.to_vec())
                                .map_err(|_| "The wallet file is not readable text.".to_string())
                        });
                    RevealCmd::Opened(result)
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        _sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        let RevealCmd::Opened(result) = msg;
        self.busy = false;

        match result {
            Ok(secret) => {
                // Twelve or twenty-four words is a phrase; anything else is
                // the key this wallet was imported from.
                self.held = Held::of(&secret);
                if self.held == Held::Phrase {
                    let mut guard = self.words.guard();
                    guard.clear();
                    for (index, word) in secret.split_whitespace().enumerate() {
                        guard.push_back((index + 1, word.to_string()));
                    }
                }
                self.shown = Some(Zeroizing::new(secret));
            }
            Err(message) => self.error = Some(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Held;

    #[test]
    fn a_phrase_is_read_as_a_phrase() {
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";
        assert_eq!(Held::of(phrase), Held::Phrase);
    }

    /// Keys and descriptors have no words to number, so they are shown as
    /// they are rather than sliced into meaningless chips.
    #[test]
    fn keys_and_descriptors_are_not_phrases() {
        for secret in [
            "L1aW4aubDFB7yfras2S1mMSt3gW4iyGqDbUtu6zXbAr9wYo9zqhV",
            "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi",
            "tr([73c5da0a/86h/0h/0h]xpub/0/*)#abcdefgh",
        ] {
            assert_eq!(Held::of(secret), Held::Secret, "{secret}");
        }
    }

    /// Twelve words of a descriptor are still not a phrase.
    #[test]
    fn twelve_tokens_that_are_not_words_are_not_a_phrase() {
        assert_eq!(Held::of(&"0/1 ".repeat(12)), Held::Secret);
    }
}
