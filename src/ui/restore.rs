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

use bdk_wallet::keys::bip39::{Language, Mnemonic};

use crate::ui::phrase::{PhraseWord, PhraseWordMsg, PhraseWordOutput, State, Word};
use crate::wallet::accounts::{CredentialKind, ScriptType};
use crate::wallet::{self, Paths, Summary};

/// Hardware first: it is the one people arrive here holding, and the one
/// that needs the most help.
/// Order is the default: the form opens on `KINDS[0]`.
///
/// A recovery phrase leads because it is what most people arrive holding —
/// twelve or twenty-four words on paper is what "I have a wallet elsewhere"
/// usually means, and a device is the second answer rather than the first. The
/// rest descend by how often anybody reaches for them.
const KINDS: [CredentialKind; 5] = [
    CredentialKind::Mnemonic,
    CredentialKind::Hardware,
    CredentialKind::Descriptor,
    CredentialKind::ExtendedKey,
    CredentialKind::Wif,
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
    acknowledged: bool,
    name: String,
}

#[derive(Debug)]
pub enum RestoreMsg {
    KindChanged(u32),
    /// Look for devices on the USB ports.
    LookForDevices,
    DeviceChosen(u32),
    BirthdayChanged(u32),
    NetworkChanged(u32),
    /// A box settled on a word; the status line is recomputed.
    PhraseChanged,
    /// 12 or 24, by index.
    PhraseLength(u32),
    /// Words that arrived at box `after` and belong to the boxes past it.
    PhraseSpill(usize, Vec<Word>),
    Submit(Box<Submission>),
    Cancel,
}

#[derive(Debug)]
// Sent once, when an import finishes. See the note on `OnboardingOutput`.
#[allow(clippy::large_enum_variant)]
pub enum RestoreOutput {
    Imported { paths: Paths, summary: Summary },
    Cancelled,
}

#[derive(Debug)]
// As above: these are results delivered once, not values kept in bulk.
#[allow(clippy::large_enum_variant)]
pub enum RestoreCmd {
    /// What a look for devices turned up.
    Devices(Vec<crate::hardware::Found>),
    /// A device answered with the descriptors of its accounts — one per
    /// standard path, the way a seed import searches all of them.
    /// The device's kind and master fingerprint, and a descriptor per account
    /// path. Both are recorded: the kind so signing knows what to connect to,
    /// the fingerprint so it can refuse a device that is not this one.
    FromDevice(Result<(String, String, Vec<String>), String>),
    Finished(Result<(Paths, Summary), String>),
}

/// What the import needs once a device has answered.
///
/// Held between asking the device and the import itself, because the two are
/// separate round trips and the form may have moved on in between.
#[derive(Debug, Clone)]
struct PendingImport {
    network: bdk_wallet::bitcoin::Network,
    birthday: crate::wallet::Checkpoint,
    name: Option<String>,
}

pub struct Restore {
    kind: CredentialKind,
    network: bdk_wallet::bitcoin::Network,
    birthday_index: u32,
    busy: bool,
    error: Option<String>,
    /// Devices found by the last look, and which one is chosen.
    devices: Vec<crate::hardware::Found>,
    device_index: u32,
    /// Whether a look for devices has happened at all, so "none found" and
    /// "not looked yet" can say different things.
    looked: bool,
    scanning: bool,
    /// Set while a device is being asked.
    pending: Option<PendingImport>,
    /// Backing model for the birthday picker, mutated in place when the
    /// network changes. Rebuilding it would reset the selection.
    birthday_model: gtk::StringList,
    /// One box per word of a recovery phrase.
    ///
    /// The model rather than the widgets is where the phrase lives, because
    /// these boxes hand words to each other — a paste into any of them fills
    /// the rest — and that is a conversation the parent has to hold.
    words: FactoryVecDeque<PhraseWord>,
}

/// Networks offered, bitcoin first because importing a seed almost always
/// means importing a real one. Signet was the default while mainnet was a
/// thing the interface half-allowed; making somebody change it to reach the
/// chain their money is on was a step that taught nothing. The
/// acknowledgement below is what carries the warning now, and it is a
/// sentence rather than a wrong default.
/// Take the `GtkFlowBoxChild`s out of the tab chain.
///
/// A factory in a `gtk::FlowBox` gets each item wrapped in a `FlowBoxChild` —
/// that is relm4's `ReturnedWidget` for this container — and the wrapper is
/// focusable in its own right. So Tab went wrapper, entry, wrapper, entry, and
/// every second press appeared to do nothing: focus really had moved, just to
/// something with nothing to type into.
///
/// Called after every resize, because the wrappers are made with the items.
fn skip_the_wrappers(flow: &gtk::FlowBox) {
    let mut index = 0;
    while let Some(child) = flow.child_at_index(index) {
        child.set_focusable(false);
        index += 1;
    }
}

impl Restore {
    fn chosen_device(&self) -> Option<&crate::hardware::Found> {
        self.devices.get(self.device_index as usize)
    }

    /// What the device group says under its title.
    fn device_status(&self) -> String {
        if self.scanning {
            return "Looking…".into();
        }
        match (self.looked, self.devices.len()) {
            (false, _) => "Plug the device in, unlock it, and press Look for devices.".into(),
            (true, 0) => format!("Nothing found. {}", crate::hardware::udev_hint()),
            (true, 1) => "Found one device.".into(),
            (true, n) => format!("Found {n} devices."),
        }
    }

    fn is_mainnet(&self) -> bool {
        self.network == bdk_wallet::bitcoin::Network::Bitcoin
    }

    fn credential_title(&self) -> &'static str {
        match self.kind {
            CredentialKind::Mnemonic => "Recovery phrase",
            CredentialKind::ExtendedKey => "Extended private key",
            CredentialKind::Wif => "Private key",
            CredentialKind::Descriptor => "Descriptor or xpub",
            CredentialKind::Hardware => "Hardware wallet",
        }
    }

    fn credential_hint(&self) -> &'static str {
        match self.kind {
            CredentialKind::Mnemonic => "The 12 or 24 words, separated by spaces",
            CredentialKind::ExtendedKey => "An xprv, tprv or vprv. No recovery phrase needed",
            CredentialKind::Wif => "A single private key in Wallet Import Format",
            CredentialKind::Descriptor => {
                "Paste an exported descriptor or extended \
                                            public key. Watch-only: no password, and Sieve \
                                            cannot sign"
            }
            CredentialKind::Hardware => {
                "Plug the device in and unlock it. On a Ledger, open \
                                         the Bitcoin app. Nothing secret crosses the cable: \
                                         Sieve takes a public key and the device keeps the \
                                         rest"
            }
        }
    }

    /// What the import will actually watch.
    /// How many words the phrase is being typed at.
    fn word_count(&self) -> usize {
        self.words.len()
    }

    /// Grow or shrink the boxes, keeping what has already been typed.
    fn set_word_count(&mut self, count: usize) {
        {
            let mut guard = self.words.guard();
            while guard.len() > count {
                guard.pop_back();
            }
            while guard.len() < count {
                let position = guard.len() + 1;
                guard.push_back((position, Word::default()));
            }
        }
        // The guard has to be dropped first: the widgets do not exist until it
        // is, and this reaches for them.
        skip_the_wrappers(self.words.widget());
    }

    /// The phrase as one string, assembled only where it is needed.
    ///
    /// `Zeroizing` from the first byte rather than built and then wrapped: a
    /// `String` that grows leaves its old buffer behind for the allocator, and
    /// there is no reason to add to that when the length is known here.
    fn phrase(&self) -> Zeroizing<String> {
        let mut joined = Zeroizing::new(String::with_capacity(self.words.len() * 9));
        for word in self.words.iter() {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(word.word().as_str());
        }
        joined
    }

    /// What the description under the boxes says, which is the whole of the
    /// feedback this screen gives while a phrase is being typed.
    ///
    /// Ordered by what is most useful to hear. A word that is not on the list
    /// is named before anything else, because it is the one mistake that can be
    /// pointed at; the count comes next, because it is the answer to "am I
    /// nearly there"; and only a complete phrase gets judged as a phrase.
    ///
    /// **A valid phrase is worth saying out loud.** Every other wallet leaves
    /// somebody to press Import and find out, and the checksum exists precisely
    /// so that a mistyped word can be caught before it becomes an empty wallet
    /// that looks exactly like a correct one.
    /// The colour that sentence should be wearing.
    ///
    /// Adwaita's own classes, so they follow the theme. Four states rather than
    /// two, because "not a BIP-39 phrase" is not one thing: a phrase from
    /// another wallet is somebody holding a *correct* backup, and colouring
    /// that the same red as a mistyped word tells them their paper is wrong.
    fn phrase_status_classes(&self) -> &'static [&'static str] {
        if self.words.iter().any(|w| w.state() == State::Bad) {
            return &["error"];
        }
        if self.words.iter().any(|w| w.word().is_empty()) {
            return &["dim-label"];
        }
        let phrase = self.phrase();
        if Mnemonic::parse_in(Language::English, phrase.as_str()).is_ok() {
            return &["success"];
        }
        if crate::ui::phrase::electrum_seed(phrase.as_str()).is_some() {
            // A warning, not an error. Nothing is wrong with what they have.
            &["warning"]
        } else {
            &["error"]
        }
    }

    fn phrase_status(&self) -> String {
        let total = self.words.len();
        if let Some(bad) = self.words.iter().position(|w| w.state() == State::Bad) {
            return format!(
                "Word {} is not one of the 2,048 recovery-phrase words. Check it against \
                 your paper.",
                bad + 1
            );
        }

        let filled = self.words.iter().filter(|w| !w.word().is_empty()).count();
        if filled < total {
            return format!(
                "{filled} of {total} words. Type them in order, or paste the whole phrase \
                 into any box."
            );
        }

        let phrase = self.phrase();
        match Mnemonic::parse_in(Language::English, phrase.as_str()) {
            Ok(_) => "This is a valid recovery phrase.".into(),
            Err(_) => {
                // Before blaming the person: Electrum uses these same 2,048
                // words with a different rule, so a perfectly good Electrum
                // seed lands here looking like a typo. Saying "one is out of
                // order" to somebody holding a correct backup sends them to
                // re-check paper that is already right, and the likely
                // conclusion is that the backup is ruined.
                if let Some(other) = crate::ui::phrase::electrum_seed(phrase.as_str()) {
                    return other.explain().to_string();
                }
                // Every word is real and the phrase is still wrong, which means
                // the checksum failed: a word is in the wrong place, or one
                // right word stands where another right word belongs. Neither
                // is visible per box, so this is the only place it can be said.
                format!(
                    "These {total} words are all real words, but they are not a valid \
                     recovery phrase — one is out of order or in place of another."
                )
            }
        }
    }

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
        birthday_for(self.network, index)
    }

    /// What the chosen birthday actually resolves to, so the consequence of the
    /// choice is visible before importing.
    fn birthday_label(&self, index: u32) -> String {
        let checkpoint = self.birthday_for(index);
        if checkpoint.height == 0 {
            return format!("{} — every block since 2009", checkpoint.when);
        }
        format!("{} — from block {}", checkpoint.when, checkpoint.height)
    }

    /// Put the current network's choices into the picker's model.
    ///
    /// In place: the list is spliced rather than replaced, because replacing it
    /// resets the selection and the selection is what gets imported.
    fn fill_birthdays(&self) {
        let choices = self.birthday_choices();
        let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
        self.birthday_model
            .splice(0, self.birthday_model.n_items(), &refs);
    }

    /// The choices offered, straight from the checkpoints they select.
    fn birthday_choices(&self) -> Vec<String> {
        birthday_choices(self.network)
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
                    set_title: "Name",
                    set_description: Some("Optional, and changeable later."),

                    #[name(name_row)]
                    adw::EntryRow {
                        set_title: "Wallet name",
                    },
                },

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

                // Everything a device needs, and nothing a device does not.
                adw::PreferencesGroup {
                    #[watch]
                    set_visible: model.kind == CredentialKind::Hardware,
                    set_title: "Your device",
                    #[watch]
                    set_description: Some(&model.device_status()),

                    #[name(device_row)]
                    adw::ComboRow {
                        set_title: "Device",
                        #[watch]
                        set_visible: !model.devices.is_empty(),
                        #[watch]
                        #[block_signal(device_chosen)]
                        set_model: Some(&gtk::StringList::new(
                            &model.devices.iter().map(|d| d.label.as_str()).collect::<Vec<_>>()
                        )),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(RestoreMsg::DeviceChosen(row.selected()));
                        } @device_chosen,
                    },

                    adw::ActionRow {
                        set_title: "Look for devices",
                        set_subtitle: "Sieve reads a public key. The device keeps everything \
                                       else, and signs for itself",
                        set_subtitle_lines: 2,
                        set_activatable: true,
                        add_suffix = &gtk::Button {
                            #[watch]
                            set_label: if model.scanning { "Looking…" } else { "Look" },
                            #[watch]
                            set_sensitive: !model.scanning,
                            set_valign: gtk::Align::Center,
                            connect_clicked => RestoreMsg::LookForDevices,
                        },
                        connect_activated => RestoreMsg::LookForDevices,
                    },
                },

                // A phrase gets a box per word; everything else is one string
                // and gets one line. Two groups rather than one that changes
                // shape, because they share nothing but a position on screen.
                adw::PreferencesGroup {
                    #[watch]
                    set_visible: model.kind == CredentialKind::Mnemonic,
                    set_title: "Recovery phrase",
                    set_description: Some(
                        "Type the words in order, or paste the whole phrase into any box."
                    ),

                    #[name(length_row)]
                    adw::ComboRow {
                        set_title: "Length",
                        set_subtitle: "How many words are on your paper",
                        set_model: Some(&gtk::StringList::new(&["12 words", "24 words"])),
                        // Follows the model, because pasting a 24-word phrase
                        // grows the boxes and this row has to agree with what
                        // is on screen. Setting it to the value it already has
                        // emits nothing, so this cannot loop.
                        #[watch]
                        set_selected: u32::from(model.word_count() > 12),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(RestoreMsg::PhraseLength(row.selected()));
                        },
                    },

                    // Not a row, so libadwaita puts it under the list rather
                    // than in it — which is where a grid belongs.
                    #[local_ref]
                    phrase_box -> gtk::FlowBox {
                        set_selection_mode: gtk::SelectionMode::None,
                        set_homogeneous: true,
                        set_min_children_per_line: 2,
                        set_max_children_per_line: 3,
                        set_row_spacing: 6,
                        set_column_spacing: 6,
                        set_margin_top: 12,
                    },

                    // Below the grid rather than in the group's description,
                    // because a description cannot carry a colour and this
                    // sentence changes meaning: counting up is neutral, a
                    // phrase from another wallet is a warning, a phrase that
                    // will not import is an error, and a phrase that is right
                    // deserves to say so.
                    gtk::Label {
                        set_wrap: true,
                        set_xalign: 0.0,
                        set_margin_top: 12,
                        #[watch]
                        set_label: &model.phrase_status(),
                        #[watch]
                        set_css_classes: model.phrase_status_classes(),
                    },
                },

                adw::PreferencesGroup {
                    #[watch]
                    set_visible: !matches!(
                        model.kind,
                        CredentialKind::Hardware | CredentialKind::Mnemonic
                    ),
                    #[watch]
                    set_title: model.credential_title(),
                    #[watch]
                    set_description: Some(model.credential_hint()),

                    #[name(credential_row)]
                    adw::EntryRow {
                        #[watch]
                        set_title: model.credential_title(),
                    },

                },

                // Its own group now that a phrase and an extended key are drawn
                // by different ones: it belongs to both, and living inside
                // either would hide it from the other.
                adw::PreferencesGroup {
                    // Only meaningful for a seed, and dangerous to confuse with
                    // the wallet password, so it is hidden otherwise.
                    #[watch]
                    set_visible: model.kind.is_hd(),

                    #[name(bip39_expander)]
                    adw::ExpanderRow {
                        set_title: "My seed has a passphrase",
                        set_subtitle: "Sometimes called a 25th word. Most seeds do not have one — leave this off if you were never asked to choose one.",
                        set_show_enable_switch: true,
                        set_enable_expansion: false,

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
                        // Built from the table the index is read against, so
                        // the two cannot disagree about what row 2 means.
                        set_model: Some(&gtk::StringList::new(
                            &wallet::NETWORKS.map(wallet::network_label)
                        )),
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
                        // The choices come from the checkpoints they select —
                        // they used to be a hand-written list matched by
                        // position, and adding a checkpoint moved every choice
                        // below it by one.
                        //
                        // Held and mutated rather than rebuilt under #[watch]:
                        // replacing a ComboRow's model resets its selection to
                        // the first item, and the submission reads the row. So
                        // a rebuilt model silently discarded the chosen
                        // birthday and imported from the most recent
                        // checkpoint — a wallet that scans nothing and says it
                        // is up to date.
                        set_model: Some(&model.birthday_model),
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
                    // A watch-only wallet holds no secret, so a password would
                    // lock a door with nothing behind it.
                    #[watch]
                    set_visible: model.kind.carries_keys(),
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
                            network_row, password_row, confirm_row,
                            acknowledge_row, name_row
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
                                acknowledged: acknowledge_row.is_active(),
                                name: name_row.text().to_string(),
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
        let mut words = FactoryVecDeque::builder()
            .launch(gtk::FlowBox::default())
            .forward(sender.input_sender(), |out| match out {
                PhraseWordOutput::Changed => RestoreMsg::PhraseChanged,
                PhraseWordOutput::Spill(after, rest) => RestoreMsg::PhraseSpill(after, rest),
            });
        {
            // Twelve to begin with: it is the commoner phrase, and pasting a
            // longer one grows the grid on its own.
            let mut guard = words.guard();
            for position in 1..=12 {
                guard.push_back((position, Word::default()));
            }
        }
        skip_the_wrappers(words.widget());

        let model = Restore {
            // A recovery phrase is first in the list, so it is what the form
            // opens on. See KINDS.
            kind: KINDS[0],
            words,
            network: bdk_wallet::bitcoin::Network::Bitcoin,
            birthday_index: 1,
            busy: false,
            error: None,
            devices: Vec::new(),
            device_index: 0,
            looked: false,
            scanning: false,
            pending: None,
            birthday_model: gtk::StringList::new(&[]),
        };
        model.fill_birthdays();
        let phrase_box = model.words.widget();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            RestoreMsg::LookForDevices => {
                if self.scanning {
                    return;
                }
                self.scanning = true;
                self.error = None;
                sender.oneshot_command(async move {
                    RestoreCmd::Devices(crate::hardware::enumerate().await)
                });
            }
            RestoreMsg::DeviceChosen(index) => self.device_index = index,

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
                // Networks have different checkpoints, so the choices change
                // with them — and an index into the old list means nothing in
                // the new one.
                self.network = wallet::network_at(index as usize);
                self.fill_birthdays();
                // The chosen index may not exist in the new list — signet has
                // two checkpoints where mainnet has seven — and an index past
                // the end would quietly become the last one.
                let choices = self.birthday_model.n_items();
                if self.birthday_index >= choices {
                    self.birthday_index = choices.saturating_sub(1);
                }
                self.error = None;
            }
            RestoreMsg::PhraseChanged => {
                // The status line reads the model, so there is nothing to do
                // but let the view run again.
                self.error = None;
            }

            RestoreMsg::PhraseLength(index) => {
                let wanted = if index == 0 { 12 } else { 24 };
                if self.words.len() == wanted {
                    return;
                }
                self.set_word_count(wanted);
                self.error = None;
            }

            RestoreMsg::PhraseSpill(after, rest) => {
                if rest.is_empty() {
                    return;
                }
                // A pasted 24-word phrase arriving in a 12-box grid is not an
                // error to report — it is a phrase, and the grid should become
                // the size of it. Only the two standard lengths, so a stray
                // paste of prose cannot stretch the screen.
                let needed = after + rest.len();
                if needed > self.words.len() && needed <= 24 {
                    self.set_word_count(24);
                }

                let mut last = after;
                for (offset, word) in rest.into_iter().enumerate() {
                    let index = after + offset;
                    if index >= self.words.len() {
                        break;
                    }
                    self.words.send(index, PhraseWordMsg::Set(word));
                    last = index + 1;
                }

                // The caret goes to the box after the last one filled, which is
                // where somebody typing a space meant to be, and where somebody
                // who pasted a short phrase needs to look.
                if last < self.words.len() {
                    self.words.send(last, PhraseWordMsg::Focus);
                }
                self.error = None;
            }

            RestoreMsg::Cancel => {
                let _ = sender.output(RestoreOutput::Cancelled);
            }
            RestoreMsg::Submit(mut submission) => {
                if self.busy {
                    return;
                }
                // The one field the submission cannot carry from a widget:
                // a phrase lives in twelve or twenty-four of them, and the
                // model is what they all report to.
                if submission.kind == CredentialKind::Mnemonic {
                    submission.credential = Secret(self.phrase());
                    // The status line says this already, and a paste followed
                    // straight by Import never gives anybody time to read it.
                    if let Some(other) =
                        crate::ui::phrase::electrum_seed(submission.credential.0.as_str())
                    {
                        self.error = Some(other.explain().to_string());
                        return;
                    }
                }
                let network = wallet::network_at(submission.network_index as usize);

                if network == bdk_wallet::bitcoin::Network::Bitcoin && !submission.acknowledged {
                    self.error =
                        Some("Confirm you understand the risk before importing to Bitcoin.".into());
                    return;
                }
                if submission.kind == CredentialKind::Hardware && self.chosen_device().is_none() {
                    self.error = Some(if self.looked {
                        format!("No device found. {}", crate::hardware::udev_hint())
                    } else {
                        "Press Look for devices first.".into()
                    });
                    return;
                }
                if submission.kind != CredentialKind::Hardware
                    && submission.credential.0.trim().is_empty()
                {
                    // Judge the submission, not the model: they can disagree if
                    // a row changed after the last view update.
                    self.error = Some(match submission.kind {
                        CredentialKind::Mnemonic => "Enter your recovery phrase.".into(),
                        CredentialKind::Wif => "Enter the private key.".into(),
                        CredentialKind::ExtendedKey => "Enter the extended key.".into(),
                        CredentialKind::Descriptor => "Enter the descriptor.".into(),
                        CredentialKind::Hardware => unreachable!("handled above"),
                    });
                    return;
                }
                if submission.kind.carries_keys() {
                    if submission.password.0.len() < 8 {
                        self.error = Some("Use a password of at least 8 characters.".into());
                        return;
                    }
                    if *submission.password.0 != *submission.confirm.0 {
                        self.error = Some("The two passwords do not match.".into());
                        return;
                    }
                }

                // From the model, not the submitted widget value. A
                // ComboRow's selection is reset by anything that replaces its
                // model, and a birthday silently reset to "most recent" gives
                // a wallet that scans nothing and calls itself up to date.
                // The model only changes when somebody chooses.
                let birthday = self.birthday_for(self.birthday_index);
                tracing::info!(
                    choice = self.birthday_index,
                    height = birthday.height,
                    when = birthday.when,
                    "importing with this birthday"
                );

                self.busy = true;
                self.error = None;

                // A device has to be asked before there is anything to import,
                // and that means waiting on somebody to press a button on it.
                if submission.kind == CredentialKind::Hardware {
                    let Some(device) = self.chosen_device() else {
                        return;
                    };
                    let kind = device.kind;
                    self.pending = Some(PendingImport {
                        network,
                        birthday,
                        name: {
                            let trimmed = submission.name.trim();
                            (!trimmed.is_empty()).then(|| trimmed.to_owned())
                        },
                    });
                    sender.oneshot_command(async move {
                        RestoreCmd::FromDevice(
                            crate::hardware::account_descriptors(kind, network)
                                .await
                                .map(|(fingerprint, found)| {
                                    (
                                        kind.label().to_string(),
                                        fingerprint.to_string(),
                                        found.into_iter().map(|(_, text)| text).collect(),
                                    )
                                })
                                .map_err(|e| e.to_string()),
                        )
                    });
                    return;
                }

                // A new wallet directory, so importing never disturbs an
                // existing wallet.
                let paths = Paths::for_wallet(&Paths::new_id());
                let trimmed = submission.name.trim();
                let name = (!trimmed.is_empty()).then(|| trimmed.to_owned());
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
                            name.clone(),
                            // An imported wallet already has history, so the
                            // window has to be wide enough to find it.
                            crate::wallet::accounts::IMPORT_LOOKAHEAD,
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
                            name.clone(),
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
                            name.clone(),
                        ),
                        // No password, no vault: the keys are somewhere
                        // else, which is the point.
                        CredentialKind::Descriptor => wallet::import_descriptor(
                            &credential,
                            &paths,
                            network,
                            birthday,
                            name.clone(),
                        ),
                        CredentialKind::Hardware => {
                            unreachable!("a device is asked before this point")
                        }
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
        let result = match msg {
            RestoreCmd::Devices(devices) => {
                self.scanning = false;
                self.looked = true;
                self.device_index = 0;
                self.devices = devices;
                return;
            }

            RestoreCmd::FromDevice(Ok((kind, fingerprint, descriptors))) => {
                // The device answered; now it is an ordinary descriptor
                // import, which is the whole point of the seam.
                let Some(pending) = self.pending.take() else {
                    self.busy = false;
                    return;
                };
                let paths = Paths::for_wallet(&Paths::new_id());
                let created = paths.clone();
                sender.spawn_oneshot_command(move || {
                    RestoreCmd::Finished(
                        wallet::import_descriptors(
                            &descriptors,
                            &paths,
                            pending.network,
                            pending.birthday,
                            pending.name,
                            Some((kind, fingerprint)),
                        )
                        .map(|summary| (created, summary))
                        .map_err(|e| e.to_string()),
                    )
                });
                return;
            }

            RestoreCmd::FromDevice(Err(message)) => {
                self.busy = false;
                self.pending = None;
                self.error = Some(crate::ui::send::capitalise(&message));
                return;
            }

            RestoreCmd::Finished(result) => result,
        };
        self.busy = false;
        match result {
            Ok((paths, summary)) => {
                let _ = sender.output(RestoreOutput::Imported { paths, summary });
            }
            Err(message) => self.error = Some(message),
        }
    }
}

/// Which checkpoint a choice selects.
///
/// Free functions, so what is offered and what it means can be checked without
/// building a form full of widgets — that correspondence has been wrong twice.
fn birthday_for(network: bdk_wallet::bitcoin::Network, index: u32) -> wallet::Checkpoint {
    let all = wallet::checkpoints(network);
    all.get(index as usize)
        .copied()
        .unwrap_or_else(|| *all.last().expect("a floor checkpoint exists"))
}

/// The choices offered for a network, in the order its checkpoints are in.
fn birthday_choices(network: bdk_wallet::bitcoin::Network) -> Vec<String> {
    wallet::checkpoints(network)
        .iter()
        .map(|c| {
            if c.height == 0 {
                c.when.to_string()
            } else {
                format!("{} or later", c.when)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The choices and the checkpoints they select are one list, not two.
    ///
    /// They used to be two: a hand-written set of phrases beside the checkpoint
    /// table. Adding a checkpoint shifted every choice below it, so "I don't
    /// know" silently became taproot activation and a wallet older than that
    /// found nothing — with the screen reporting exactly what was asked for.
    /// A phrase from another wallet is a warning, not an error.
    ///
    /// The colours carry meaning here: red says "you got this wrong", and
    /// somebody holding a correct Electrum backup has got nothing wrong. The
    /// four states are the four different things the sentence can mean.
    #[test]
    fn the_status_colour_matches_what_the_sentence_means() {
        use crate::ui::phrase::electrum_seed;

        let valid = "abandon abandon abandon abandon abandon abandon \
                     abandon abandon abandon abandon abandon about";
        let valid: String = valid.split_whitespace().collect::<Vec<_>>().join(" ");
        let electrum = "rapid phone kid day save forward gasp cereal nasty fat absorb load";
        let mistyped = "abandon abandon abandon abandon abandon abandon \
                        abandon abandon abandon abandon abandon abandon";
        let mistyped: String = mistyped.split_whitespace().collect::<Vec<_>>().join(" ");

        // The classifier the view uses, in the same order.
        fn class(phrase: &str) -> &'static str {
            if Mnemonic::parse_in(Language::English, phrase).is_ok() {
                "success"
            } else if electrum_seed(phrase).is_some() {
                "warning"
            } else {
                "error"
            }
        }

        assert_eq!(class(&valid), "success");
        assert_eq!(class(electrum), "warning");
        assert_eq!(class(&mistyped), "error");
        assert_ne!(
            class(electrum),
            class(&mistyped),
            "a correct phrase from another wallet must not look like a typo"
        );
    }

    #[test]
    fn every_choice_selects_the_checkpoint_it_names() {
        for network in wallet::NETWORKS {
            let choices = birthday_choices(network);
            let checkpoints = wallet::checkpoints(network);
            assert_eq!(
                choices.len(),
                checkpoints.len(),
                "{network}: a choice for every checkpoint and no more"
            );

            for (index, checkpoint) in checkpoints.iter().enumerate() {
                assert_eq!(
                    birthday_for(network, index as u32).height,
                    checkpoint.height,
                    "{network}: choice {index} selects the wrong checkpoint"
                );
            }

            // And the last choice is the whole chain, which is what "I don't
            // know" has to mean.
            let last = choices.len() as u32 - 1;
            assert_eq!(birthday_for(network, last).height, 0, "{network}");
        }
    }
}
