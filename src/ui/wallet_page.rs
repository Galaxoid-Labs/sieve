//! The unlocked wallet: balance, receive address, and sync status.

use adw::prelude::*;
use relm4::abstractions::Toaster;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::node::Progress;
use crate::settings::{Denomination, Settings};
use crate::wallet::accounts::ScriptType;
use crate::wallet::Summary;
use crate::ui::send::{Password, SendForm, SendMsg, SendOutput};
use crate::wallet::send::{Draft, Plan};

#[derive(Debug)]
pub enum WalletPageOutput {
    SwitchWallet,
    RefreshChain,
    ShowPreferences,
    /// Ask for the password again — the wallet is on screen but locked.
    Unlock,
    /// Reveal a new address on this path.
    NewAddress(crate::wallet::accounts::ScriptType),
    /// The send form came into view and wants a fee rate to start from.
    EstimateFee,
    /// Try to bring Tor up again after it failed.
    RetryTor,
    /// Build a transaction, watch-only, and hand back what it would cost.
    PlanSend(Box<Draft>),
    /// Sign the reviewed transaction and broadcast it.
    Send { plan: Box<Plan>, password: Password },
}

/// One connected peer.
#[derive(Debug)]
pub struct PeerRow {
    address: String,
    serves_filters: Option<bool>,
}

#[relm4::factory(pub)]
impl FactoryComponent for PeerRow {
    type Init = crate::wallet::node::PeerInfo;
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            set_title: &self.address,
            set_title_lines: 1,
            // The distinction that matters: most of the network does not serve
            // filters, and a peer that does not cannot help this wallet sync.
            set_subtitle: match self.serves_filters {
                Some(true) => "Serves compact filters",
                Some(false) => "No compact filters",
                // Kyoto reports nothing for some connections; saying so beats
                // claiming the peer is useless when we simply do not know.
                None => "Services not reported",
            },

            add_prefix = &gtk::Image {
                set_icon_name: Some(match self.serves_filters {
                    Some(true) => "network-wireless-symbolic",
                    Some(false) => "network-offline-symbolic",
                    None => "network-idle-symbolic",
                }),
                set_css_classes: match self.serves_filters {
                    Some(true) => &["success"],
                    _ => &["dim-label"],
                },
            },
        }
    }

    fn init_model(
        peer: Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        PeerRow { address: peer.address, serves_filters: peer.serves_filters }
    }
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
    /// Broadcast but not in a block yet.
    pending: bool,
}

#[derive(Debug)]
pub enum TxRowOutput {
    Selected(String),
}

#[relm4::factory(pub)]
impl FactoryComponent for TxRow {
    type Init = (crate::wallet::TxSummary, Denomination, u32, Option<crate::price::Price>, String);
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
                    "sieve-receive-symbolic"
                } else {
                    "sieve-send-symbolic"
                }),
                // Accent, not warning: a payment waiting for a block is in
                // progress, not in trouble. It recolours itself with the
                // theme, which a hardcoded colour would not.
                set_css_classes: if self.pending { &["accent"] } else { &[] },
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
                    // Pending money is not settled money, so an incoming
                    // one does not get the confident green until it is in a
                    // block — the same rule coin selection now follows.
                    set_css_classes: if self.pending {
                        &["numeric", "heading", "dim-label"]
                    } else if self.incoming {
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
        (tx, denomination, tip, price, network): Self::Init,
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
                denomination.format(magnitude, &network)
            ),
            incoming,
            pending: tx.height.is_none(),
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
    /// Somewhere to say things worth saying once and not keeping.
    toaster: Toaster,
    chain: Option<crate::wallet::node::ChainInfo>,
    peers_list: FactoryVecDeque<PeerRow>,
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
    /// Which path the receive view is showing. By type, not by position: the
    /// account list is rebuilt on every sync and an index would drift.
    receive_path: Option<crate::wallet::accounts::ScriptType>,
    /// The view stack, so locking can put it back on the one view that has
    /// something to say while locked.
    stack: Option<adw::ViewStack>,
    /// The send form, which owns its own form/review/sent states.
    send: Controller<SendForm>,
    /// How connections are being made, when they go through Tor.
    tor: Option<String>,
    /// Why nothing is connecting, when Tor is on and could not be started.
    tor_problem: Option<String>,
    /// Machines, as opposed to connections. kyoto opens more than one
    /// connection to some peers, so the two numbers differ and saying only the
    /// first invites the reader to count the list and find it wrong.
    distinct_peers: usize,
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
    Toast(String),
    SetChain(Option<crate::wallet::node::ChainInfo>),
    RefreshChain,
    /// How connections are being made: `Some` when they go through Tor.
    SetTor(Option<String>),
    /// The connected peers, arriving faster than the chain view can.
    SetPeers(Vec<crate::wallet::node::PeerInfo>),
    /// Tor is on and could not be started, so nothing is connecting.
    TorProblem(Option<String>),
    RetryTor,
    /// From the send form, on its way to the app.
    PlanSend(Box<Draft>),
    SendNow { plan: Box<Plan>, password: Password },
    /// The send form is on screen and wants a fee rate to start from.
    EstimateFee,
    /// From the app, on its way back to the send form.
    FeeSuggestion(f64, String),
    Planned(Box<Result<Plan, String>>),
    Sent(Box<Result<String, String>>),
    /// Choose which derivation path to receive on.
    SelectReceivePath(u32),
    SelectPath(crate::wallet::accounts::ScriptType),
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
            Some(s) => self.settings.denomination.format(s.balance_sats, &s.network),
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
        self.receive_path
            .and_then(|path| summary.accounts.iter().find(|a| a.script_type == path))
            .map(|a| a.next_address.clone())
            .unwrap_or_else(|| summary.next_address.clone())
    }

    /// The code a camera reads. Regenerated whenever the address changes.
    fn qr(&self) -> Option<gtk::gdk::Texture> {
        let address = self.address();
        if address == "—" {
            return None;
        }
        super::qr::texture(&super::qr::payment_uri(&address))
    }

    /// Whether this path is one the wallet actually watches.
    fn has_path(&self, path: crate::wallet::accounts::ScriptType) -> bool {
        self.summary
            .as_ref()
            .is_some_and(|s| s.accounts.iter().any(|a| a.script_type == path))
    }

    fn path_selected(&self, path: crate::wallet::accounts::ScriptType) -> bool {
        match self.receive_path {
            Some(selected) => selected == path,
            None => self
                .summary
                .as_ref()
                .and_then(|s| s.accounts.iter().find(|a| a.next_address == s.next_address))
                .is_some_and(|a| a.script_type == path),
        }
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

    fn chain_tip(&self) -> String {
        match &self.chain {
            Some(c) => thousands(c.tip_height),
            None => "—".into(),
        }
    }

    /// How far the wallet has verified behind what the peers report. Zero is
    /// the answer people want; anything else says the sync is not finished.
    fn behind(&self) -> String {
        let (Some(chain), Some(summary)) = (&self.chain, &self.summary) else {
            return "—".into();
        };
        match chain.tip_height.saturating_sub(summary.tip) {
            0 => "Up to date with the network".into(),
            1 => "1 block behind".into(),
            n => format!("{} blocks behind", thousands(n)),
        }
    }

    fn last_block(&self) -> String {
        match self.chain.as_ref().and_then(|c| c.tip_time) {
            Some(_) => format_relative(self.chain.as_ref().and_then(|c| c.tip_time)),
            None => "—".into(),
        }
    }

    fn difficulty(&self) -> String {
        match &self.chain {
            Some(c) if c.difficulty > 0.0 => format!("{:.3} T", c.difficulty / 1e12),
            _ => "—".into(),
        }
    }

    /// Hashrate in the unit that keeps the number readable.
    fn hashrate(&self) -> String {
        let Some(chain) = &self.chain else { return "—".into() };
        if chain.hashrate <= 0.0 {
            return "—".into();
        }
        for (limit, unit) in [(1e21, "ZH/s"), (1e18, "EH/s"), (1e15, "PH/s"), (1e12, "TH/s")] {
            if chain.hashrate >= limit {
                return format!("{:.2} {unit}", chain.hashrate / limit);
            }
        }
        format!("{:.0} H/s", chain.hashrate)
    }

    fn block_pace(&self) -> String {
        match self.chain.as_ref().and_then(|c| c.mean_interval) {
            Some(seconds) => format!("{:.1} minutes between blocks", seconds / 60.0),
            None => "—".into(),
        }
    }

    /// Blocks left in this difficulty period, and where the pace points.
    fn retarget(&self) -> String {
        let Some(chain) = &self.chain else { return "—".into() };
        let blocks = format!("{} blocks away", thousands(chain.blocks_to_retarget));
        match chain.retarget_estimate {
            // A multiplier above one means the next period gets harder.
            Some(estimate) => {
                let change = (estimate - 1.0) * 100.0;
                format!("{blocks} · estimated {change:+.1}%")
            }
            None => blocks,
        }
    }

    /// What the Connection row says.
    ///
    /// Named plainly in both directions. "Direct" is the default and not a
    /// fault, but a peer on the other end of a direct connection learns this
    /// machine's address, and the row should not let that pass unsaid.
    fn connection_route(&self) -> String {
        match &self.tor {
            Some(route) => route.clone(),
            None => "Direct — the peers you connect to see your IP address".into(),
        }
    }

    fn min_relay_fee(&self) -> String {
        match self.chain.as_ref().and_then(|c| c.min_relay_fee) {
            Some(rate) => format!("{rate:.2} sat/vB"),
            None => "—".into(),
        }
    }

    fn peer_count(&self) -> String {
        let Some(chain) = &self.chain else { return "Connecting…".into() };
        let serving = chain
            .peers
            .iter()
            .filter(|p| p.serves_filters == Some(true))
            .count();
        // The count lives in the Sync group; this says the thing the list is
        // actually for.
        // Distinct machines, which is the number that says how spread out this
        // wallet's requests are — several connections to one node is not the
        // same as several nodes.
        let distinct = chain.peers.len();
        match (distinct, serving) {
            (d, 0) => format!("{d} distinct addresses"),
            (d, n) => format!("{d} distinct addresses · {n} serving compact filters"),
        }
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
        let selected = self
            .receive_path
            .and_then(|path| summary.accounts.iter().find(|a| a.script_type == path));
        let Some(account) =
            selected.or_else(|| summary.accounts.iter().find(|a| a.next_address == summary.next_address))
        else {
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
        // One reading, both numbers. The chain snapshot is preferred because
        // the node's NeedConnections warning stops firing once the target is
        // met, so a count taken from it freezes at the last value below the
        // target; before the first snapshot arrives, that warning is all there
        // is.
        let (connections, peers) = match &self.chain {
            Some(chain) => (chain.connections, chain.peers.len()),
            None => match self.peers {
                Some((connections, _)) => (connections, self.distinct_peers),
                None => return "Connecting…".into(),
            },
        };
        let required = crate::wallet::node::REQUIRED_PEERS as usize;

        // Connections and machines are two true numbers answering different
        // questions, and kyoto holds more than one connection to some peers.
        // Printing only the first over a list of the second reads as an error
        // in one of them.
        match peers {
            0 => format!("{connections} of {required} connections"),
            peers if peers == connections => format!("{peers} of {required} peers connected"),
            peers => format!("{peers} peers over {connections} connections"),
        }
    }

    fn pending(&self) -> String {
        match &self.summary {
            Some(s) if s.pending_sats > 0 => {
                self.settings.denomination.format(s.pending_sats, &s.network)
            }
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

            // The overlay wraps the whole navigation view rather than one
            // page's content: a toast raised while a transaction is open has
            // to float over that page, not underneath it.
            #[wrap(Some)]
            #[local_ref]
            set_child = toast_overlay -> adw::ToastOverlay {

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
                    // Nothing behind these until the password is in, and a
                    // button that opens an empty page reads as a broken app.
                    // Insensitive rather than hidden: the breakpoint owns
                    // this row's visibility, and two things setting one
                    // property fight.
                    #[watch]
                    set_sensitive: !model.locked,
                },
            },

            // Sync state sits above every view rather than inside one: it
            // qualifies whatever number you happen to be looking at.
            add_top_bar = &adw::Banner {
                #[watch]
                set_revealed: model.syncing() && !model.locked && model.tor_problem.is_none(),
                #[watch]
                set_title: &model.progress.label(),
            },

            // Tor was asked for and could not be had. Nothing is connecting,
            // which is the point: going out over the clear instead would be
            // the one thing this must never do quietly.
            add_top_bar = &adw::Banner {
                #[watch]
                set_revealed: model.tor_problem.is_some() && !model.locked,
                #[watch]
                set_title: model.tor_problem.as_deref().unwrap_or_default(),
                set_button_label: Some("Try again"),
                connect_button_clicked => WalletPageMsg::RetryTor,
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
                                    set_label: model.settings.denomination.label(
                        model.summary.as_ref().map_or("bitcoin", |s| s.network.as_str())
                    ),
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

                add_titled_with_icon[Some("receive"), "Receive", "sieve-receive-symbolic"] =
                &gtk::ScrolledWindow {
                    set_vexpand: true,

                    adw::Clamp {
                        set_maximum_size: 420,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 18,
                            set_margin_all: 18,
                            set_valign: gtk::Align::Center,

                            // A white card behind the code, in both themes. An
                            // inverted QR looks better in dark mode and scans
                            // worse, so the code keeps its own ground.
                            gtk::Box {
                                // Its own white ground, not the theme's card:
                                // a card is dark in dark mode, which is
                                // exactly when a QR code needs light.
                                add_css_class: "qr-ground",
                                set_halign: gtk::Align::Center,
                                set_valign: gtk::Align::Center,
                                set_overflow: gtk::Overflow::Hidden,
                                // Fixed, so the card does not resize with the
                                // code. A longer address needs a denser QR
                                // version, and sizing to the texture made the
                                // card jump every time the type changed.
                                set_size_request: (280, 280),

                                gtk::Picture {
                                    set_hexpand: true,
                                    set_vexpand: true,
                                    // Clipped to the rounded ground, or the
                                    // code's white square fills the corners
                                    // the radius is trying to cut away.
                                    set_overflow: gtk::Overflow::Hidden,
                                    set_content_fit: gtk::ContentFit::Contain,
                                    #[watch]
                                    set_paintable: model.qr().as_ref(),
                                },
                            },

                            // All four at once rather than hidden in a
                            // dropdown: which address type you are handing out
                            // is worth seeing without opening anything.
                            gtk::Box {
                                add_css_class: "linked",
                                set_halign: gtk::Align::Center,
                                #[watch]
                                set_visible: model.has_path_choice(),

                                #[name(path_legacy)]
                                gtk::ToggleButton {
                                    set_label: "Legacy",
                                    #[watch]
                                    set_visible: model.has_path(ScriptType::Legacy),
                                    #[watch]
                                    #[block_signal(legacy_toggled)]
                                    set_active: model.path_selected(ScriptType::Legacy),
                                    connect_toggled[sender] => move |button| {
                                        if button.is_active() {
                                            sender.input(WalletPageMsg::SelectPath(
                                                ScriptType::Legacy,
                                            ));
                                        }
                                    } @legacy_toggled,
                                },

                                #[name(path_nested)]
                                gtk::ToggleButton {
                                    set_label: "Nested",
                                    set_group: Some(&path_legacy),
                                    #[watch]
                                    set_visible: model.has_path(ScriptType::NestedSegwit),
                                    #[watch]
                                    #[block_signal(nested_toggled)]
                                    set_active: model.path_selected(ScriptType::NestedSegwit),
                                    connect_toggled[sender] => move |button| {
                                        if button.is_active() {
                                            sender.input(WalletPageMsg::SelectPath(
                                                ScriptType::NestedSegwit,
                                            ));
                                        }
                                    } @nested_toggled,
                                },

                                #[name(path_native)]
                                gtk::ToggleButton {
                                    set_label: "SegWit",
                                    set_group: Some(&path_legacy),
                                    #[watch]
                                    set_visible: model.has_path(ScriptType::NativeSegwit),
                                    #[watch]
                                    #[block_signal(native_toggled)]
                                    set_active: model.path_selected(ScriptType::NativeSegwit),
                                    connect_toggled[sender] => move |button| {
                                        if button.is_active() {
                                            sender.input(WalletPageMsg::SelectPath(
                                                ScriptType::NativeSegwit,
                                            ));
                                        }
                                    } @native_toggled,
                                },

                                #[name(path_taproot)]
                                gtk::ToggleButton {
                                    set_label: "Taproot",
                                    set_group: Some(&path_legacy),
                                    #[watch]
                                    set_visible: model.has_path(ScriptType::Taproot),
                                    #[watch]
                                    #[block_signal(taproot_toggled)]
                                    set_active: model.path_selected(ScriptType::Taproot),
                                    connect_toggled[sender] => move |button| {
                                        if button.is_active() {
                                            sender.input(WalletPageMsg::SelectPath(
                                                ScriptType::Taproot,
                                            ));
                                        }
                                    } @taproot_toggled,
                                },
                            },

                            gtk::Label {
                                add_css_class: "dim-label",
                                set_halign: gtk::Align::Center,
                                set_wrap: true,
                                set_justify: gtk::Justification::Center,
                                #[watch]
                                set_label: &model.address_hint(),
                            },

                            gtk::Label {
                                add_css_class: "monospace",
                                set_wrap: true,
                                set_wrap_mode: gtk::pango::WrapMode::WordChar,
                                set_selectable: true,
                                set_justify: gtk::Justification::Center,
                                set_valign: gtk::Align::Center,
                                // A taproot address is 62 characters where a
                                // legacy one is 34, so the label wraps to a
                                // different number of lines and everything
                                // below it moves. Wrap them all the same way
                                // and reserve the height of the longest.
                                set_max_width_chars: 32,
                                set_height_request: 66,
                                // Pango hyphenates at wrap points by default.
                                // A hyphen inside a displayed address is not
                                // cosmetic: someone reading it off the screen
                                // could copy the character into a payment.
                                set_attributes: Some(&unhyphenated),
                                #[watch]
                                set_label: &model.address(),
                            },

                            gtk::Box {
                                set_halign: gtk::Align::Center,
                                set_spacing: 12,

                                gtk::Button {
                                    add_css_class: "pill",
                                    add_css_class: "suggested-action",
                                    set_label: "Copy",
                                    connect_clicked => WalletPageMsg::CopyAddress,
                                },

                                gtk::Button {
                                    add_css_class: "pill",
                                    set_label: "New address",
                                    set_tooltip_text: Some(
                                        "Use a different address for each payer"
                                    ),
                                    connect_clicked => WalletPageMsg::NewAddress,
                                },
                            },
                        },
                    },
                },

                // The send form is its own component: it has a state
                // machine of its own — form, review, sent — and this file is
                // long enough. Filled in below, so it keeps its place in the
                // switcher between Receive and Network.
                #[name(send_slot)]
                add_titled_with_icon[Some("send"), "Send", "sieve-send-symbolic"] =
                &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                },

                add_titled_with_icon[Some("network"), "Network", "network-wireless-symbolic"] =
                &adw::PreferencesPage {

                    // First on the page, because it qualifies everything under
                    // it: the same sync means something different depending on
                    // whether the peers below can see where it came from.
                    adw::PreferencesGroup {
                        set_title: "Connection",

                        adw::ActionRow {
                            set_title: "Route",
                            #[watch]
                            set_subtitle: &model.connection_route(),
                            set_subtitle_lines: 2,

                            add_prefix = &gtk::Image {
                                #[watch]
                                set_icon_name: Some(if model.tor.is_some() {
                                    "channel-secure-symbolic"
                                } else {
                                    "network-wireless-symbolic"
                                }),
                                // Accent when covered, plain when not: this is
                                // a statement of fact, not a warning — going
                                // direct is the default and not a fault.
                                #[watch]
                                set_css_classes: if model.tor.is_some() {
                                    &["accent"]
                                } else {
                                    &["dim-label"]
                                },
                            },
                        },
                    },

                    adw::PreferencesGroup {
                        set_title: "Sync",
                        set_description: Some(
                            "Sieve downloads compact block filters and matches them on this \
                             machine. No server is told which addresses are yours."
                        ),

                        #[wrap(Some)]
                        set_header_suffix = &gtk::Button {
                            set_icon_name: "view-refresh-symbolic",
                            set_tooltip_text: Some("Refresh"),
                            add_css_class: "flat",
                            set_valign: gtk::Align::Center,
                            connect_clicked => WalletPageMsg::RefreshChain,
                        },

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

                    // Everything below is read out of the header chain this
                    // node already holds. None of it asks anyone a new
                    // question, so none of it costs a disclosure.
                    adw::PreferencesGroup {
                        set_title: "Chain",

                        adw::ActionRow {
                            set_title: "Tip",
                            #[watch]
                            set_subtitle: &model.behind(),

                            add_suffix = &gtk::Label {
                                add_css_class: "numeric",
                                add_css_class: "heading",
                                #[watch]
                                set_label: &model.chain_tip(),
                            },
                        },

                        adw::ActionRow {
                            set_title: "Last block",
                            #[watch]
                            set_subtitle: &model.last_block(),
                        },

                        adw::ActionRow {
                            set_title: "Recent pace",
                            #[watch]
                            set_subtitle: &model.block_pace(),
                        },
                    },

                    adw::PreferencesGroup {
                        set_title: "Difficulty",

                        adw::ActionRow {
                            set_title: "Current",
                            set_subtitle: "Work needed per block",

                            add_suffix = &gtk::Label {
                                add_css_class: "numeric",
                                #[watch]
                                set_label: &model.difficulty(),
                            },
                        },

                        adw::ActionRow {
                            set_title: "Network hashrate",
                            set_subtitle: "Implied by the difficulty",

                            add_suffix = &gtk::Label {
                                add_css_class: "numeric",
                                #[watch]
                                set_label: &model.hashrate(),
                            },
                        },

                        adw::ActionRow {
                            set_title: "Next adjustment",
                            #[watch]
                            set_subtitle: &model.retarget(),
                        },
                    },

                    adw::PreferencesGroup {
                        set_title: "Fees",

                        adw::ActionRow {
                            set_title: "Minimum relay fee",
                            set_subtitle: "The lowest your peers will forward",

                            add_suffix = &gtk::Label {
                                add_css_class: "numeric",
                                #[watch]
                                set_label: &model.min_relay_fee(),
                            },
                        },
                    },

                    adw::PreferencesGroup {
                        set_title: "Peers",
                        #[watch]
                        set_description: Some(&model.peer_count()),

                        #[local_ref]
                        peers_box -> gtk::ListBox {
                            add_css_class: "boxed-list",
                            set_selection_mode: gtk::SelectionMode::None,
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
                add_bottom_bar = &adw::ViewSwitcherBar {
                    #[watch]
                    set_sensitive: !model.locked,
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
        let peers_list = FactoryVecDeque::builder().launch_default().detach();
        let transactions = FactoryVecDeque::builder().launch_default().forward(
            sender.input_sender(),
            |out| match out {
                TxRowOutput::Selected(txid) => WalletPageMsg::ShowTransaction(txid),
            },
        );
        let send = SendForm::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                SendOutput::Plan(draft) => WalletPageMsg::PlanSend(draft),
                SendOutput::Send { plan, password } => WalletPageMsg::SendNow { plan, password },
                SendOutput::Toast(message) => WalletPageMsg::Toast(message),
            },
        );

        let mut model = WalletPage {
            settings: Settings::load(),
            stack: None,
            send,
            tor: None,
            tor_problem: None,
            distinct_peers: 0,
            locked: true,
            price: None,
            toaster: Toaster::default(),
            chain: None,
            peers_list,
            name: "Sieve".into(),
            transactions,
            path_model: gtk::StringList::new(&[]),
            path_labels: Vec::new(),
            receive_index: 0,
            receive_path: None,
            fresh_address: None,
            summary: None,
            progress: Progress::Connecting,
            peers: None,
            note: None,
            error: None,
        };
        let tx_list = model.transactions.widget();
        let toast_overlay = model.toaster.overlay_widget();

        // Wrapping must not put a hyphen inside an address.
        let unhyphenated = gtk::pango::AttrList::new();
        unhyphenated.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
        let peers_box = model.peers_list.widget();
        let path_model = model.path_model.clone();
        let widgets = view_output!();

        // Both switchers are declared above the stack they drive, so the links
        // are made once the whole tree exists.
        model.stack = Some(widgets.view_stack.clone());
        widgets.send_slot.append(model.send.widget());

        // Both fee sources cost something — a block download or a disclosure —
        // so neither happens until the form is actually on screen.
        widgets.view_stack.connect_visible_child_name_notify({
            let sender = sender.clone();
            move |stack| {
                if stack.visible_child_name().as_deref() == Some("send") {
                    sender.input(WalletPageMsg::EstimateFee);
                }
            }
        });
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
            WalletPageMsg::ShowTransaction(txid) => {
                self.show_transaction(&txid, root, &sender)
            }
            WalletPageMsg::SelectPath(path) => {
                self.receive_path = Some(path);
                // The fresh address belonged to the path being left.
                self.fresh_address = None;
            }
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
            WalletPageMsg::Toast(message) => self.toaster.add_toast(adw::Toast::new(&message)),
            WalletPageMsg::RefreshChain => {
                let _ = sender.output(WalletPageOutput::RefreshChain);
            }
            WalletPageMsg::SetChain(chain) => {
                if let Some(info) = &chain {
                    self.distinct_peers = info.peers.len();
                    let mut guard = self.peers_list.guard();
                    guard.clear();
                    for peer in &info.peers {
                        guard.push_back(peer.clone());
                    }
                }
                // What peers will relay is the floor under the fee field.
                self.send
                    .emit(SendMsg::SetMinFee(chain.as_ref().and_then(|c| c.min_relay_fee)));
                self.chain = chain;
            }
            WalletPageMsg::SetTor(tor) => self.tor = tor,

            WalletPageMsg::TorProblem(problem) => self.tor_problem = problem,
            WalletPageMsg::RetryTor => {
                let _ = sender.output(WalletPageOutput::RetryTor);
            }

            WalletPageMsg::SetPeers(peers) => {
                self.distinct_peers = peers.len();
                let mut guard = self.peers_list.guard();
                guard.clear();
                for peer in peers {
                    guard.push_back(peer);
                }
            }

            WalletPageMsg::SetPrice(price) => {
                self.send.emit(SendMsg::SetPrice(price));
                self.price = price;
                if let Some(summary) = self.summary.clone() {
                    self.rebuild_transactions(&summary);
                }
            }
            WalletPageMsg::SetDenomination(denomination) => {
                self.settings.denomination = denomination;
                self.send.emit(SendMsg::SetDenomination(denomination));
                // The rows hold formatted text, so they are rebuilt.
                if let Some(summary) = self.summary.clone() {
                        self.rebuild_transactions(&summary);
                }
            }
            WalletPageMsg::ShowPreferences => {
                let _ = sender.output(WalletPageOutput::ShowPreferences);
            }
            WalletPageMsg::SetLocked(locked) => {
                self.locked = locked;
                // Whatever was on screen when the wallet locked — a receive
                // address, the chain view — belongs to a wallet nobody has
                // proved they may look at. Back to the view that says so.
                if locked && let Some(stack) = &self.stack {
                    stack.set_visible_child_name("activity");
                }
            }
            WalletPageMsg::SetName(name) => self.name = name,
            WalletPageMsg::RequestUnlock => {
                let _ = sender.output(WalletPageOutput::Unlock);
            }
            WalletPageMsg::Reset => {
                // A half-filled payment belongs to the wallet being left.
                self.send.emit(SendMsg::Reset);
                self.summary = None;
                self.progress = Progress::Connecting;
                self.peers = None;
                self.distinct_peers = 0;
                self.note = None;
                self.error = None;
                self.receive_index = 0;
                self.receive_path = None;
                self.fresh_address = None;
                self.path_labels.clear();
                self.path_model.splice(0, self.path_model.n_items(), &[]);
                self.transactions.guard().clear();
                // The chain belongs to a network, not just a wallet: leaving
                // it up meant a signet wallet briefly showing mainnet's
                // difficulty and peers.
                self.chain = None;
                self.peers_list.guard().clear();
                self.price = None;
            }
            WalletPageMsg::SwitchWallet => {
                let _ = sender.output(WalletPageOutput::SwitchWallet);
            }
            WalletPageMsg::Show(summary) => {
                // Rebuild rather than diff: four rows, and the set only changes
                // when a sync lands.
                self.sync_path_picker(&summary);
                self.rebuild_transactions(&summary);
                self.send.emit(SendMsg::Show(Box::new(summary.clone())));
                self.summary = Some(summary);
            }

            WalletPageMsg::PlanSend(draft) => {
                let _ = sender.output(WalletPageOutput::PlanSend(draft));
            }
            WalletPageMsg::SendNow { plan, password } => {
                let _ = sender.output(WalletPageOutput::Send { plan, password });
            }
            WalletPageMsg::EstimateFee => {
                let _ = sender.output(WalletPageOutput::EstimateFee);
            }
            WalletPageMsg::FeeSuggestion(rate, source) => {
                self.send.emit(SendMsg::Suggest { rate, source });
            }
            WalletPageMsg::Planned(result) => self.send.emit(SendMsg::Planned(result)),
            WalletPageMsg::Sent(result) => {
                if let Ok(txid) = result.as_ref() {
                    let short: String = txid.chars().take(12).collect();
                    self.toaster
                        .add_toast(adw::Toast::new(&format!("Payment sent — {short}…")));
                }
                self.send.emit(SendMsg::Sent(result));
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
                // self.address(), not the summary's: the picker and the
                // refresh button both change which address is on screen, and
                // copying a different one than is displayed is its own kind of
                // wrong even when both belong to this wallet.
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&self.address());
                    sender.input(WalletPageMsg::Toast("Address copied".into()));
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
            guard.push_back((
                    tx.clone(),
                    self.settings.denomination,
                    summary.tip,
                    self.price,
                    summary.network.clone(),
                ));
        }
    }

    /// Open one transaction as a page over the wallet.
    ///
    /// A transaction is a place you go and come back from, not a prompt you
    /// answer, so it pushes onto the wallet's own navigation rather than
    /// arriving as a dialog in front of it.
    fn show_transaction(
        &self,
        txid: &str,
        root: &adw::BreakpointBin,
        sender: &ComponentSender<Self>,
    ) {
        let Some(summary) = &self.summary else { return };
        let Some(tx) = summary.transactions.iter().find(|t| t.txid == txid) else {
            return;
        };
        // BreakpointBin -> ToastOverlay -> NavigationView.
        let Some(nav) = root
            .child()
            .and_then(|child| child.downcast::<adw::ToastOverlay>().ok())
            .and_then(|overlay| overlay.child())
            .and_then(|child| child.downcast::<adw::NavigationView>().ok())
        else {
            tracing::warn!("could not find the wallet's navigation view");
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
            self.settings.denomination.format(magnitude, &summary.network)
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
            Some(fee) => detail.add(&detail_row(
                "Fee",
                &self.settings.denomination.format(fee, &summary.network),
            )),
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

        if let Some(url) = explorer_url(&summary.network, &tx.txid) {
        let elsewhere = adw::PreferencesGroup::new();
        let explorer = adw::ActionRow::new();
        explorer.set_title("View on mempool.space");
        // Said plainly for the same reason the price switch says it: this is a
        // disclosure, and a bigger one than a price lookup, because it names
        // the transaction rather than only the fact that a wallet was opened.
        explorer.set_subtitle("Opens your browser, and tells the explorer you looked at this transaction");
        explorer.set_subtitle_lines(2);
        explorer.set_activatable(true);
        explorer.add_suffix(&gtk::Image::from_icon_name("web-browser-symbolic"));

        let launcher_parent = root.clone();
        let sender = sender.clone();
        explorer.connect_activated(move |_| {
            let sender = sender.clone();
            crate::ui::browser::open(&url, &launcher_parent, move |message| {
                sender.input(WalletPageMsg::Toast(message));
            });
        });

        elsewhere.add(&explorer);
        page.add(&elsewhere);
        }

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

/// Where to look a transaction up.
///
/// mempool.space serves mainnet at the root and test networks under a prefix.
/// Getting this wrong sends someone to a page that says the transaction does
/// not exist, which reads as "your wallet is lying" rather than "wrong site".
pub(crate) fn explorer_url(network: &str, txid: &str) -> Option<String> {
    match network {
        "bitcoin" => Some(format!("https://mempool.space/tx/{txid}")),
        // The test networks mempool.space actually serves.
        "signet" | "testnet" | "testnet4" => {
            Some(format!("https://mempool.space/{network}/tx/{txid}"))
        }
        // Regtest is a chain on this machine; no public explorer can see it,
        // and offering a link that always says "not found" is worse than
        // offering none.
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_urls_match_the_network() {
        // Mainnet sits at the root; everything else is prefixed. Sending
        // someone to the wrong one shows "transaction not found", which reads
        // as the wallet lying rather than the link being wrong.
        assert_eq!(
            explorer_url("bitcoin", "abc123").as_deref(),
            Some("https://mempool.space/tx/abc123")
        );
        assert_eq!(
            explorer_url("signet", "abc123").as_deref(),
            Some("https://mempool.space/signet/tx/abc123")
        );
        assert_eq!(
            explorer_url("testnet", "abc123").as_deref(),
            Some("https://mempool.space/testnet/tx/abc123")
        );
        assert_eq!(
            explorer_url("testnet4", "abc123").as_deref(),
            Some("https://mempool.space/testnet4/tx/abc123")
        );

        // No public explorer can see a chain running on this machine.
        assert_eq!(explorer_url("regtest", "abc123"), None);
    }
}
