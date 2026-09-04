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
pub struct SeedWord {
    position: usize,
    word: String,
}

/// Hand-written for the reason `Face` and `Word` have one: this holds a word
/// of a recovery phrase, and a derived `Debug` would print it. Nothing routes
/// a `SeedWord` through a logged message today — its `Input` is `()` — so this
/// is a fence rather than a fix, put up because the type is one message
/// signature away from being the leak that `RevealCmd` was.
impl std::fmt::Debug for SeedWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeedWord")
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

/// One face of a die, on its way into the roll sequence.
///
/// A newtype with a redacted `Debug` rather than a bare `u32`, and it is not
/// paranoia: relm4 traces every message under `RUST_LOG=relm4=trace`, so a
/// derived `Debug` would write the entire roll sequence to the log one line at a
/// time. The rolls are a share of the seed until the phrase exists.
#[derive(Clone, Copy)]
pub struct Face(u32);

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A die being rolled, and what has come up on it so far.
struct DiceRolls {
    sides: u32,
    /// The sequence exactly as entered. Key material: `Zeroizing`, never
    /// printed, and dropped as soon as the phrase has been made from it.
    rolls: Zeroizing<String>,
}

impl std::fmt::Debug for DiceRolls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DiceRolls {{ sides: {}, rolls: <redacted> }}",
            self.sides
        )
    }
}

/// The dice picker's labels, in `wallet::DICE` order. A test keeps them aligned:
/// the index out of the picker is what chooses the die, so a list that drifts
/// from `DICE` would silently roll the wrong one.
const DIE_LABELS: [&str; 5] = [
    "6-sided (d6)",
    "8-sided (d8)",
    "10-sided (d10)",
    "12-sided (d12)",
    "20-sided (d20)",
];

/// Said under the roll grid, and both halves of it are load-bearing.
const DICE_NOTE: &str = "Do not write these rolls down — they cannot recreate \
                         your phrase on their own. And do not re-roll a run that \
                         looks wrong: runs are what randomness looks like.";

/// One face of the die, as a button on the roll screen.
#[derive(Debug)]
pub struct DieFace {
    value: u32,
}

#[relm4::factory(pub)]
impl FactoryComponent for DieFace {
    type Init = u32;
    type Input = ();
    type Output = Face;
    type CommandOutput = ();
    type ParentWidget = gtk::FlowBox;

    view! {
        gtk::Button {
            add_css_class: "die-face",
            add_css_class: "numeric",
            set_label: &self.value.to_string(),
            connect_clicked[sender, value = self.value] => move |_| {
                let _ = sender.output(Face(value));
            },
        }
    }

    fn init_model(value: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        DieFace { value }
    }
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
    /// Rolling a die for entropy of one's own. Only reached when that was asked
    /// for on the password step; the default flow goes straight to the phrase.
    Dice,
    Phrase,
    Verify,
    Working,
}

impl Step {
    fn tag(self) -> &'static str {
        match self {
            Step::Welcome => "welcome",
            Step::Password => "password",
            Step::Dice => "dice",
            Step::Phrase => "phrase",
            Step::Verify => "verify",
            Step::Working => "working",
        }
    }

    fn previous(self) -> Option<Step> {
        match self {
            Step::Welcome | Step::Working => None,
            Step::Password => Some(Step::Welcome),
            Step::Dice => Some(Step::Password),
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
    Back,
    Configured(Setup),
    /// A die came up on this face. Carries a `Face` rather than a number so the
    /// sequence cannot be reassembled from relm4's message trace.
    Roll(Face),
    /// Take back the last roll, for the one somebody presses wrong at roll 73.
    UndoRoll,
    /// Enough rolled: mix them in and make the phrase.
    RollsDone,
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
    /// Whether the switch asking for a passphrase is on. The switch decides and
    /// the field does not: an expander left on with nothing typed is refused,
    /// rather than silently deriving the wallet that has no passphrase.
    pub passphrase_wanted: bool,
    pub length: wallet::PhraseLength,
    pub network: bdk_wallet::bitcoin::Network,
    /// Whether the warning against putting real money in unreviewed software
    /// was read and switched past. Only asked for on bitcoin.
    pub acknowledged: bool,
    /// The die to roll for entropy of one's own, when that was asked for.
    /// `None` — the default — means the operating system supplies it alone.
    pub dice_sides: Option<u32>,
}

impl std::fmt::Debug for Setup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Setup(<redacted>)")
    }
}

#[derive(Debug)]
// The large variant is the one this enum exists for — a wallet was made —
// and it is sent once. Boxing it would trade a clearer type for a saving
// nobody can measure.
#[allow(clippy::large_enum_variant)]
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
    /// The die chosen and what has been rolled on it, when somebody is supplying
    /// entropy of their own. `None` is the default and the ordinary case.
    dice: Option<DiceRolls>,
    /// One button per face, rebuilt when a die is chosen.
    faces: FactoryVecDeque<DieFace>,
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
            // Back from the words returns to the rolls rather than to the form,
            // so one step backwards does not throw a hundred of them away.
            // Leaving the roll screen itself is what discards them, and that is
            // deliberate: they are key material, and the phrase they were about
            // to make is being abandoned.
            (Step::Phrase, _) if self.dice.is_some() => Some(Step::Dice),
            (step, _) => step.previous(),
        }
    }

    /// Which step of how many, since rolling a die inserts one.
    ///
    /// Counted off `dice`, which is only set once the form is submitted — so the
    /// password step says "of 3" while the switch is being considered and "of 4"
    /// only once it has been acted on. That is the honest reading: nothing is
    /// committed until Continue.
    fn step_label(&self) -> &'static str {
        step_label(self.step, self.dice.is_some())
    }

    /// How many rolls have been entered.
    fn rolls_entered(&self) -> usize {
        self.dice.as_ref().map_or(0, |d| d.rolls.chars().count())
    }

    /// How many this die needs to carry the phrase's own entropy.
    fn rolls_wanted(&self) -> u32 {
        self.dice
            .as_ref()
            .map_or(0, |d| wallet::rolls_needed(d.sides, self.length))
    }

    /// What the roll screen says above the grid.
    ///
    /// It states the guarantee and its limit in the same breath, because half of
    /// it is the half people get wrong: rolls are mixed in, so they can only
    /// add — and mixing is exactly why nothing here can prove they were used.
    fn dice_description(&self) -> String {
        let sides = self.dice.as_ref().map_or(6, |d| d.sides);
        format!(
            "Roll your {sides}-sided die and press the face that came up. These rolls are \
             mixed into the randomness this computer already provides, so they can only \
             add to it — never weaken it."
        )
    }

    /// `73 / 100`, and what that means once it is passed.
    fn roll_count_label(&self) -> String {
        let entered = self.rolls_entered();
        let wanted = self.rolls_wanted() as usize;
        match entered >= wanted {
            true => format!(
                "{entered} of {wanted} rolls — enough for a {}-word phrase",
                self.length.words()
            ),
            false => format!("{entered} of {wanted} rolls"),
        }
    }

    fn roll_fraction(&self) -> f64 {
        let wanted = self.rolls_wanted() as f64;
        match wanted > 0.0 {
            true => (self.rolls_entered() as f64 / wanted).min(1.0),
            false => 0.0,
        }
    }

    /// Whether enough has been rolled to carry the phrase's own entropy.
    fn rolls_complete(&self) -> bool {
        self.dice.is_some() && self.rolls_entered() >= self.rolls_wanted() as usize
    }

    /// The finish button, which says either what is left or what it will use.
    ///
    /// Held shut until the count is met. Under mixing a short session is not
    /// *unsafe* — the operating system's bytes are underneath it either way —
    /// but somebody who asked for a hundred rolls of their own entropy and
    /// stopped at thirty has most of what they came for missing, and nothing
    /// afterwards would ever tell them. The count is the only moment it can be
    /// said, so it is said by not opening the door.
    fn use_rolls_label(&self) -> String {
        match self.rolls_complete() {
            true => format!("Use these {} rolls", self.rolls_entered()),
            false => {
                let left = self.rolls_wanted() as usize - self.rolls_entered();
                match left {
                    1 => "One more roll".into(),
                    n => format!("{n} more rolls"),
                }
            }
        }
    }

    /// Make the phrase, mixing in the rolls when there are any, and move to the
    /// screen that shows it.
    ///
    /// One place rather than two, so the dice path and the ordinary path cannot
    /// come to differ in what they do with the result.
    fn make_phrase(&mut self) {
        let made = match &self.dice {
            Some(dice) => wallet::generate_mnemonic_with_rolls(self.length, &dice.rolls),
            None => wallet::generate_mnemonic(self.length),
        };
        match made {
            Ok(phrase) => {
                {
                    let mut guard = self.words.guard();
                    guard.clear();
                    for (index, word) in phrase.split_whitespace().enumerate() {
                        guard.push_back((index + 1, word.to_string()));
                    }
                }
                self.mnemonic = Some(phrase);
                self.challenge = pick_challenge(self.length.words());
                self.step = Step::Phrase;
            }
            Err(e) => self.error = Some(e.to_string()),
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

/// The shortest wallet password Sieve will seal a vault with.
///
/// One constant, read by both the Continue button and the handler behind it. Two
/// copies of this number would eventually disagree, and the failure would be a
/// button that refuses to light for a password the handler would have taken.
const MIN_PASSWORD: usize = 8;

/// A ceiling on the roll sequence, far past any honest session.
///
/// Not a limit anybody should meet: it exists so a key held down cannot grow the
/// string without end. Extra rolls are free — SHA-256 absorbs any length — so
/// there is no reason for this to be tight.
const MAX_ROLLS: usize = 1024;

/// The setup form reduced to what decides whether it is finished.
///
/// Booleans rather than the text itself, for two reasons: the rule can be checked
/// without a display, and no password has to leave the widget holding it to be
/// judged. `Debug` is derived here precisely because nothing in it is a secret —
/// which is only true because it holds answers rather than inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FormState {
    password_long_enough: bool,
    passwords_match: bool,
    mainnet: bool,
    acknowledged: bool,
    passphrase_wanted: bool,
    passphrase_typed: bool,
    passphrases_match: bool,
}

/// What still has to be done before a wallet can be made, or `None` when nothing
/// does.
///
/// Named in form order rather than in order of severity, because it is read as a
/// tooltip on a button somebody is looking at: the useful answer is the next thing
/// to do, and the next thing is the one nearest the top of the screen. A disabled
/// button that does not say why is worse than one that answers with an error.
fn what_is_missing(form: FormState) -> Option<&'static str> {
    if !form.password_long_enough {
        return Some("Use a password of at least 8 characters.");
    }
    if !form.passwords_match {
        return Some("The two passwords do not match.");
    }
    if form.mainnet && !form.acknowledged {
        return Some("Switch on the acknowledgement to make a wallet on bitcoin.");
    }
    // The switch is what asks for a passphrase, so the switch is what has to be
    // answered. Left on and empty this would seal the no-passphrase wallet, and
    // nothing afterwards would ever say that is what happened — BIP-39 derives a
    // different seed for "" than for absent, and both look like a working wallet.
    if form.passphrase_wanted && !form.passphrase_typed {
        return Some("Type the passphrase, or switch it off.");
    }
    if form.passphrase_wanted && !form.passphrases_match {
        return Some("The two passphrases do not match.");
    }
    None
}

/// Why that many rolls, said beside the number.
///
/// The count on its own is a demand without a reason, and the reason is the
/// whole point of the screen: the target is not a house rule, it is how many
/// throws of *this* die carry the bits *this* phrase length holds. Somebody who
/// can see that can also see why picking a d20 halves the work.
///
/// "At least" is exact rather than hedging: `rolls_needed` rounds up, so fifty
/// d6 rolls carry 129.2 bits and not 128 on the nose.
fn roll_target_note(sides: u32, length: wallet::PhraseLength) -> String {
    format!(
        "{} rolls — at least the {} bits a {}-word phrase carries",
        wallet::rolls_needed(sides, length),
        length.bits(),
        length.words()
    )
}

/// How many faces to put on a row, so the grid comes out a rectangle.
///
/// The largest divisor of the face count that is at most five — five being about
/// as wide as the clamp holds at a comfortable button size. Every die Sieve
/// offers divides evenly by this, which is what keeps the last row full.
fn faces_per_line(sides: u32) -> u32 {
    (2..=5)
        .rev()
        .find(|n| sides.is_multiple_of(*n))
        .unwrap_or(sides)
        .max(1)
}

/// Which step of how many, given whether a die is being rolled.
///
/// Free-standing so it can be checked without a display, like the other copy in
/// this file. Saying "Step 2 of 3" on a four-step flow is a small lie that makes
/// the last screen a surprise.
fn step_label(step: Step, rolling: bool) -> &'static str {
    match (step, rolling) {
        (Step::Welcome, _) => "Set up",
        (Step::Password, false) => "Step 1 of 3",
        (Step::Password, true) => "Step 1 of 4",
        (Step::Dice, _) => "Step 2 of 4",
        (Step::Phrase, false) => "Step 2 of 3",
        (Step::Phrase, true) => "Step 3 of 4",
        (Step::Verify, false) => "Step 3 of 3",
        (Step::Verify, true) => "Step 4 of 4",
        (Step::Working, _) => "Creating",
    }
}

/// Where the words came from, said under them.
///
/// Free-standing like `phrase_warning`, and for the same reason: this is a
/// claim about how the wallet was made, so it is pinned by a test rather than
/// left to drift when the code it describes changes. `wallet::generate_mnemonic`
/// is what it describes — `getrandom::fill`, which is `getrandom(2)` on Linux,
/// the same call the vault uses for its salt, nonces and data key.
///
/// Said at all because a person handed twelve words has no way to tell a good
/// phrase from a bad one by looking: they are the same twelve words either way.
/// The provenance is the only part that can be shown.
fn entropy_note(words: usize) -> String {
    let bits = words * 32 / 3;
    format!(
        "Chosen with {bits} bits of randomness from this computer's operating system — \
         the same source that seals the wallet file, and the only one Sieve uses."
    )
}

/// The chains offered when making a wallet, bitcoin first and by default.
///
/// Order matters twice: it is what the picker shows, and it is what an index
/// out of the picker means.
const NETWORKS: [bdk_wallet::bitcoin::Network; 3] = wallet::NETWORKS;

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
                    set_subtitle: model.step_label(),
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
                    // Named for the screen rather than for its first field. It
                    // began as a password prompt and has since collected the
                    // chain, the phrase length, the passphrase and the dice —
                    // and a title naming one of six things reads as though the
                    // rest arrived by accident.
                    set_title: "Set up your wallet",
                    set_description: Some(
                        "The chain, the phrase and the passphrase are fixed once the \
                         wallet is made. The password is not — that one can be changed \
                         later."
                    ),

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 400,
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,

                            adw::PreferencesGroup {
                                set_title: "Password",
                                // Off the page title and onto the group it
                                // actually describes. Every other group here
                                // names itself; this one was relying on the
                                // heading, which is why the heading could not
                                // say anything else.
                                set_description: Some(
                                    "This locks the wallet on this computer. It is not \
                                     part of your recovery phrase — if you forget it you \
                                     can still restore from the words themselves."
                                ),

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
                                    // From the same table the index is read
                                    // against, so they cannot disagree.
                                    set_model: Some(&gtk::StringList::new(
                                        &NETWORKS.map(wallet::network_label)
                                    )),
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

                                // Unlike the passphrase above it, nothing here
                                // can go wrong: rolls are mixed into the
                                // computer's own randomness, so the worst a
                                // careless session does is add less than it
                                // could have. That is why it sits behind a
                                // plain switch and not behind a warning.
                                #[name(dice_expander)]
                                adw::ExpanderRow {
                                    set_title: "Roll your own randomness",
                                    set_subtitle: "Advanced. Dice rolls are mixed into \
                                                   the randomness this computer \
                                                   provides — they can only add to it, \
                                                   never weaken it.",
                                    set_show_enable_switch: true,
                                    set_enable_expansion: false,

                                    #[name(die_row)]
                                    add_row = &adw::ComboRow {
                                        set_title: "Die",
                                        set_model: Some(&gtk::StringList::new(&DIE_LABELS)),
                                    },

                                    // On an ActionRow's suffix rather than on
                                    // the ComboRow's subtitle: a subtitle
                                    // squeezes a ComboRow's value into an
                                    // ellipsis, which is the trap the Length
                                    // row above and the Locking row in
                                    // preferences both had to be dug out of.
                                    #[name(roll_target_row)]
                                    add_row = &adw::ActionRow {
                                        set_title: "Rolls needed",
                                        // Safe to wrap: this row has no value
                                        // label to squeeze, which is the trap
                                        // the Die row above it must avoid.
                                        set_subtitle_lines: 2,
                                    },
                                },
                            },

                            #[name(continue_button)]
                            gtk::Button {
                                add_css_class: "suggested-action",
                                add_css_class: "pill",
                                set_halign: gtk::Align::Center,
                                set_label: "Continue",
                                // Sensitivity is wired in `init`: seven controls
                                // share one rule, and reading them there keeps
                                // every password out of the model and out of
                                // every message.
                                set_sensitive: false,
                                connect_clicked[
                                    sender, pass_row, confirm_row, name_row, length_row,
                                    passphrase_expander, passphrase_row, passphrase_confirm_row,
                                    network_row, acknowledge_row, dice_expander, die_row
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
                                        passphrase_wanted: wanted,
                                        length: if length_row.selected() == 1 {
                                            wallet::PhraseLength::TwentyFour
                                        } else {
                                            wallet::PhraseLength::Twelve
                                        },
                                        network: wallet::network_at(
                                            network_row.selected() as usize
                                        ),
                                        acknowledged: acknowledge_row.is_active(),
                                        dice_sides: dice_expander
                                            .enables_expansion()
                                            .then(|| {
                                                wallet::DICE[(die_row.selected() as usize)
                                                    .min(wallet::DICE.len() - 1)]
                                            }),
                                    }));
                                },
                            },
                        },
                    },
                },

                // ---- dice ----
                add_named[Some("dice")] = &adw::StatusPage {
                    set_title: "Roll your own randomness",
                    #[watch]
                    set_description: Some(&model.dice_description()),

                    #[wrap(Some)]
                    set_child = &adw::Clamp {
                        set_maximum_size: 400,
                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 18,

                            // The buttons are the affordance and the accessible
                            // path; the number keys are the fast one. Ninety-nine
                            // clicks is a minute and a half of aiming a pointer,
                            // and a hand resting on the keypad never has to look
                            // away from the die.
                            // Not homogeneous, and centred: homogeneous made
                            // every face stretch to a share of the whole clamp,
                            // so a d6 came out as three buttons the width of a
                            // finger each. The rows per line are set when the
                            // die is chosen, so every die gets whole rows
                            // rather than a ragged last one.
                            #[local_ref]
                            face_grid -> gtk::FlowBox {
                                set_selection_mode: gtk::SelectionMode::None,
                                set_row_spacing: 6,
                                set_column_spacing: 6,
                                set_homogeneous: false,
                                set_halign: gtk::Align::Center,
                            },

                            gtk::ProgressBar {
                                #[watch]
                                set_fraction: model.roll_fraction(),
                            },

                            gtk::Label {
                                add_css_class: "dim-label",
                                set_halign: gtk::Align::Center,
                                #[watch]
                                set_label: &model.roll_count_label(),
                            },

                            gtk::Box {
                                set_orientation: gtk::Orientation::Horizontal,
                                set_spacing: 12,
                                set_halign: gtk::Align::Center,

                                gtk::Button {
                                    add_css_class: "pill",
                                    set_label: "Undo",
                                    #[watch]
                                    set_sensitive: model.rolls_entered() > 0,
                                    connect_clicked => OnboardingMsg::UndoRoll,
                                },

                                gtk::Button {
                                    add_css_class: "suggested-action",
                                    add_css_class: "pill",
                                    #[watch]
                                    set_label: &model.use_rolls_label(),
                                    #[watch]
                                    set_sensitive: model.rolls_complete(),
                                    connect_clicked => OnboardingMsg::RollsDone,
                                },
                            },

                            gtk::Label {
                                add_css_class: "dim-label",
                                add_css_class: "caption",
                                set_wrap: true,
                                set_justify: gtk::Justification::Center,
                                set_label: DICE_NOTE,
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

                            // Under the words rather than in the page
                            // description, which is spoken for: that slot
                            // carries the warning that anyone reading these
                            // can spend the money, and provenance must not
                            // crowd out custody.
                            gtk::Label {
                                add_css_class: "dim-label",
                                add_css_class: "caption",
                                set_wrap: true,
                                set_justify: gtk::Justification::Center,
                                #[watch]
                                set_label: &entropy_note(model.length.words()),
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
        let faces = FactoryVecDeque::builder()
            .launch_default()
            .forward(sender.input_sender(), OnboardingMsg::Roll);
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
            dice: None,
            faces,
            error: None,
        };
        let word_grid = model.words.widget();
        let face_grid = model.faces.widget();
        let widgets = view_output!();

        // Continue answers for the whole form, so every control on the form has
        // to re-ask it. Wired here rather than in the view because one rule is
        // shared by seven signals, and because reading the rows straight keeps
        // the passwords where they were typed: what reaches `what_is_missing` is
        // a handful of booleans, and nothing crosses a message boundary at all.
        let refresh = {
            let button = widgets.continue_button.clone();
            let pass = widgets.pass_row.clone();
            let confirm = widgets.confirm_row.clone();
            let network = widgets.network_row.clone();
            let acknowledge = widgets.acknowledge_row.clone();
            let expander = widgets.passphrase_expander.clone();
            let phrase = widgets.passphrase_row.clone();
            let phrase_confirm = widgets.passphrase_confirm_row.clone();
            std::rc::Rc::new(move || {
                // An unexpanded row keeps whatever was typed before it was
                // switched off, so the switch decides whether there is a
                // passphrase at all — exactly as the Continue handler reads it.
                let wanted = expander.enables_expansion();
                let missing = what_is_missing(FormState {
                    password_long_enough: pass.text().len() >= MIN_PASSWORD,
                    passwords_match: pass.text() == confirm.text(),
                    mainnet: wallet::network_at(network.selected() as usize)
                        == bdk_wallet::bitcoin::Network::Bitcoin,
                    acknowledged: acknowledge.is_active(),
                    passphrase_wanted: wanted,
                    passphrase_typed: !phrase.text().is_empty(),
                    passphrases_match: phrase.text() == phrase_confirm.text(),
                });
                button.set_sensitive(missing.is_none());
                button.set_tooltip_text(missing);
            })
        };

        for row in [
            &widgets.pass_row,
            &widgets.confirm_row,
            &widgets.passphrase_row,
            &widgets.passphrase_confirm_row,
        ] {
            let refresh = refresh.clone();
            row.connect_changed(move |_| refresh());
        }
        {
            let refresh = refresh.clone();
            widgets
                .acknowledge_row
                .connect_active_notify(move |_| refresh());
        }
        {
            let refresh = refresh.clone();
            widgets
                .passphrase_expander
                .connect_enable_expansion_notify(move |_| refresh());
        }
        {
            let refresh = refresh.clone();
            widgets
                .network_row
                .connect_selected_notify(move |_| refresh());
        }
        // The starting state, which is "nothing typed yet" and so not ready.
        refresh();

        // How many rolls the chosen die needs, which depends on the phrase
        // length chosen two rows above it. Derived from wallet::rolls_needed
        // rather than written down, so the screen cannot promise bits the
        // arithmetic does not deliver.
        let roll_target = {
            let row = widgets.roll_target_row.clone();
            let die = widgets.die_row.clone();
            let length = widgets.length_row.clone();
            std::rc::Rc::new(move || {
                let sides = wallet::DICE[(die.selected() as usize).min(wallet::DICE.len() - 1)];
                let phrase = match length.selected() == 1 {
                    true => wallet::PhraseLength::TwentyFour,
                    false => wallet::PhraseLength::Twelve,
                };
                row.set_subtitle(&roll_target_note(sides, phrase));
            })
        };
        for row in [&widgets.die_row, &widgets.length_row] {
            let roll_target = roll_target.clone();
            row.connect_selected_notify(move |_| roll_target());
        }
        roll_target();

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
            OnboardingMsg::Begin => self.step = Step::Password,

            OnboardingMsg::Restore => {
                let _ = sender.output(OnboardingOutput::WantsRestore);
            }

            OnboardingMsg::Roll(Face(value)) => {
                // Ignored anywhere but the roll screen: the key controller is on
                // the window, so this arrives for any digit the focused widget
                // did not want.
                if self.step != Step::Dice {
                    return;
                }
                let Some(dice) = self.dice.as_mut() else {
                    return;
                };
                // Clicked, so the value is a face on this die by construction.
                // Checked anyway, because the alternative to checking is
                // silently hashing a face that does not exist.
                let face = match (1..=dice.sides).contains(&value) {
                    true => value,
                    false => return,
                };
                // Well past any honest session, and only here so a key held
                // down cannot grow the string without end.
                if dice.rolls.chars().count() >= MAX_ROLLS {
                    return;
                }
                // Base-36 so a face past 9 stays one character, which keeps
                // "how many rolls" a matter of counting characters.
                dice.rolls.push(char::from_digit(face, 36).unwrap_or('0'));
            }

            OnboardingMsg::UndoRoll => {
                if let Some(dice) = self.dice.as_mut() {
                    dice.rolls.pop();
                }
            }

            OnboardingMsg::RollsDone => {
                // The same gate the button is held shut by, asked again: a
                // threshold enforced only by the sensitivity of the widget that
                // crosses it is enforced by the view.
                if self.rolls_complete() {
                    self.make_phrase();
                }
            }

            OnboardingMsg::Back => match self.previous() {
                Some(previous) => {
                    // Going back past the roll screen drops the rolls. They are
                    // a share of a seed that is no longer being made, and
                    // keeping them would mean a later run silently inheriting
                    // entropy somebody thought they had walked away from.
                    if self.step == Step::Dice {
                        self.dice = None;
                        self.faces.guard().clear();
                    }
                    self.step = previous;
                }
                // Already at the first step, so back means leaving setup.
                None => {
                    let _ = sender.output(OnboardingOutput::Cancelled);
                }
            },

            OnboardingMsg::NetworkChanged(index) => {
                self.network = wallet::network_at(index as usize);
                self.mainnet = self.network == bdk_wallet::bitcoin::Network::Bitcoin;
            }

            OnboardingMsg::Configured(setup) => {
                // The same rule the Continue button is disabled by, asked
                // again. The button should already have made this unreachable;
                // it stays because a form guarded only by the sensitivity of
                // the widget that submits it is guarded by the view, and this
                // decides whether a vault gets sealed.
                //
                // No minimum and no rules for the passphrase itself: every byte
                // of a BIP-39 passphrase is part of the key, so any string is a
                // valid one. All that can be checked is that it was typed the
                // same way twice, and that one was typed at all.
                let wanted = setup.passphrase_wanted;
                if let Some(missing) = what_is_missing(FormState {
                    password_long_enough: setup.password.0.len() >= MIN_PASSWORD,
                    passwords_match: *setup.password.0 == *setup.confirm.0,
                    mainnet: setup.network == bdk_wallet::bitcoin::Network::Bitcoin,
                    acknowledged: setup.acknowledged,
                    passphrase_wanted: wanted,
                    passphrase_typed: !setup.passphrase.0.is_empty(),
                    passphrases_match: *setup.passphrase.0 == *setup.passphrase_confirm.0,
                }) {
                    self.error = Some(missing.into());
                    return;
                }
                // The switch decides, not the field: an empty passphrase asked
                // for is refused above rather than quietly meaning "none",
                // since BIP-39 derives a different seed for "" than for absent
                // and both look like a perfectly good wallet afterwards.
                self.passphrase = wanted.then(|| setup.passphrase.0.clone());
                self.length = setup.length;
                self.network = setup.network;
                self.password = Some(setup.password.0);
                let trimmed = setup.name.trim();
                self.name = (!trimmed.is_empty()).then(|| trimmed.to_owned());

                match setup.dice_sides {
                    // Rolling first: the phrase cannot be made until there is
                    // something to mix into it.
                    Some(sides) => {
                        let mut guard = self.faces.guard();
                        guard.clear();
                        for face in 1..=sides {
                            guard.push_back(face);
                        }
                        drop(guard);
                        // A divisor of the face count, so the grid is a
                        // rectangle: 2 rows of 3 for a d6, 4 of 5 for a d20. A
                        // ragged last row reads as a rendering fault.
                        let per_line = faces_per_line(sides);
                        let grid = self.faces.widget();
                        grid.set_min_children_per_line(per_line);
                        grid.set_max_children_per_line(per_line);
                        self.dice = Some(DiceRolls {
                            sides,
                            rolls: Zeroizing::new(String::new()),
                        });
                        self.step = Step::Dice;
                    }
                    None => {
                        self.dice = None;
                        self.make_phrase();
                    }
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
                if let Some(wanted) = self.passphrase.as_ref()
                    && **wanted != *passphrase.0
                {
                    self.error = Some(
                        "That passphrase does not match the one you set. It is part \
                         of the key, so it has to be exact — spaces and capitals \
                         included."
                            .into(),
                    );
                    return;
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
    /// A word of the phrase must not print itself, even though nothing logs
    /// one today. The type is one message signature away from being the leak
    /// `RevealCmd` was.
    /// Neither the phrase nor a die roll prints itself.
    ///
    /// `Secret` carries what becomes the seed and `Face` carries the rolls
    /// mixed into it, and both travel in messages relm4 formats. A roll is
    /// key material until the phrase exists, which is why it is fenced like
    /// the phrase rather than like a number.
    #[test]
    fn a_secret_and_a_roll_do_not_print_themselves() {
        let secret = super::Secret(zeroize::Zeroizing::new("abandon about".to_string()));
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{secret:?}").contains("abandon"));

        let face = super::Face(4);
        assert_eq!(format!("{face:?}"), "<redacted>");
        assert!(!format!("{face:?}").contains('4'));

        // And through the messages that actually reach relm4's span fields.
        let rolled = super::OnboardingMsg::Roll(super::Face(6));
        assert!(!format!("{rolled:?}").contains('6'), "{rolled:?}");
    }

    #[test]
    fn a_seed_word_does_not_print_itself() {
        let word = super::SeedWord {
            position: 7,
            word: "abandon".into(),
        };
        let printed = format!("{word:?}");
        assert!(!printed.contains("abandon"), "{printed}");
        // The number is not a secret, and it is what makes the rest legible.
        assert!(printed.contains('7'), "{printed}");
    }

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

    /// The note under the words claims a number of bits. BIP-39 fixes what that
    /// number is — a phrase carries `words * 32 / 3` bits — so the claim is
    /// checked against the standard rather than against itself. A screen that
    /// tells somebody they have 256 bits when they have 128 is worse than one
    /// that says nothing, because it is the sentence they would rely on.
    #[test]
    fn the_phrase_screen_states_the_right_number_of_bits() {
        assert!(
            entropy_note(12).contains("128 bits"),
            "{}",
            entropy_note(12)
        );
        assert!(
            entropy_note(24).contains("256 bits"),
            "{}",
            entropy_note(24)
        );

        // And it names the source rather than merely asserting the words are
        // random, which is what every wallet says and none of them show.
        for words in [12, 24] {
            let note = entropy_note(words);
            assert!(note.contains("operating system"), "{note}");
        }
    }

    /// A form with nothing wrong with it.
    fn ready() -> FormState {
        FormState {
            password_long_enough: true,
            passwords_match: true,
            mainnet: true,
            acknowledged: true,
            passphrase_wanted: false,
            passphrase_typed: false,
            passphrases_match: true,
        }
    }

    #[test]
    fn a_finished_form_is_finished() {
        assert_eq!(what_is_missing(ready()), None);
        // And on a test network, where there is nothing to acknowledge.
        let signet = FormState {
            mainnet: false,
            acknowledged: false,
            ..ready()
        };
        assert_eq!(what_is_missing(signet), None);
    }

    #[test]
    fn every_unfinished_form_says_what_is_left() {
        for (name, form) in [
            (
                "short password",
                FormState {
                    password_long_enough: false,
                    ..ready()
                },
            ),
            (
                "mismatched passwords",
                FormState {
                    passwords_match: false,
                    ..ready()
                },
            ),
            (
                "unacknowledged mainnet",
                FormState {
                    acknowledged: false,
                    ..ready()
                },
            ),
            (
                "mismatched passphrases",
                FormState {
                    passphrase_wanted: true,
                    passphrase_typed: true,
                    passphrases_match: false,
                    ..ready()
                },
            ),
        ] {
            assert!(
                what_is_missing(form).is_some(),
                "{name} was allowed through"
            );
        }
    }

    /// The switch is what asks for a passphrase, so the switch is what has to be
    /// answered. Left on with nothing typed, this used to fall through to "no
    /// passphrase" — a wallet derived from `""`-as-absent rather than the one the
    /// person switching it on was asking for, with nothing afterwards to say so.
    /// `ROADMAP.md` claimed it was refused; it was not.
    #[test]
    fn a_passphrase_asked_for_and_not_typed_is_refused() {
        let empty = FormState {
            passphrase_wanted: true,
            passphrase_typed: false,
            // Two empty fields do match, which is exactly why matching alone
            // never caught this.
            passphrases_match: true,
            ..ready()
        };
        assert!(what_is_missing(empty).is_some());

        // And with the switch off, the same two empty fields are fine: not
        // wanting a passphrase is an answer.
        let unwanted = FormState {
            passphrase_wanted: false,
            ..empty
        };
        assert_eq!(what_is_missing(unwanted), None);
    }

    /// The tooltip is read off a button somebody is looking at, so the first
    /// thing it names should be the first thing on the screen.
    #[test]
    fn what_is_missing_is_reported_in_form_order() {
        let nothing_done = FormState {
            password_long_enough: false,
            passwords_match: false,
            acknowledged: false,
            ..ready()
        };
        assert_eq!(
            what_is_missing(nothing_done),
            Some("Use a password of at least 8 characters."),
            "the password is above the acknowledgement on the screen"
        );
    }

    /// The picker's labels are positional: the index out of the ComboRow is what
    /// selects from `wallet::DICE`, so a list that drifts would roll a die
    /// nobody chose — and would ask for the wrong number of rolls with it.
    #[test]
    fn the_die_labels_line_up_with_the_dice() {
        assert_eq!(DIE_LABELS.len(), wallet::DICE.len());
        for (label, sides) in DIE_LABELS.iter().zip(wallet::DICE) {
            assert!(
                label.contains(&sides.to_string()),
                "{label} is not the label for a d{sides}"
            );
        }
        // d6 first, because it is the default and the floor both.
        assert_eq!(wallet::DICE[0], 6);
    }

    /// Every page the stack can be asked for has to exist, dice included. A
    /// name the stack does not have is a silent no-op that leaves the flow on
    /// whichever page it was showing.
    /// The number and its reason travel together, and both come from the same
    /// arithmetic the phrase is actually made with.
    #[test]
    fn the_roll_target_says_what_it_buys() {
        let note = roll_target_note(6, wallet::PhraseLength::Twelve);
        assert!(note.starts_with("50 rolls"), "{note}");
        assert!(note.contains("128 bits"), "{note}");
        assert!(note.contains("12-word"), "{note}");

        // A longer phrase asks for more of the same die, and says so.
        let longer = roll_target_note(6, wallet::PhraseLength::TwentyFour);
        assert!(longer.starts_with("100 rolls"), "{longer}");
        assert!(longer.contains("256 bits"), "{longer}");

        // And a bigger die asks for fewer, which is the trade the row exists
        // to make visible.
        let d20 = roll_target_note(20, wallet::PhraseLength::TwentyFour);
        assert!(d20.starts_with("60 rolls"), "{d20}");
        assert!(d20.contains("256 bits"), "{d20}");
    }

    /// A ragged last row reads as a rendering fault rather than a layout, so
    /// every die Sieve offers has to divide evenly into its row width.
    #[test]
    fn every_die_lays_out_as_a_rectangle() {
        for sides in wallet::DICE {
            let per_line = faces_per_line(sides);
            assert!(
                (2..=5).contains(&per_line),
                "d{sides} puts {per_line} on a row"
            );
            assert!(
                sides.is_multiple_of(per_line),
                "d{sides} would leave a ragged row"
            );
        }
        assert_eq!(faces_per_line(6), 3);
        assert_eq!(faces_per_line(20), 5);
        // A prime face count has no rectangle; one row is the honest answer
        // rather than a panic.
        assert_eq!(faces_per_line(7), 7);
    }

    #[test]
    fn the_dice_step_names_a_page() {
        assert_eq!(Step::Dice.tag(), "dice");
        assert_eq!(Step::Dice.previous(), Some(Step::Password));
    }

    /// Rolling adds a step, and the header has to count it. Saying "Step 2 of 3"
    /// on a four-step flow is a small lie that makes the last screen a surprise.
    #[test]
    fn the_header_counts_the_roll_screen_when_there_is_one() {
        for (step, rolling, expected) in [
            (Step::Password, false, "Step 1 of 3"),
            (Step::Password, true, "Step 1 of 4"),
            (Step::Dice, true, "Step 2 of 4"),
            (Step::Phrase, false, "Step 2 of 3"),
            (Step::Phrase, true, "Step 3 of 4"),
            (Step::Verify, false, "Step 3 of 3"),
            (Step::Verify, true, "Step 4 of 4"),
        ] {
            assert_eq!(
                step_label(step, rolling),
                expected,
                "{step:?} rolling={rolling}"
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
