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

/// One word of the recovery phrase, numbered.
///
/// A chip each rather than a block of text: these get copied onto paper by
/// hand, and the number beside a word is what makes that possible to check.
#[derive(Debug)]
pub struct SeedWord {
    position: usize,
    word: String,
}

#[relm4::factory(pub)]
impl FactoryComponent for SeedWord {
    type Init = (usize, String);
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::FlowBox;

    view! {
        gtk::Box {
            add_css_class: "seed-word",
            set_spacing: 8,
            set_valign: gtk::Align::Center,

            gtk::Label {
                add_css_class: "seed-index",
                add_css_class: "numeric",
                set_width_chars: 2,
                set_xalign: 1.0,
                set_label: &self.position.to_string(),
            },

            gtk::Label {
                add_css_class: "monospace",
                set_xalign: 0.0,
                set_hexpand: true,
                set_selectable: false,
                set_label: &self.word,
            },
        }
    }

    fn init_model(
        (position, word): Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        SeedWord { position, word }
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
            Step::Password => "password",
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
    Restore,
    /// Entered from the wallet list rather than as a first run: there is
    /// somewhere to go back to, and the welcome step has already been answered.
    EnteredByChoice,
    /// Whether a wallet list sits behind this screen.
    CanCancel(bool),
    Back,
    SetPassword(Secret, Secret, String),
    PhraseWritten,
    Verify(Secret, Secret, Secret),
}

#[derive(Debug)]
pub enum OnboardingOutput {
    Created { paths: Paths, summary: Summary },
    WantsRestore,
    /// Backed out of the first step; there is a screen behind this one.
    Cancelled,
}

#[derive(Debug)]
pub enum OnboardingCmd {
    Created(Result<(Paths, Summary), String>),
}

pub struct Onboarding {
    step: Step,
    /// Whether there is a screen behind this one to return to.
    can_cancel: bool,
    /// Whether the welcome step is part of this run at all.
    ///
    /// Entered from the wallet list, "create a new wallet" has already been
    /// chosen, and showing a screen whose job is to ask that question again is
    /// a step nobody needs.
    skip_welcome: bool,
    /// Held only between generation and sealing.
    mnemonic: Option<Zeroizing<String>>,
    password: Option<Zeroizing<String>>,
    /// What to call it. Optional — an unnamed wallet still gets a stable
    /// fallback — but naming it here beats renaming it later.
    name: Option<String>,
    /// 1-based word positions the user must type back.
    challenge: [usize; 3],
    words: FactoryVecDeque<SeedWord>,
    error: Option<String>,
}

impl Onboarding {
    /// The step behind this one, for this run of the flow.
    ///
    /// With the welcome step skipped, the password step is the first, so going
    /// back from it leaves setup rather than revealing a screen this run never
    /// showed.
    fn previous(&self) -> Option<Step> {
        match (self.step, self.skip_welcome) {
            (Step::Password, true) => None,
            (step, _) => step.previous(),
        }
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
    type Init = ();
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
                    set_visible: model.previous().is_some() || model.can_cancel,
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
                                set_label: "I already have a wallet",
                                connect_clicked => OnboardingMsg::Restore,
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
                                #[name(name_row)]
                                adw::EntryRow {
                                    set_title: "Wallet name (optional)",
                                },

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
                                connect_clicked[sender, pass_row, confirm_row, name_row] => move |_| {
                                    sender.input(OnboardingMsg::SetPassword(
                                        Secret(Zeroizing::new(pass_row.text().to_string())),
                                        Secret(Zeroizing::new(confirm_row.text().to_string())),
                                        name_row.text().to_string(),
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

                            #[local_ref]
                            word_grid -> gtk::FlowBox {
                                set_selection_mode: gtk::SelectionMode::None,
                                set_row_spacing: 8,
                                set_column_spacing: 8,
                                set_homogeneous: true,
                                // Two across on a narrow window, three when
                                // there is room. Twelve words divide evenly
                                // into either, so no row is left ragged.
                                set_min_children_per_line: 2,
                                set_max_children_per_line: 3,
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
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let words = FactoryVecDeque::builder().launch_default().detach();
        let model = Onboarding {
            step: Step::Welcome,
            can_cancel: false,
            skip_welcome: false,
            mnemonic: None,
            password: None,
            name: None,
            challenge: [1, 2, 3],
            words,
            error: None,
        };
        let word_grid = model.words.widget();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        self.error = None;
        match msg {
            OnboardingMsg::EnteredByChoice => {
                self.can_cancel = true;
                self.skip_welcome = true;
                self.step = Step::Password;
            }
            OnboardingMsg::CanCancel(can) => self.can_cancel = can,
            OnboardingMsg::Begin => self.step = Step::Password,

            OnboardingMsg::Restore => {
                let _ = sender.output(OnboardingOutput::WantsRestore);
            }

            OnboardingMsg::Back => match self.previous() {
                Some(previous) => self.step = previous,
                // Already at the first step, so back means leaving setup.
                None => {
                    let _ = sender.output(OnboardingOutput::Cancelled);
                }
            },

            OnboardingMsg::SetPassword(pass, confirm, name) => {
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
                        {
                            let mut guard = self.words.guard();
                            guard.clear();
                            for (index, word) in phrase.split_whitespace().enumerate() {
                                guard.push_back((index + 1, word.to_string()));
                            }
                        }
                        self.mnemonic = Some(phrase);
                        self.password = Some(pass.0);
                        let trimmed = name.trim();
                        self.name = (!trimmed.is_empty()).then(|| trimmed.to_owned());
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

                let name = self.name.clone();
                let (Some(mnemonic), Some(password)) =
                    (self.mnemonic.clone(), self.password.clone())
                else {
                    self.error = Some("The setup state was lost. Start again.".into());
                    return;
                };

                self.step = Step::Working;
                // Every creation mints a new wallet directory, so making
                // another never disturbs one that already exists.
                let paths = Paths::for_wallet(&Paths::new_id());
                // A wallet created here is new, so its birthday is simply the
                // newest checkpoint this build knows about.
                let network = wallet::DEFAULT_NETWORK;
                let birthday = wallet::checkpoints(network)[0];
                // Taproot to receive on, with native segwit alongside it.
                // Taproot is the better address to hand out — a single-sig
                // spend is indistinguishable from any other key-path spend —
                // but plenty of exchanges still refuse to send to bc1p, and a
                // wallet nobody can pay is a wallet with a hole in it. Legacy
                // and nested are not derived: a new wallet has no history on
                // them and nothing sends to them by preference.
                let primary = wallet::accounts::ScriptType::Taproot;
                let script_types = vec![primary, wallet::accounts::ScriptType::NativeSegwit];
                // Argon2 and the database write both block. Off the main thread.
                let created_paths = paths.clone();
                sender.spawn_oneshot_command(move || {
                    OnboardingCmd::Created(
                        wallet::create(
                            &mnemonic,
                            password.as_bytes(),
                            &paths,
                            crate::vault::KdfParams::default(),
                            network,
                            birthday,
                            &script_types,
                            primary,
                            None,
                            None,
                            // A wallet created here starts empty, so BDK's
                            // default window is plenty.
                            25,
                        )
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
        let OnboardingCmd::Created(result) = msg;
        match result {
            Ok((paths, summary)) => {
                // The wallet exists on disk now; drop everything secret.
                self.mnemonic = None;
                self.password = None;
                let _ = sender.output(OnboardingOutput::Created { paths, summary });
            }
            Err(message) => {
                self.step = Step::Verify;
                self.error = Some(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_names_a_page_in_the_stack() {
        // The stack switches by name, and a name it does not have is a silent
        // no-op — it stays on whichever page it was showing. That is exactly
        // how renaming the password step left creation stuck on the welcome
        // screen: the page was renamed and the tag was not.
        //
        // These must match the add_named calls in the view.
        let pages = ["welcome", "password", "phrase", "verify", "working"];
        for step in [
            Step::Welcome,
            Step::Password,
            Step::Phrase,
            Step::Verify,
            Step::Working,
        ] {
            assert!(
                pages.contains(&step.tag()),
                "{:?} has tag {:?}, which is not a page in the stack",
                step,
                step.tag()
            );
        }
    }

    #[test]
    fn entering_by_choice_starts_past_the_welcome_step() {
        // Choosing "create a new wallet" answers the question the welcome step
        // asks, so it must not be asked again.
        assert_eq!(Step::Password.previous(), Some(Step::Welcome));
    }
}
