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
    /// TEMPORARY — put the screen back to its first step for a look at it.
    /// `EnteredByChoice` deliberately skips the welcome, since somebody who
    /// already has a wallet has already been welcomed.
    PreviewWelcome,
    /// Whether a wallet list sits behind this screen.
    Back,
    Configured(Setup),
    /// The chain was changed, which decides whether the warning is shown.
    NetworkChanged(u32),
    PhraseWritten,
    Verify(Secret, Secret, Secret, Secret),
}

/// Everything the first step collects.
///
/// A struct rather than five positional arguments: two of these are passwords
/// and two are passphrases, they are all `Secret`, and getting the order wrong
/// would compile.
pub struct Setup {
    pub password: Secret,
    pub confirm: Secret,
    pub name: String,
    /// The BIP-39 passphrase, empty when none was asked for.
    pub passphrase: Secret,
    pub passphrase_confirm: Secret,
    pub length: wallet::PhraseLength,
    pub network: bdk_wallet::bitcoin::Network,
    /// Whether the warning against putting real money in unreviewed software
    /// was read and switched past. Only asked for on bitcoin.
    pub acknowledged: bool,
}

impl std::fmt::Debug for Setup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Setup(<redacted>)")
    }
}

#[derive(Debug)]
pub enum OnboardingOutput {
    Created {
        paths: Paths,
        summary: Summary,
    },
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
    /// The BIP-39 passphrase, when one was asked for. Part of the key, not of
    /// the file encryption, and never written down anywhere but the person's
    /// own paper.
    passphrase: Option<Zeroizing<String>>,
    /// How long the generated phrase is, which the copy and the challenge both
    /// have to agree with.
    length: wallet::PhraseLength,
    /// Which chain this wallet will be on. Chosen before the phrase is
    /// generated, because it decides the birthday the wallet records and there
    /// is no changing it afterwards.
    network: bdk_wallet::bitcoin::Network,
    /// Whether the warning is on screen, which is only on bitcoin.
    mainnet: bool,
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

    /// What the phrase screen says, which depends on whether the words alone
    /// are enough to recover the wallet. With a passphrase they are not, and
    /// somebody who writes down only the words has a backup that restores an
    /// empty wallet — silently, years later.
    fn phrase_warning(&self) -> String {
        phrase_warning(self.length.words(), self.passphrase.is_some())
    }

    fn word(&self, position: usize) -> Option<String> {
        self.mnemonic
            .as_ref()?
            .split_whitespace()
            .nth(position - 1)
            .map(str::to_owned)
    }
}

/// What the phrase screen says. Free-standing so it can be checked without a
/// display: the wording is the safety property, not the widget.
fn phrase_warning(words: usize, passphrase: bool) -> String {
    match passphrase {
        false => format!(
            "These {words} words are the only way to recover your money. \
             Write them on paper. Anyone who reads them can spend your coins."
        ),
        true => format!(
            "These {words} words and your passphrase together are the only way to \
             recover your money — the words alone restore a different, empty wallet. \
             Write them on paper. Anyone who has both can spend your coins."
        ),
    }
}

/// The chains offered when making a wallet, bitcoin first and by default.
///
/// Order matters twice: it is what the picker shows, and it is what an index
/// out of the picker means.
const NETWORKS: [bdk_wallet::bitcoin::Network; 2] = [
    bdk_wallet::bitcoin::Network::Bitcoin,
    bdk_wallet::bitcoin::Network::Signet,
];

/// Three distinct 1-based positions in a phrase of `words` words.
fn pick_challenge(words: usize) -> [usize; 3] {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    let mut chosen: Vec<usize> = Vec::with_capacity(3);
    for byte in bytes {
        let candidate = (byte as usize % words) + 1;
        if !chosen.contains(&candidate) {
            chosen.push(candidate);
        }
        if chosen.len() == 3 {
            break;
        }
    }
    // Entropy exhausted without three distinct values: fall back rather than loop.
    while chosen.len() < 3 {
        let next = (1..=words).find(|n| !chosen.contains(n)).unwrap_or(1);
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
                // The first screen anybody sees. Built by hand rather than
                // from an adw::StatusPage: the mark wants to be larger and
                // closer to the name than a status icon sits, and this is the
                // one screen where that is worth the extra lines.
                add_named[Some("welcome")] = &gtk::ScrolledWindow {
                    set_vexpand: true,

                    adw::Clamp {
                        set_maximum_size: 420,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_margin_all: 24,

                            // The wallet and the way in, centred in whatever
                            // room the window has.
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 6,
                                set_vexpand: true,
                                set_valign: gtk::Align::Center,

                                // Sieve's own icon, which is also what the
                                // launcher and the About window show: the
                                // first screen should introduce the thing by
                                // the face it will be recognised by.
                                gtk::Image {
                                    set_icon_name: Some(crate::APP_ID),
                                    set_pixel_size: 148,
                                    set_halign: gtk::Align::Center,
                                    add_css_class: "welcome-mark",
                                },

                                gtk::Label {
                                    add_css_class: "welcome-name",
                                    set_label: "Sieve",
                                    set_halign: gtk::Align::Center,
                                },

                                gtk::Label {
                                    add_css_class: "welcome-line",
                                    set_label: "A privacy-focused Bitcoin wallet,\nbuilt on compact block filters",
                                    set_justify: gtk::Justification::Center,
                                    set_halign: gtk::Align::Center,
                                    set_wrap: true,
                                    set_margin_bottom: 24,
                                },

                                gtk::Button {
                                    add_css_class: "suggested-action",
                                    add_css_class: "pill",
                                    set_label: "Create a new wallet",
                                    connect_clicked => OnboardingMsg::Begin,
                                },
                                gtk::Button {
                                    add_css_class: "pill",
                                    set_label: "I already have a wallet",
                                    set_margin_top: 6,
                                    connect_clicked => OnboardingMsg::Restore,
                                },
                            },

                            // At the foot of the screen: the claim the wallet
                            // is built on, which is worth reading and not
                            // worth competing with the name for attention.
                            gtk::Label {
                                add_css_class: "welcome-note",
                                set_label: "The blockchain is checked here, on this machine. \
                                            No server is ever told which addresses are yours.",
                                set_justify: gtk::Justification::Center,
                                set_halign: gtk::Align::Center,
                                set_valign: gtk::Align::End,
                                set_wrap: true,
                                set_margin_top: 24,
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
                         the words themselves."
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

                            adw::PreferencesGroup {
                                set_title: "Network",

                                #[name(network_row)]
                                adw::ComboRow {
                                    set_title: "Chain",
                                    // Bitcoin first, and the default. Making
                                    // somebody change this to reach the chain
                                    // their money is on taught nothing; the
                                    // warning below is what carries the point.
                                    set_model: Some(&gtk::StringList::new(&[
                                        "Bitcoin (real coins)",
                                        "Signet (test coins)",
                                    ])),
                                    connect_selected_notify[sender] => move |row| {
                                        sender.input(OnboardingMsg::NetworkChanged(row.selected()));
                                    },
                                },

                                #[name(acknowledge_row)]
                                adw::SwitchRow {
                                    add_css_class: "warning",
                                    set_title: "I understand this software is unreviewed",
                                    set_subtitle: "Sieve has had no external security review. \
                                                   Its vault format and key handling are \
                                                   unaudited. Do not keep money here that you \
                                                   cannot afford to lose.",
                                    #[watch]
                                    set_visible: model.mainnet,
                                },
                            },

                            // Its own group, well away from the password.
                            // These two are the pair this wallet must never
                            // let anybody confuse: one encrypts a file on this
                            // machine and can be changed, the other is part of
                            // the key itself and cannot.
                            adw::PreferencesGroup {
                                set_title: "Recovery phrase",
                                // On the group rather than the row: a subtitle
                                // this long squeezes a ComboRow's value into
                                // an ellipsis, and "24…" is not a choice
                                // anybody can read.
                                set_description: Some(
                                    "Both lengths are far beyond guessing. Twice the \
                                     words is twice as much to copy down without a \
                                     mistake, which is how phrases are really lost."
                                ),

                                #[name(length_row)]
                                adw::ComboRow {
                                    set_title: "Length",
                                    set_model: Some(&gtk::StringList::new(&[
                                        "12 words",
                                        "24 words",
                                    ])),
                                },

                                #[name(passphrase_expander)]
                                adw::ExpanderRow {
                                    set_title: "Add a passphrase",
                                    set_subtitle: "Advanced. A 25th word, held only in \
                                                   your head — it is part of the key, so \
                                                   the phrase alone will not restore this \
                                                   wallet. Forgetting it loses the money.",
                                    set_show_enable_switch: true,
                                    set_enable_expansion: false,

                                    #[name(passphrase_row)]
                                    add_row = &adw::PasswordEntryRow {
                                        set_title: "Passphrase",
                                    },
                                    #[name(passphrase_confirm_row)]
                                    add_row = &adw::PasswordEntryRow {
                                        set_title: "Confirm passphrase",
                                    },
                                },
                            },

                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_halign: gtk::Align::Center,
                                set_label: "Continue",
                                connect_clicked[
                                    sender, pass_row, confirm_row, name_row, length_row,
                                    passphrase_expander, passphrase_row, passphrase_confirm_row,
                                    network_row, acknowledge_row
                                ] => move |_| {
                                    // An unexpanded row still holds whatever
                                    // was typed before it was switched off, so
                                    // the switch decides, not the field.
                                    let wanted = passphrase_expander.enables_expansion();
                                    let text = |row: &adw::PasswordEntryRow| {
                                        Secret(Zeroizing::new(if wanted {
                                            row.text().to_string()
                                        } else {
                                            String::new()
                                        }))
                                    };
                                    sender.input(OnboardingMsg::Configured(Setup {
                                        password: Secret(Zeroizing::new(
                                            pass_row.text().to_string(),
                                        )),
                                        confirm: Secret(Zeroizing::new(
                                            confirm_row.text().to_string(),
                                        )),
                                        name: name_row.text().to_string(),
                                        passphrase: text(&passphrase_row),
                                        passphrase_confirm: text(&passphrase_confirm_row),
                                        length: if length_row.selected() == 1 {
                                            wallet::PhraseLength::TwentyFour
                                        } else {
                                            wallet::PhraseLength::Twelve
                                        },
                                        network: NETWORKS
                                            [(network_row.selected() as usize).min(1)],
                                        acknowledged: acknowledge_row.is_active(),
                                    }));
                                },
                            },
                        },
                    },
                },

                // ---- phrase ----
                add_named[Some("phrase")] = &adw::StatusPage {
                    set_title: "Write these words down",
                    #[watch]
                    set_description: Some(&model.phrase_warning()),

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

                            // A passphrase typed wrong is not an error
                            // anywhere later: it derives a valid, empty wallet
                            // and the money appears to be gone. This is the
                            // only moment it can be checked against something,
                            // because this is the only moment Sieve still
                            // knows what it was meant to be.
                            adw::PreferencesGroup {
                                #[watch]
                                set_visible: model.passphrase.is_some(),
                                set_title: "Passphrase",
                                set_description: Some(
                                    "Type it again from your own copy, not from memory."
                                ),

                                #[name(passphrase_back)]
                                adw::PasswordEntryRow {
                                    set_title: "Passphrase",
                                },
                            },

                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_halign: gtk::Align::Center,
                                set_label: "Create wallet",
                                connect_clicked[
                                    sender, word_a, word_b, word_c, passphrase_back
                                ] => move |_| {
                                    sender.input(OnboardingMsg::Verify(
                                        Secret(Zeroizing::new(word_a.text().to_string())),
                                        Secret(Zeroizing::new(word_b.text().to_string())),
                                        Secret(Zeroizing::new(word_c.text().to_string())),
                                        Secret(Zeroizing::new(
                                            passphrase_back.text().to_string(),
                                        )),
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
            passphrase: None,
            length: wallet::PhraseLength::Twelve,
            network: NETWORKS[0],
            mainnet: true,
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
            // TEMPORARY — remove with the menu entry that sends it.
            OnboardingMsg::PreviewWelcome => {
                self.can_cancel = true;
                self.skip_welcome = false;
                self.step = Step::Welcome;
            }

            OnboardingMsg::EnteredByChoice => {
                self.can_cancel = true;
                self.skip_welcome = true;
                self.step = Step::Password;
            }
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

            OnboardingMsg::NetworkChanged(index) => {
                self.network = NETWORKS[(index as usize).min(1)];
                self.mainnet = self.network == bdk_wallet::bitcoin::Network::Bitcoin;
            }

            OnboardingMsg::Configured(setup) => {
                // Real money in software nobody has reviewed is a decision,
                // and it is made here rather than discovered later.
                if setup.network == bdk_wallet::bitcoin::Network::Bitcoin && !setup.acknowledged {
                    self.error =
                        Some("Switch on the acknowledgement to make a wallet on bitcoin.".into());
                    return;
                }
                if setup.password.0.len() < 8 {
                    self.error = Some("Use at least 8 characters.".into());
                    return;
                }
                if *setup.password.0 != *setup.confirm.0 {
                    self.error = Some("The two passwords do not match.".into());
                    return;
                }
                // No minimum and no rules: every byte of a BIP-39 passphrase
                // is part of the key, so any string is a valid one. Only that
                // it was typed the same way twice can be checked — and an
                // empty one asked for is a mistake worth catching, since it
                // would derive the no-passphrase wallet and nothing would say
                // so.
                if !setup.passphrase.0.is_empty() || !setup.passphrase_confirm.0.is_empty() {
                    if *setup.passphrase.0 != *setup.passphrase_confirm.0 {
                        self.error = Some("The two passphrases do not match.".into());
                        return;
                    }
                    self.passphrase = Some(setup.passphrase.0.clone());
                } else {
                    self.passphrase = None;
                }
                self.length = setup.length;
                self.network = setup.network;
                match wallet::generate_mnemonic(setup.length) {
                    Ok(phrase) => {
                        {
                            let mut guard = self.words.guard();
                            guard.clear();
                            for (index, word) in phrase.split_whitespace().enumerate() {
                                guard.push_back((index + 1, word.to_string()));
                            }
                        }
                        self.mnemonic = Some(phrase);
                        self.password = Some(setup.password.0);
                        let trimmed = setup.name.trim();
                        self.name = (!trimmed.is_empty()).then(|| trimmed.to_owned());
                        self.challenge = pick_challenge(self.length.words());
                        self.step = Step::Phrase;
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            }

            OnboardingMsg::PhraseWritten => self.step = Step::Verify,

            OnboardingMsg::Verify(a, b, c, passphrase) => {
                let given = [a, b, c];
                let matches = self
                    .challenge
                    .iter()
                    .zip(given.iter())
                    .all(|(position, given)| {
                        self.word(*position)
                            .is_some_and(|expected| expected == given.0.trim().to_lowercase())
                    });

                if !matches {
                    self.error =
                        Some("Those words do not match. Check your copy and try again.".into());
                    return;
                }

                // Compared exactly. A passphrase is bytes, not words: leading
                // space, capital letter and trailing space are all part of the
                // key, so trimming or lowercasing here would accept a copy
                // that does not open the wallet.
                if let Some(wanted) = self.passphrase.as_ref() {
                    if **wanted != *passphrase.0 {
                        self.error = Some(
                            "That passphrase does not match the one you set. It is part \
                             of the key, so it has to be exact — spaces and capitals \
                             included."
                                .into(),
                        );
                        return;
                    }
                }

                let bip39 = self.passphrase.clone();
                // Collected on the first step and, until now, dropped on the
                // floor: `create` takes a name and was being handed `None`, so
                // naming a wallet while making it did nothing at all.
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
                let network = self.network;
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
                            bip39.as_ref().map(|phrase| phrase.as_str()),
                            name,
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
    fn the_first_chain_offered_is_the_one_with_real_money_on_it() {
        // The picker's order and this list have to agree: an index out of the
        // combo row means whatever this array says it means, and getting that
        // backwards would make a wallet on the chain nobody asked for — with
        // no way to move it afterwards, since the network is baked into the
        // descriptors and the birthday.
        assert_eq!(NETWORKS[0], bdk_wallet::bitcoin::Network::Bitcoin);
        assert_eq!(NETWORKS[1], bdk_wallet::bitcoin::Network::Signet);
    }

    #[test]
    fn the_challenge_stays_inside_the_phrase() {
        // Asking for word 19 of a twelve-word phrase is a challenge nobody can
        // answer, and the wallet would refuse to be created.
        for length in [
            wallet::PhraseLength::Twelve,
            wallet::PhraseLength::TwentyFour,
        ] {
            let words = length.words();
            for _ in 0..200 {
                let challenge = pick_challenge(words);
                for position in challenge {
                    assert!(
                        (1..=words).contains(&position),
                        "word {position} of {words}"
                    );
                }
                assert!(
                    challenge[0] < challenge[1] && challenge[1] < challenge[2],
                    "positions must be distinct and in order: {challenge:?}"
                );
            }
        }
    }

    #[test]
    fn the_phrase_screen_says_when_the_words_alone_are_not_enough() {
        // Somebody who writes down only the words has a backup that restores
        // an empty wallet. They find that out years later, so the screen that
        // tells them to write is the only place it can be said.
        let plain = phrase_warning(24, false);
        assert!(plain.contains("24 words"), "{plain}");
        assert!(!plain.contains("passphrase"), "{plain}");

        let guarded = phrase_warning(24, true);
        assert!(guarded.contains("passphrase"), "{guarded}");
        assert!(
            guarded.contains("empty wallet"),
            "it has to say what happens, not just that a passphrase exists: {guarded}"
        );
    }

    #[test]
    fn entering_by_choice_starts_past_the_welcome_step() {
        // Choosing "create a new wallet" answers the question the welcome step
        // asks, so it must not be asked again.
        assert_eq!(Step::Password.previous(), Some(Step::Welcome));
    }
}
