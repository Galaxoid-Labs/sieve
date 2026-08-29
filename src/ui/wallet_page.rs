//! The unlocked wallet: balance, receive address, and sync status.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::node::Progress;
use crate::settings::{Denomination, Settings};
use crate::wallet::Summary;

#[derive(Debug)]
pub enum WalletPageOutput {
    SwitchWallet,
    ShowPreferences,
    /// Ask for the password again — the wallet is on screen but locked.
    Unlock,
    /// Reveal a new address on this path.
    NewAddress(crate::wallet::accounts::ScriptType),
}

/// One transaction in the activity list.
#[derive(Debug)]
pub struct TxRow {
    txid: String,
    title: String,
    subtitle: String,
    amount: String,
    /// What that amount is worth now, if a price is on hand.
    fiat: Option<String>,
    incoming: bool,
}

#[derive(Debug)]
pub enum TxRowOutput {
    Selected(String),
}

#[relm4::factory(pub)]
impl FactoryComponent for TxRow {
    type Init = (crate::wallet::TxSummary, Denomination, u32, Option<crate::price::Price>);
    type Input = ();
    type Output = TxRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            set_title: &self.title,
            set_subtitle: &self.subtitle,
            set_activatable: true,

            add_prefix = &gtk::Image {
                set_icon_name: Some(if self.incoming {
                    "go-bottom-symbolic"
                } else {
                    "go-top-symbolic"
                }),
            },

            add_suffix = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_valign: gtk::Align::Center,
                set_halign: gtk::Align::End,

                gtk::Label {
                    set_halign: gtk::Align::End,
                    // set_css_classes replaces the whole list, so the weight
                    // has to be part of it rather than added separately.
                    // Direction carries the colour, so a glance at the column
                    // reads without parsing the sign.
                    set_css_classes: if self.incoming {
                        &["numeric", "heading", "success"]
                    } else {
                        &["numeric", "heading", "dim-label"]
                    },
                    set_label: &self.amount,
                },

                gtk::Label {
                    set_halign: gtk::Align::End,
                    add_css_class: "numeric",
                    add_css_class: "dim-label",
                    add_css_class: "caption",
                    set_visible: self.fiat.is_some(),
                    set_label: self.fiat.as_deref().unwrap_or_default(),
                },
            },

            // The row opens a page, so it carries the chevron that promises one.
            add_suffix = &gtk::Image {
                set_icon_name: Some("go-next-symbolic"),
                add_css_class: "dim-label",
            },

            connect_activated[sender, txid = self.txid.clone()] => move |_| {
                let _ = sender.output(TxRowOutput::Selected(txid.clone()));
            },
        }
    }

    fn init_model(
        (tx, denomination, tip, price): Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        let incoming = tx.is_incoming();
        let magnitude = tx.net_sats.unsigned_abs();
        let confirmations = tx.confirmations(tip);

        TxRow {
            title: if incoming { "Received".into() } else { "Sent".into() },
            subtitle: match (tx.height, confirmations) {
                // A filter wallet cannot see the mempool, so this is rare and
                // worth naming rather than showing a blank.
                (None, _) => "Unconfirmed".to_string(),
                // While a payment is shallow the confirmation count is the
                // thing being watched; after that, when it happened is.
                (Some(_), c) if c < 6 => {
                    format!("{} · {}", format_relative(tx.seen_at), plural_confirmations(c))
                }
                (Some(_), _) => format_relative(tx.seen_at),
            },
            amount: format!(
                "{}{}",
                if incoming { "+" } else { "−" },
                denomination.format(magnitude)
            ),
            incoming,
            fiat: price.map(|p| format!("≈ ${:.2}", p.value_of(magnitude))),
            txid: tx.txid,
        }
    }
}

fn plural_confirmations(n: u32) -> String {
    match n {
        0 => "Awaiting confirmation".into(),
        1 => "1 confirmation".into(),
        n => format!("{n} confirmations"),
    }
}

/// Group digits so six-figure block heights stay readable.
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How long ago, in the terms a person would use.
///
/// Recent payments are the ones being checked on, so they get relative time.
/// Past a week that stops meaning anything and it falls back to a date.
fn format_relative(seen_at: Option<u64>) -> String {
    let Some(seconds) = seen_at else { return "Confirmed".into() };
    let Ok(then) = gtk::glib::DateTime::from_unix_local(seconds as i64) else {
        return "Confirmed".into();
    };
    let Ok(now) = gtk::glib::DateTime::now_local() else {
        return format_when(seen_at);
    };

    let elapsed = now.difference(&then).as_seconds();
    match elapsed {
        e if e < 0 => "Just now".into(),
        e if e < 60 => "Just now".into(),
        e if e < 3_600 => format!("{} minutes ago", e / 60),
        e if e < 7_200 => "An hour ago".into(),
        e if e < 86_400 => format!("{} hours ago", e / 3_600),
        e if e < 172_800 => "Yesterday".into(),
        e if e < 604_800 => format!("{} days ago", e / 86_400),
        _ => format_when(seen_at),
    }
}

/// A date, or nothing if the node never told us when.
fn format_when(seen_at: Option<u64>) -> String {
    let Some(seconds) = seen_at else { return "Confirmed".into() };
    gtk::glib::DateTime::from_unix_local(seconds as i64)
        .and_then(|d| d.format("%e %b %Y"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "Confirmed".into())
}

pub struct WalletPage {
    summary: Option<Summary>,
    progress: Progress,
    peers: Option<(usize, usize)>,
    note: Option<String>,
    error: Option<String>,
    settings: Settings,
    price: Option<crate::price::Price>,
    /// The wallet is the root screen now, so it exists before anyone has
    /// proved they may look at it.
    locked: bool,
    name: String,
    transactions: FactoryVecDeque<TxRow>,
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
    /// Which unit amounts are shown in. Owned by the app, since the
    /// preferences dialog is where it is changed.
    SetDenomination(crate::settings::Denomination),
    /// `None` clears it — the setting was turned off, or the fetch failed.
    SetPrice(Option<crate::price::Price>),
    /// Choose which derivation path to receive on.
    SelectReceivePath(u32),
    /// Ask for an address that has not been handed to anyone yet.
    NewAddress,
    /// Open the detail sheet for one transaction.
    ShowTransaction(String),
    /// The freshly revealed address came back.
    ShowFreshAddress(String),
    /// Clear everything that belonged to a different wallet.
    Reset,
    SetLocked(bool),
    ShowPreferences,
    SetName(String),
    RequestUnlock,
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

    /// The balance in dollars, when a price is on hand.
    ///
    /// Approximate by construction — one exchange's last trade — so it is
    /// marked as such rather than presented as a second exact figure beside
    /// an exact one.
    fn fiat(&self) -> Option<String> {
        let price = self.price?;
        let summary = self.summary.as_ref()?;
        Some(format!("≈ ${:.2}", price.value_of(summary.balance_sats)))
    }

    /// The line under the balance: what qualifies the number above it.
    ///
    /// Pending is the important half — a filter wallet cannot see the mempool,
    /// so anything pending here is already mined but shallow.
    fn balance_caption(&self) -> String {
        let Some(summary) = &self.summary else { return "Not yet synced".into() };

        let mut parts = Vec::new();
        if summary.pending_sats > 0 {
            parts.push(format!("{} pending", self.pending()));
        }
        parts.push(match summary.transactions.len() {
            0 => "No transactions".into(),
            1 => "1 transaction".into(),
            n => format!("{n} transactions"),
        });
        parts.join(" · ")
    }

    fn has_transactions(&self) -> bool {
        self.summary.as_ref().is_some_and(|s| !s.transactions.is_empty())
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

    fn syncing(&self) -> bool {
        !matches!(self.progress, Progress::Synced)
    }
}

#[relm4::component(pub)]
impl Component for WalletPage {
    type Init = ();
    type Input = WalletPageMsg;
    type Output = WalletPageOutput;
    type CommandOutput = ();

    view! {
        adw::BreakpointBin {
            set_size_request: (360, 300),

            #[wrap(Some)]
            #[name(inner_nav)]
            set_child = &adw::NavigationView {

                #[name(main_page)]
                adw::NavigationPage {
                    set_tag: Some("main"),
                    set_title: "Wallet",

                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                #[name(header_bar)]
                add_top_bar = &adw::HeaderBar {
                // The window keeps its title. The switcher gets its own row
                // below, rather than displacing the thing that says which
                // wallet you are looking at.
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    #[watch]
                    set_title: &model.name,
                    #[watch]
                    set_subtitle: model.summary.as_ref().map_or("", |s| s.network.as_str()),
                },

                // Preferences belong in a dialog reached from the header, not
                // in the switcher beside the things you actually do with money.
                pack_end = &gtk::Button {
                    set_icon_name: "open-menu-symbolic",
                    set_tooltip_text: Some("Preferences"),
                    connect_clicked => WalletPageMsg::ShowPreferences,
                },
            },

            // Centred in a toolbar row of its own, directly under the header.
            // Hidden by the breakpoint when the bottom bar takes over.
            #[name(switcher_row)]
            add_top_bar = &gtk::Box {
                add_css_class: "toolbar",

                #[name(view_switcher)]
                adw::ViewSwitcher {
                    set_policy: adw::ViewSwitcherPolicy::Wide,
                    set_halign: gtk::Align::Center,
                    set_hexpand: true,
                },
            },

            // Sync state sits above every view rather than inside one: it
            // qualifies whatever number you happen to be looking at.
            add_top_bar = &adw::Banner {
                #[watch]
                set_revealed: model.syncing() && !model.locked,
                #[watch]
                set_title: &model.progress.label(),
            },

            #[wrap(Some)]
            #[name(view_stack)]
            set_content = &adw::ViewStack {

                add_titled_with_icon[Some("activity"), "Activity", "document-open-recent-symbolic"] =
                &gtk::ScrolledWindow {
                    set_vexpand: true,

                    adw::Clamp {
                        // Wider than Adwaita's 600 default: a transaction row
                        // is a wide thing — direction, description and amount
                        // across one line — and the balance above it wants
                        // room to breathe. Still clamped, so the list does not
                        // stretch to absurd line lengths on a large monitor.
                        set_maximum_size: 900,
                        set_tightening_threshold: 600,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            set_margin_all: 18,
                            set_valign: gtk::Align::Start,

                            adw::StatusPage {
                                set_icon_name: Some("channel-secure-symbolic"),
                                set_title: "Wallet locked",
                                set_description: Some(
                                    "Unlock to see balances and addresses."
                                ),
                                #[watch]
                                set_visible: model.locked,

                                #[wrap(Some)]
                                set_child = &gtk::Button {
                                    add_css_class: "suggested-action",
                                    add_css_class: "pill",
                                    set_halign: gtk::Align::Center,
                                    set_label: "Unlock",
                                    connect_clicked => WalletPageMsg::RequestUnlock,
                                },
                            },

                            // The balance is what people open a wallet to see,
                            // so it leads, in a card of its own rather than as
                            // the first item of an undifferentiated column.
                            gtk::Box {
                                add_css_class: "card",
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 2,
                                set_margin_top: 12,
                                #[watch]
                                set_visible: !model.locked,

                                gtk::Label {
                                    add_css_class: "title-1",
                                    add_css_class: "numeric",
                                    set_wrap: true,
                                    set_margin_top: 24,
                                    set_justify: gtk::Justification::Center,
                                    #[watch]
                                    set_label: &model.balance(),
                                },

                                gtk::Button {
                                    add_css_class: "flat",
                                    add_css_class: "dim-label",
                                    set_halign: gtk::Align::Center,
                                    set_tooltip_text: Some("Switch between BTC and satoshis"),
                                    #[watch]
                                    set_label: model.settings.denomination.label(),
                                    connect_clicked => WalletPageMsg::ShowPreferences,
                                },

                                gtk::Label {
                                    add_css_class: "title-4",
                                    add_css_class: "dim-label",
                                    add_css_class: "numeric",
                                    set_halign: gtk::Align::Center,
                                    #[watch]
                                    set_visible: model.fiat().is_some(),
                                    #[watch]
                                    set_label: &model.fiat().unwrap_or_default(),
                                },

                                gtk::Label {
                                    add_css_class: "dim-label",
                                    set_halign: gtk::Align::Center,
                                    set_margin_bottom: 24,
                                    set_wrap: true,
                                    set_justify: gtk::Justification::Center,
                                    #[watch]
                                    set_label: &model.balance_caption(),
                                },
                            },

                            gtk::Label {
                                add_css_class: "heading",
                                set_halign: gtk::Align::Start,
                                set_margin_top: 12,
                                set_label: "Transactions",
                                #[watch]
                                set_visible: model.has_transactions() && !model.locked,
                            },

                            #[local_ref]
                            tx_list -> gtk::ListBox {
                                add_css_class: "boxed-list",
                                set_selection_mode: gtk::SelectionMode::None,
                                set_margin_top: 12,
                                #[watch]
                                set_visible: model.has_transactions() && !model.locked,
                            },

                            adw::StatusPage {
                                set_icon_name: Some("document-open-recent-symbolic"),
                                set_title: "No transactions yet",
                                set_description: Some(
                                    "Payments appear here once they are mined. Sieve reads them \
                                     from block filters, so an unconfirmed payment stays invisible \
                                     until it confirms."
                                ),
                                #[watch]
                                set_visible: !model.has_transactions() && !model.locked,
                            },
                        },
                    },
                },

                add_titled_with_icon[Some("receive"), "Receive", "go-bottom-symbolic"] =
                &adw::PreferencesPage {

                    adw::PreferencesGroup {
                        set_title: "Receive",
                        #[watch]
                        set_description: Some(&model.address_hint()),

                        #[name(path_picker)]
                        adw::ComboRow {
                            set_title: "Address type",
                            // Model set once and mutated in place; see path_model.
                            set_model: Some(&path_model),
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
                },

                add_titled_with_icon[Some("send"), "Send", "go-top-symbolic"] =
                &adw::StatusPage {
                    set_icon_name: Some("go-top-symbolic"),
                    set_title: "Sending is not built yet",
                    set_description: Some(
                        "Sieve can watch this wallet but cannot spend from it. Nothing here \
                         can move your coins."
                    ),
                },

                add_titled_with_icon[Some("network"), "Network", "network-wireless-symbolic"] =
                &adw::PreferencesPage {

                    adw::PreferencesGroup {
                        set_title: "Sync",
                        set_description: Some(
                            "Sieve downloads compact block filters and matches them on this \
                             machine. No server is told which addresses are yours."
                        ),

                        adw::ActionRow {
                            set_title: "Status",
                            #[watch]
                            set_subtitle: &model.progress.label(),

                            add_suffix = &gtk::Spinner {
                                set_valign: gtk::Align::Center,
                                #[watch]
                                set_visible: model.syncing()
                                    && model.progress.fraction().is_none(),
                                #[watch]
                                set_spinning: model.syncing()
                                    && model.progress.fraction().is_none(),
                            },

                            add_suffix = &gtk::ProgressBar {
                                set_valign: gtk::Align::Center,
                                set_width_request: 120,
                                #[watch]
                                set_visible: model.syncing()
                                    && model.progress.fraction().is_some(),
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

            },

                // Revealed by the breakpoint below, when the header has no
                // room for the switcher.
                #[name(switcher_bar)]
                add_bottom_bar = &adw::ViewSwitcherBar {},
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
        let transactions = FactoryVecDeque::builder().launch_default().forward(
            sender.input_sender(),
            |out| match out {
                TxRowOutput::Selected(txid) => WalletPageMsg::ShowTransaction(txid),
            },
        );
        let model = WalletPage {
            settings: Settings::load(),
            locked: true,
            price: None,
            name: "Sieve".into(),
            transactions,
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
        let tx_list = model.transactions.widget();
        let path_model = model.path_model.clone();
        let widgets = view_output!();

        // Both switchers are declared above the stack they drive, so the links
        // are made once the whole tree exists.
        widgets.view_switcher.set_stack(Some(&widgets.view_stack));
        widgets.switcher_bar.set_stack(Some(&widgets.view_stack));

        // Switcher row under the header while there is room for it, bottom bar
        // once there is not, never both. The title stays put either way.
        match adw::BreakpointCondition::parse("max-width: 550sp") {
            Ok(condition) => {
                let breakpoint = adw::Breakpoint::new(condition);
                breakpoint.add_setter(&widgets.switcher_row, "visible", Some(&false.to_value()));
                breakpoint.add_setter(&widgets.switcher_bar, "reveal", Some(&true.to_value()));
                root.add_breakpoint(breakpoint);
            }
            Err(e) => tracing::warn!(%e, "could not parse the layout breakpoint"),
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            WalletPageMsg::ShowTransaction(txid) => self.show_transaction(&txid, root),
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
            WalletPageMsg::SetPrice(price) => {
                self.price = price;
                if let Some(summary) = self.summary.clone() {
                    self.rebuild_transactions(&summary);
                }
            }
            WalletPageMsg::SetDenomination(denomination) => {
                self.settings.denomination = denomination;
                // The rows hold formatted text, so they are rebuilt.
                if let Some(summary) = self.summary.clone() {
                        self.rebuild_transactions(&summary);
                }
            }
            WalletPageMsg::ShowPreferences => {
                let _ = sender.output(WalletPageOutput::ShowPreferences);
            }
            WalletPageMsg::SetLocked(locked) => self.locked = locked,
            WalletPageMsg::SetName(name) => self.name = name,
            WalletPageMsg::RequestUnlock => {
                let _ = sender.output(WalletPageOutput::Unlock);
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
                self.transactions.guard().clear();
            }
            WalletPageMsg::SwitchWallet => {
                let _ = sender.output(WalletPageOutput::SwitchWallet);
            }
            WalletPageMsg::Show(summary) => {
                // Rebuild rather than diff: four rows, and the set only changes
                // when a sync lands.
                self.sync_path_picker(&summary);
                self.rebuild_transactions(&summary);
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
    /// Rebuild the activity list. Rows carry formatted text, so they are
    /// rebuilt when the summary or the denomination changes.
    fn rebuild_transactions(&mut self, summary: &Summary) {
        let mut guard = self.transactions.guard();
        guard.clear();
        for tx in &summary.transactions {
            guard.push_back((tx.clone(), self.settings.denomination, summary.tip, self.price));
        }
    }

    /// Open one transaction as a page over the wallet.
    ///
    /// A transaction is a place you go and come back from, not a prompt you
    /// answer, so it pushes onto the wallet's own navigation rather than
    /// arriving as a dialog in front of it.
    fn show_transaction(&self, txid: &str, root: &adw::BreakpointBin) {
        let Some(summary) = &self.summary else { return };
        let Some(tx) = summary.transactions.iter().find(|t| t.txid == txid) else {
            return;
        };
        let Some(nav) = root
            .child()
            .and_then(|child| child.downcast::<adw::NavigationView>().ok())
        else {
            return;
        };

        let page = adw::PreferencesPage::new();
        let incoming = tx.is_incoming();
        let magnitude = tx.net_sats.unsigned_abs();

        // The amount leads, the way the balance leads the wallet.
        let headline = adw::PreferencesGroup::new();
        let amount = gtk::Label::new(Some(&format!(
            "{}{}",
            if incoming { "+" } else { "−" },
            self.settings.denomination.format(magnitude)
        )));
        amount.add_css_class("title-1");
        amount.add_css_class("numeric");
        if incoming {
            amount.add_css_class("success");
        }
        amount.set_wrap(true);
        amount.set_justify(gtk::Justification::Center);

        let caption = gtk::Label::new(Some(if incoming { "Received" } else { "Sent" }));
        caption.add_css_class("dim-label");

        let stack = gtk::Box::new(gtk::Orientation::Vertical, 4);
        stack.set_margin_bottom(12);
        stack.append(&amount);
        stack.append(&caption);

        if let Some(price) = self.price {
            // Current price against a past amount, so this is what the coins
            // are worth now rather than when they moved. The approximation
            // sign carries that; spelling it out read as clutter.
            let value = gtk::Label::new(Some(&format!(
                "≈ ${:.2}",
                price.value_of(magnitude)
            )));
            value.add_css_class("dim-label");
            value.add_css_class("numeric");
            stack.append(&value);
        }
        headline.add(&stack);
        page.add(&headline);

        let status = adw::PreferencesGroup::new();
        status.set_title("Status");
        match tx.height {
            Some(height) => {
                status.add(&detail_row(
                    "Confirmations",
                    &plural_confirmations(tx.confirmations(summary.tip)),
                ));
                status.add(&detail_row("Block", &thousands(height)));
                status.add(&detail_row("Date", &format_when(tx.seen_at)));
            }
            None => status.add(&detail_row(
                "Confirmations",
                "Unconfirmed — not yet in a block",
            )),
        }
        page.add(&status);

        let detail = adw::PreferencesGroup::new();
        detail.set_title("Detail");
        match tx.fee_sats {
            Some(fee) => detail.add(&detail_row("Fee", &self.settings.denomination.format(fee))),
            // Only knowable when every input is ours, which an incoming payment
            // built by someone else will not be.
            None => detail.add(&detail_row("Fee", "Not known — inputs are not all yours")),
        }
        detail.add(&detail_row("Derivation path", &tx.script_type.to_string()));

        let txid_row = adw::ActionRow::new();
        txid_row.set_title("Transaction ID");
        txid_row.set_subtitle(&tx.txid);
        txid_row.set_subtitle_lines(2);
        txid_row.add_css_class("property");
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.set_tooltip_text(Some("Copy transaction ID"));
        copy.set_valign(gtk::Align::Center);
        copy.add_css_class("flat");
        let to_copy = tx.txid.clone();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&to_copy);
            }
        });
        txid_row.add_suffix(&copy);
        detail.add(&txid_row);
        page.add(&detail);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));

        let nav_page = adw::NavigationPage::new(&toolbar, "Transaction");
        nav.push(&nav_page);
    }

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

}

/// A read-only label/value row, as used throughout the detail sheet.
fn detail_row(title: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(value);
    row.set_subtitle_lines(2);
    row
}
