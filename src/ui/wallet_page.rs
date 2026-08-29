//! The unlocked wallet: balance, receive address, and sync status.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::node::Progress;
use crate::settings::{Denomination, Settings};
use crate::wallet::{AccountSummary, Summary};

/// One derivation path's row. A factory rather than a static list, because the
/// number of paths depends on how the wallet was created.
#[derive(Debug)]
pub struct PathRow {
    label: String,
    amount: String,
}

#[relm4::factory(pub)]
impl FactoryComponent for PathRow {
    type Init = (AccountSummary, Denomination);
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            set_title: &self.label,

            add_suffix = &gtk::Label {
                add_css_class: "numeric",
                set_label: &self.amount,
            },
        }
    }

    fn init_model(
        (summary, denomination): Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        PathRow {
            label: summary.script_type.to_string(),
            amount: denomination.format(summary.balance_sats),
        }
    }
}

#[derive(Debug)]
pub enum WalletPageOutput {
    SwitchWallet,
    /// Reveal a new address on this path.
    NewAddress(crate::wallet::accounts::ScriptType),
}

pub struct WalletPage {
    summary: Option<Summary>,
    progress: Progress,
    peers: Option<(usize, usize)>,
    paths: FactoryVecDeque<PathRow>,
    note: Option<String>,
    error: Option<String>,
    settings: Settings,
    /// Backing model for the address-type picker.
    ///
    /// Held and mutated in place rather than rebuilt under `#[watch]`: swapping
    /// a ComboRow's model resets its selection, so a rebuild on every view
    /// update would snap the picker back while someone was using it.
    path_model: gtk::StringList,
    /// Labels currently in `path_model`, so it is only rewritten when the set
    /// of paths genuinely changes.
    path_labels: Vec<String>,
    receive_index: u32,
    /// A freshly revealed address, shown instead of the next unused one until
    /// the path or wallet changes. Giving the same unused address to two payers
    /// links them, so asking for a new one has to actually produce one.
    fresh_address: Option<String>,
}

#[derive(Debug)]
pub enum WalletPageMsg {
    Show(Summary),
    SetProgress(Progress),
    Peers { connected: usize, required: usize },
    /// Something a person could actually act on. Routine peer churn is not this.
    Note(String),
    Failed(String),
    CopyAddress,
    SwitchWallet,
    /// Swap between decimal BTC and satoshis.
    ToggleDenomination,
    /// Choose which derivation path to receive on.
    SelectReceivePath(u32),
    /// Ask for an address that has not been handed to anyone yet.
    NewAddress,
    /// The freshly revealed address came back.
    ShowFreshAddress(String),
    /// Clear everything that belonged to a different wallet.
    Reset,
}

impl WalletPage {
    fn balance(&self) -> String {
        match &self.summary {
            Some(s) => self.settings.denomination.format(s.balance_sats),
            None => "—".into(),
        }
    }

    /// The address for the selected path, falling back to the wallet's primary
    /// one when there is no breakdown to choose from.
    fn address(&self) -> String {
        if let Some(fresh) = &self.fresh_address {
            return fresh.clone();
        }
        let Some(summary) = &self.summary else { return "—".into() };
        summary
            .accounts
            .get(self.receive_index as usize)
            .map(|a| a.next_address.clone())
            .unwrap_or_else(|| summary.next_address.clone())
    }

    fn has_path_choice(&self) -> bool {
        self.summary.as_ref().is_some_and(|s| s.accounts.len() > 1)
    }

    /// What the selected path's addresses look like, so the choice is
    /// recognisable without knowing BIP numbers.
    fn address_hint(&self) -> String {
        let Some(summary) = &self.summary else { return String::new() };
        let Some(account) = summary.accounts.get(self.receive_index as usize) else {
            return String::new();
        };
        let network = summary.network.parse().unwrap_or(bdk_wallet::bitcoin::Network::Signet);
        format!(
            "{} addresses start with {}",
            account.script_type.label(),
            account.script_type.example_prefix(network)
        )
    }

    fn peers(&self) -> String {
        match self.peers {
            Some((connected, required)) => format!("{connected} of {required} connected"),
            None => "Connecting…".into(),
        }
    }

    fn pending(&self) -> String {
        match &self.summary {
            Some(s) if s.pending_sats > 0 => self.settings.denomination.format(s.pending_sats),
            _ => "None".into(),
        }
    }

    fn verified_to(&self) -> String {
        match &self.summary {
            Some(s) if s.tip > 0 => format!("Block {}", s.tip),
            _ => "—".into(),
        }
    }

    /// A single-path wallet needs no breakdown; an imported one does.
    fn has_breakdown(&self) -> bool {
        self.summary.as_ref().is_some_and(|s| s.accounts.len() > 1)
    }

    /// The paths that were searched and came back empty.
    ///
    /// Listing them as one line rather than a row each keeps the reassurance —
    /// they were checked, not skipped — without four rows of zeros.
    fn searched_and_empty(&self) -> String {
        let Some(summary) = &self.summary else { return String::new() };
        let empty: Vec<&str> = summary
            .accounts
            .iter()
            .filter(|a| a.balance_sats == 0 && a.pending_sats == 0)
            .map(|a| a.script_type.label())
            .collect();

        if empty.is_empty() {
            "Every path searched holds coins.".into()
        } else if empty.len() == summary.accounts.len() {
            format!("Searched, nothing found: {}.", empty.join(", "))
        } else {
            format!("Also searched, nothing found: {}.", empty.join(", "))
        }
    }

    fn syncing(&self) -> bool {
        !matches!(self.progress, Progress::Synced)
    }
}

#[relm4::component(pub)]
impl SimpleComponent for WalletPage {
    type Init = ();
    type Input = WalletPageMsg;
    type Output = WalletPageOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Sieve",
                    #[watch]
                    set_subtitle: model.summary.as_ref().map_or("", |s| s.network.as_str()),
                },

                pack_end = &gtk::Button {
                    set_icon_name: "view-list-symbolic",
                    set_tooltip_text: Some("Switch wallet"),
                    connect_clicked => WalletPageMsg::SwitchWallet,
                },
            },

            #[wrap(Some)]
            set_content = &adw::PreferencesPage {
                adw::PreferencesGroup {
                    set_title: "Balance",

                    adw::ActionRow {
                        set_title: "Confirmed",
                        #[watch]
                        set_subtitle: &model.balance(),
                        set_activatable: true,
                        set_tooltip_text: Some("Switch between BTC and satoshis"),
                        connect_activated => WalletPageMsg::ToggleDenomination,

                        add_suffix = &gtk::Label {
                            add_css_class: "dim-label",
                            #[watch]
                            set_label: model.settings.denomination.label(),
                        },
                    },

                    adw::ActionRow {
                        set_title: "Pending",
                        // Compact block filters describe transactions in blocks,
                        // so an unconfirmed payment is invisible until it is
                        // mined. Saying so beats a balance that looks wrong.
                        set_subtitle: "Unconfirmed payments appear once mined",
                        #[watch]
                        set_visible: model.summary.as_ref().is_some_and(|s| s.pending_sats > 0),

                        add_suffix = &gtk::Label {
                            add_css_class: "numeric",
                            #[watch]
                            set_label: &model.pending(),
                        },
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Derivation paths",
                    set_description: Some(
                        "An imported seed is searched on every standard path.                          Paths showing nothing were scanned and found empty."
                    ),
                    #[watch]
                    set_visible: model.has_breakdown(),

                    #[local_ref]
                    paths_box -> gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Network",
                    set_description: Some(
                        "Sieve downloads compact block filters and matches them on this \
                         machine. No server learns which addresses are yours."
                    ),

                    adw::ActionRow {
                        set_title: "Status",
                        #[watch]
                        set_subtitle: &model.progress.label(),

                        // Spinner while the work is unbounded, bar once the
                        // node reports a real fraction. Never both, and both
                        // sit inside the row rather than under the card.
                        add_suffix = &gtk::Spinner {
                            set_valign: gtk::Align::Center,
                            #[watch]
                            set_visible: model.syncing() && model.progress.fraction().is_none(),
                            #[watch]
                            set_spinning: model.syncing() && model.progress.fraction().is_none(),
                        },

                        add_suffix = &gtk::ProgressBar {
                            set_valign: gtk::Align::Center,
                            set_width_request: 120,
                            #[watch]
                            set_visible: model.syncing() && model.progress.fraction().is_some(),
                            #[watch]
                            set_fraction: model.progress.fraction().unwrap_or(0.0),
                        },
                    },

                    adw::ActionRow {
                        set_title: "Peers",
                        #[watch]
                        set_subtitle: &model.peers(),
                    },

                    adw::ActionRow {
                        set_title: "Verified to",
                        #[watch]
                        set_subtitle: &model.verified_to(),
                    },

                    adw::ActionRow {
                        add_css_class: "warning",
                        set_title: "Note",
                        #[watch]
                        set_visible: model.note.is_some(),
                        #[watch]
                        set_subtitle: model.note.as_deref().unwrap_or_default(),
                        set_subtitle_lines: 2,
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Receive",
                    #[watch]
                    set_description: Some(&model.address_hint()),

                    #[name(path_picker)]
                    adw::ComboRow {
                        set_title: "Address type",
                        // Model set once and mutated in place; see path_model.
                        set_model: Some(&path_model),
                        // Only worth showing when there is a real choice.
                        #[watch]
                        set_visible: model.has_path_choice(),
                        connect_selected_notify[sender] => move |row| {
                            sender.input(WalletPageMsg::SelectReceivePath(row.selected()));
                        },
                    },

                    adw::ActionRow {
                        set_title: "Next address",
                        #[watch]
                        set_subtitle: &model.address(),
                        set_subtitle_lines: 2,

                        add_suffix = &gtk::Button {
                            set_icon_name: "view-refresh-symbolic",
                            set_tooltip_text: Some("New address — use a different one for each payer"),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                            connect_clicked => WalletPageMsg::NewAddress,
                        },

                        add_suffix = &gtk::Button {
                            set_icon_name: "edit-copy-symbolic",
                            set_tooltip_text: Some("Copy address"),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                            connect_clicked => WalletPageMsg::CopyAddress,
                        },
                    },
                },

                adw::PreferencesGroup {
                    #[watch]
                    set_visible: model.error.is_some(),

                    adw::ActionRow {
                        add_css_class: "error",
                        set_title: "Sync problem",
                        #[watch]
                        set_subtitle: model.error.as_deref().unwrap_or_default(),
                        set_subtitle_lines: 3,
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
        let paths = FactoryVecDeque::builder().launch_default().detach();
        let model = WalletPage {
            paths,
            settings: Settings::load(),
            path_model: gtk::StringList::new(&[]),
            path_labels: Vec::new(),
            receive_index: 0,
            fresh_address: None,
            summary: None,
            progress: Progress::Connecting,
            peers: None,
            note: None,
            error: None,
        };
        let paths_box = model.paths.widget();
        let path_model = model.path_model.clone();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            WalletPageMsg::SelectReceivePath(index) => {
                self.receive_index = index;
                // The fresh address belonged to the path being left.
                self.fresh_address = None;
            }
            WalletPageMsg::NewAddress => {
                if let Some(summary) = &self.summary
                    && let Some(account) = summary.accounts.get(self.receive_index as usize)
                {
                    let _ = sender.output(WalletPageOutput::NewAddress(account.script_type));
                }
            }
            WalletPageMsg::ShowFreshAddress(address) => self.fresh_address = Some(address),
            WalletPageMsg::ToggleDenomination => {
                self.settings.denomination = self.settings.denomination.toggled();
                self.settings.save();
                // The rows hold formatted text, so they are rebuilt.
                if let Some(summary) = self.summary.clone() {
                    self.rebuild_paths(&summary);
                }
            }
            WalletPageMsg::Reset => {
                self.summary = None;
                self.progress = Progress::Connecting;
                self.peers = None;
                self.note = None;
                self.error = None;
                self.receive_index = 0;
                self.fresh_address = None;
                self.path_labels.clear();
                self.path_model.splice(0, self.path_model.n_items(), &[]);
                self.paths.guard().clear();
            }
            WalletPageMsg::SwitchWallet => {
                let _ = sender.output(WalletPageOutput::SwitchWallet);
            }
            WalletPageMsg::Show(summary) => {
                // Rebuild rather than diff: four rows, and the set only changes
                // when a sync lands.
                self.sync_path_picker(&summary);
                self.rebuild_paths(&summary);
                self.summary = Some(summary);
            }
            WalletPageMsg::SetProgress(progress) => {
                self.progress = progress;
                self.error = None;
            }
            WalletPageMsg::Peers { connected, required } => {
                self.peers = Some((connected, required))
            }
            WalletPageMsg::Note(note) => self.note = Some(note),
            WalletPageMsg::Failed(message) => self.error = Some(message),
            WalletPageMsg::CopyAddress => {
                if let Some(summary) = &self.summary
                    && let Some(display) = gtk::gdk::Display::default()
                {
                    display.clipboard().set_text(&summary.next_address);
                }
            }
        }
    }
}

impl WalletPage {
    /// Rewrite the picker only when the set of paths actually changes, so a
    /// routine sync update cannot reset a selection someone just made.
    fn sync_path_picker(&mut self, summary: &Summary) {
        let labels: Vec<String> = summary
            .accounts
            .iter()
            .map(|a| a.script_type.to_string())
            .collect();
        if labels == self.path_labels {
            return;
        }

        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.path_model.splice(0, self.path_model.n_items(), &refs);
        self.path_labels = labels;

        // Default to the wallet's primary path rather than whatever happens to
        // be first: for an import that is Native SegWit, not Legacy.
        self.receive_index = summary
            .accounts
            .iter()
            .position(|a| a.next_address == summary.next_address)
            .unwrap_or(0) as u32;
    }

    /// Only paths holding something get a row; the rest are named in the group
    /// description instead.
    fn rebuild_paths(&mut self, summary: &Summary) {
        let mut guard = self.paths.guard();
        guard.clear();
        if summary.accounts.len() > 1 {
            for account in &summary.accounts {
                if account.balance_sats > 0 || account.pending_sats > 0 {
                    guard.push_back((account.clone(), self.settings.denomination));
                }
            }
        }
    }
}
