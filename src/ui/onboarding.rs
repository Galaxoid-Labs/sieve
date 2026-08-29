//! First-run flow: generate a phrase, show it once, prove it was written down,
//! then seal the wallet.
//!
//! The pages live in a `gtk::Stack` with a Back button we drive ourselves,
//! which is the pattern GNOME Initial Setup uses — libadwaita has no wizard
//! widget, and `NavigationView`'s push/pop model fights a flow where advancing
//! depends on validation.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};
use zeroize::Zeroizing;

use crate::wallet::{self, Paths, Summary};

/// Secret string with a redacted `Debug`, so relm4's message tracing can never
/// print a password or a recovery phrase.
pub struct Secret(Zeroizing<String>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Password,
    Phrase,
    Verify,
    Working,
}

impl Step {
    fn tag(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Password => "passphrase",
            Step::Phrase => "phrase",
            Step::Verify => "verify",
            Step::Working => "working",
        }
    }

    fn previous(self) -> Option<Step> {
        match self {
            Step::Welcome | Step::Working => None,
            Step::Password => Some(Step::Welcome),
            Step::Phrase => Some(Step::Password),
            Step::Verify => Some(Step::Phrase),
        }
    }
}

#[derive(Debug)]
pub enum OnboardingMsg {
    Begin,
    Back,
    SetPassword(Secret, Secret),
    PhraseWritten,
    Verify(Secret, Secret, Secret),
}

#[derive(Debug)]
pub enum OnboardingOutput {
    Created(Summary),
}

#[derive(Debug)]
pub enum OnboardingCmd {
    Created(Result<Summary, String>),
}

pub struct Onboarding {
    paths: Paths,
    step: Step,
    /// Held only between generation and sealing.
    mnemonic: Option<Zeroizing<String>>,
    password: Option<Zeroizing<String>>,
    /// 1-based word positions the user must type back.
    challenge: [usize; 3],
    error: Option<String>,
}

impl Onboarding {
    /// The phrase as two numbered columns of monospace text.
    fn phrase_display(&self) -> String {
        let Some(phrase) = &self.mnemonic else {
            return String::new();
        };
        let words: Vec<&str> = phrase.split_whitespace().collect();
        let rows = words.len().div_ceil(2);
        (0..rows)
            .map(|row| {
                let left = format!("{:>2}. {:<10}", row + 1, words[row]);
                match words.get(row + rows) {
                    Some(word) => format!("{left}   {:>2}. {word}", row + rows + 1),
                    None => left.trim_end().to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn word(&self, position: usize) -> Option<String> {
        self.mnemonic
            .as_ref()?
            .split_whitespace()
            .nth(position - 1)
            .map(str::to_owned)
    }
}

/// Three distinct 1-based positions in a twelve-word phrase.
fn pick_challenge() -> [usize; 3] {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    let mut chosen: Vec<usize> = Vec::with_capacity(3);
    for byte in bytes {
        let candidate = (byte as usize % 12) + 1;
        if !chosen.contains(&candidate) {
            chosen.push(candidate);
        }
        if chosen.len() == 3 {
            break;
        }
    }
    // Entropy exhausted without three distinct values: fall back rather than loop.
    while chosen.len() < 3 {
        let next = (1..=12).find(|n| !chosen.contains(n)).unwrap_or(1);
        chosen.push(next);
    }
    chosen.sort_unstable();
    [chosen[0], chosen[1], chosen[2]]
}

#[relm4::component(pub)]
impl Component for Onboarding {
    type Init = Paths;
    type Input = OnboardingMsg;
    type Output = OnboardingOutput;
    type CommandOutput = OnboardingCmd;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Sieve",
                    #[watch]
                    set_subtitle: match model.step {
                        Step::Welcome => "Set up",
                        Step::Password => "Step 1 of 3",
                        Step::Phrase => "Step 2 of 3",
                        Step::Verify => "Step 3 of 3",
                        Step::Working => "Creating",
                    },
                },
                pack_start = &gtk::Button {
                    set_icon_name: "go-previous-symbolic",
                    set_tooltip_text: Some("Back"),
                    #[watch]
                    set_visible: model.step.previous().is_some(),
                    connect_clicked => OnboardingMsg::Back,
                },
            },

            #[wrap(Some)]
            set_content = &gtk::Stack {
                set_transition_type: gtk::StackTransitionType::SlideLeftRight,
                #[watch(skip_init)]
                set_visible_child_name: model.step.tag(),

                // ---- welcome ----
                add_named[Some("welcome")] = &adw::StatusPage {
                    set_icon_name: Some("channel-secure-symbolic"),
                    set_title: "Welcome to Sieve",
                    set_description: Some(
                        "Sieve checks the blockchain privately, on your own machine. \
                         No server is ever told which addresses belong to you."
                    ),

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 320,
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,

                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_label: "Create a new wallet",
                                connect_clicked => OnboardingMsg::Begin,
                            },
                            gtk::Button {
                                add_css_class: "pill",
                                set_label: "Restore from a recovery phrase",
                                // Lands with the restore path in the next pass.
                                set_sensitive: false,
                            },
                        },
                    },
                },

                // ---- passphrase ----
                add_named[Some("password")] = &adw::StatusPage {
                    set_title: "Choose a password",
                    set_description: Some(
                        "This locks the wallet on this computer. It is not part of your \
                         recovery phrase — if you forget it you can still restore from \
                         those twelve words."
                    ),

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 400,
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,

                            adw::PreferencesGroup {
                                #[name(pass_row)]
                                adw::PasswordEntryRow {
                                    set_title: "Password",
                                },
                                #[name(confirm_row)]
                                adw::PasswordEntryRow {
                                    set_title: "Confirm password",
                                },
                            },

                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_halign: gtk::Align::Center,
                                set_label: "Continue",
                                connect_clicked[sender, pass_row, confirm_row] => move |_| {
                                    sender.input(OnboardingMsg::SetPassword(
                                        Secret(Zeroizing::new(pass_row.text().to_string())),
                                        Secret(Zeroizing::new(confirm_row.text().to_string())),
                                    ));
                                },
                            },
                        },
                    },
                },

                // ---- phrase ----
                add_named[Some("phrase")] = &adw::StatusPage {
                    set_title: "Write these words down",
                    set_description: Some(
                        "These twelve words are the only way to recover your money. \
                         Write them on paper. Anyone who reads them can spend your coins."
                    ),

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 400,
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 18,

                            gtk::Label {
                                add_css_class: "card",
                                add_css_class: "monospace",
                                set_margin_all: 4,
                                set_selectable: false,
                                set_justify: gtk::Justification::Left,
                                set_xalign: 0.0,
                                #[watch]
                                set_label: &model.phrase_display(),
                            },

                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_halign: gtk::Align::Center,
                                set_label: "I have written them down",
                                connect_clicked => OnboardingMsg::PhraseWritten,
                            },
                        },
                    },
                },

                // ---- verify ----
                add_named[Some("verify")] = &adw::StatusPage {
                    set_title: "Check your copy",
                    #[watch]
                    set_description: Some(&format!(
                        "Type words {}, {} and {} from the phrase you just wrote down.",
                        model.challenge[0], model.challenge[1], model.challenge[2]
                    )),

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 400,
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,

                            adw::PreferencesGroup {
                                #[name(word_a)]
                                adw::EntryRow {
                                    #[watch]
                                    set_title: &format!("Word {}", model.challenge[0]),
                                },
                                #[name(word_b)]
                                adw::EntryRow {
                                    #[watch]
                                    set_title: &format!("Word {}", model.challenge[1]),
                                },
                                #[name(word_c)]
                                adw::EntryRow {
                                    #[watch]
                                    set_title: &format!("Word {}", model.challenge[2]),
                                },
                            },

                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_halign: gtk::Align::Center,
                                set_label: "Create wallet",
                                connect_clicked[sender, word_a, word_b, word_c] => move |_| {
                                    sender.input(OnboardingMsg::Verify(
                                        Secret(Zeroizing::new(word_a.text().to_string())),
                                        Secret(Zeroizing::new(word_b.text().to_string())),
                                        Secret(Zeroizing::new(word_c.text().to_string())),
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

                // ---- working ----
                add_named[Some("working")] = &adw::StatusPage {
                    set_title: "Creating your wallet",
                    set_description: Some("Encrypting the recovery phrase. This takes a moment."),

                    #[wrap(Some)]
                    set_child = &gtk::Spinner {
                        set_spinning: true,
                        set_halign: gtk::Align::Center,
                        set_size_request: (32, 32),
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
        let model = Onboarding {
            paths,
            step: Step::Welcome,
            mnemonic: None,
            password: None,
            challenge: [1, 2, 3],
            error: None,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        self.error = None;
        match msg {
            OnboardingMsg::Begin => self.step = Step::Password,

            OnboardingMsg::Back => {
                if let Some(previous) = self.step.previous() {
                    self.step = previous;
                }
            }

            OnboardingMsg::SetPassword(pass, confirm) => {
                if pass.0.len() < 8 {
                    self.error = Some("Use at least 8 characters.".into());
                    return;
                }
                if *pass.0 != *confirm.0 {
                    self.error = Some("The two passwords do not match.".into());
                    return;
                }
                match wallet::generate_mnemonic() {
                    Ok(phrase) => {
                        self.mnemonic = Some(phrase);
                        self.password = Some(pass.0);
                        self.challenge = pick_challenge();
                        self.step = Step::Phrase;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }

            OnboardingMsg::PhraseWritten => self.step = Step::Verify,

            OnboardingMsg::Verify(a, b, c) => {
                let given = [a, b, c];
                let matches = self.challenge.iter().zip(given.iter()).all(|(position, given)| {
                    self.word(*position)
                        .is_some_and(|expected| expected == given.0.trim().to_lowercase())
                });

                if !matches {
                    self.error =
                        Some("Those words do not match. Check your copy and try again.".into());
                    return;
                }

                let (Some(mnemonic), Some(password)) =
                    (self.mnemonic.clone(), self.password.clone())
                else {
                    self.error = Some("The setup state was lost. Start again.".into());
                    return;
                };

                self.step = Step::Working;
                let paths = self.paths.clone();
                // Argon2 and the database write both block. Off the main thread.
                sender.spawn_oneshot_command(move || {
                    OnboardingCmd::Created(
                        wallet::create(
                            &mnemonic,
                            password.as_bytes(),
                            &paths,
                            crate::vault::KdfParams::default(),
                        )
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
        let OnboardingCmd::Created(result) = msg;
        match result {
            Ok(summary) => {
                // The wallet exists on disk now; drop everything secret.
                self.mnemonic = None;
                self.password = None;
                let _ = sender.output(OnboardingOutput::Created(summary));
            }
            Err(message) => {
                self.step = Step::Verify;
                self.error = Some(message);
            }
        }
    }
}
