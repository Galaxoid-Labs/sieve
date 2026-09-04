//! The unlocked wallet: balance, receive address, and sync status.

use adw::prelude::*;
use relm4::abstractions::Toaster;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::settings::{Denomination, Settings};
use crate::ui::send::{Password, SendForm, SendMsg, SendOutput};
use crate::wallet::Summary;
use crate::wallet::accounts::ScriptType;
use crate::wallet::node::Progress;
use crate::wallet::send::{Draft, Plan};

#[derive(Debug)]
pub enum WalletPageOutput {
    /// Throw away this wallet's chain data and scan again from its birthday.
    /// The app owns the node, so it owns the confirmation too.
    AskRescan,
    /// Look on the derivation paths this wallet is not watching.
    AskSearchPaths,
    /// Somebody typed the words: show this wallet's receive address, and
    /// keep showing it. Recorded against the wallet, not the session.
    AcknowledgeReceive,
    /// Name a transaction or an address, or clear its name. The app owns the
    /// label file, so it does the writing.
    SetLabel {
        kind: crate::wallet::labels::Kind,
        reference: String,
        text: String,
    },
    /// Hold a coin back, or release it. The app owns the label file, so it does
    /// the writing — the same route a name takes.
    SetFrozen {
        outpoint: String,
        frozen: bool,
    },
    /// Save a reviewed payment for a signer elsewhere. The app owns the file
    /// dialogs, as it does for labels and descriptors.
    SaveUnsigned(Box<crate::wallet::send::Plan>),
    /// Sign it on the device this wallet came from, then broadcast.
    SignOnDevice(Box<crate::wallet::send::Plan>),
    /// Show a receive address on the device's own screen, so the two can be
    /// compared. Sieve never reports the result: it cannot see the device's
    /// screen, and a tick it drew itself would be exactly the reassurance
    /// the attack this defends against needs.
    VerifyAddress {
        path: crate::wallet::accounts::ScriptType,
        index: u32,
    },
    /// Announce an unconfirmed transaction to another peer.
    Rebroadcast {
        txid: String,
        from: crate::wallet::accounts::ScriptType,
    },
    /// Rebuild an unconfirmed payment at a higher fee — either paying the
    /// same people, or paying nobody, which is what cancelling one means.
    PlanBump {
        txid: String,
        from: crate::wallet::accounts::ScriptType,
        fee_rate: f64,
        cancel: bool,
    },
    ShowPreferences,
    /// Ask for the password again — the wallet is on screen but locked.
    Unlock,
    /// Reveal a new address on this path.
    NewAddress(crate::wallet::accounts::ScriptType),
    /// The send form came into view and wants a fee rate to start from.
    EstimateFee,
    /// Read the block at the tip, for the Network tab's chain rows.
    ReadTipBlock,
    /// Try to bring Tor up again after it failed.
    RetryTor,
    /// Build a transaction, watch-only, and hand back what it would cost.
    PlanSend(Box<Draft>),
    /// Sign the reviewed transaction and broadcast it.
    Send {
        plan: Box<Plan>,
        password: Password,
        passphrase: Password,
    },
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
                // kyoto reports nothing for many connections, and during
                // the header stage it has not asked: headers come from any
                // peer at all.
                None => "Has not said what it serves",
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

    fn init_model(peer: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        PeerRow {
            address: peer.address,
            serves_filters: peer.serves_filters,
        }
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
    type Init = (
        crate::wallet::TxSummary,
        Denomination,
        u32,
        Option<crate::price::Price>,
        String,
        // Whether to name the derivation path: only useful on a wallet that
        // watches more than one.
        bool,
        // What this payment was for, when it has been named.
        Option<String>,
        // Whether amounts are being kept off the screen.
        bool,
    );
    type Input = ();
    type Output = TxRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            // Markup, so the direction and the name given to a payment are not
            // the same text in the same weight.
            set_use_markup: true,
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
        (tx, denomination, tip, price, network, show_path, label, hidden): Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        let incoming = tx.is_incoming();
        let magnitude = tx.net_sats.unsigned_abs();
        let confirmations = tx.confirmations(tip);

        let when = match (tx.height, confirmations) {
            // A filter wallet cannot see the mempool, so this is rare and
            // worth naming rather than showing a blank.
            (None, _) => "Unconfirmed".to_string(),
            // While a payment is shallow the confirmation count is the thing
            // being watched; after that, when it happened is.
            (Some(_), c) if c < 6 => {
                format!(
                    "{} · {}",
                    format_relative(tx.seen_at),
                    plural_confirmations(c)
                )
            }
            (Some(_), _) => format_relative(tx.seen_at),
        };
        // A payment whose fee has already been raised says so on the row,
        // because the alternative is opening it and raising it again on one
        // that is simply waiting its turn.
        let when = if tx.replaces.is_empty() {
            when
        } else {
            format!("{when} · fee raised")
        };

        TxRow {
            // Direction first, always: which way the money went is what the
            // row is for, and a label answers a different question. The name
            // joins it rather than replacing it.
            title: {
                let direction = if incoming { "Received" } else { "Sent" };
                match &label {
                    // The *name* is weighted, not the direction dimmed. Dimming
                    // the direction made "Sent" look one way on a named payment
                    // and another on an unnamed one, which is most of them —
                    // the same word changing appearance for a reason that has
                    // nothing to do with it. This way the direction reads
                    // identically on every row and the name is what stands out,
                    // which is right: the name is the only thing here that is
                    // not already said by the icon and by the amount's colour.
                    Some(label) => format!(
                        "{direction} · <b>{}</b>",
                        gtk::glib::markup_escape_text(label)
                    ),
                    None => direction.to_owned(),
                }
            },
            subtitle: if show_path {
                format!("{when} · {}", tx.script_type.label())
            } else {
                when
            },
            // The sign stays: which way the money went is what the row is
            // for, and hiding the amount should not hide the direction.
            amount: if hidden {
                format!("{}{}", if incoming { "+" } else { "−" }, WalletPage::HIDDEN)
            } else {
                format!(
                    "{}{}",
                    if incoming { "+" } else { "−" },
                    denomination.format(magnitude, &network)
                )
            },
            incoming,
            pending: tx.height.is_none(),
            fiat: (!hidden)
                .then(|| price.map(|p| format!("≈ ${}", crate::price::usd(p.value_of(magnitude)))))
                .flatten(),
            txid: tx.txid,
        }
    }
}

/// "1 coin", "3 coins".
/// Said as an offer, not a question: an address works perfectly well without
/// a name, and the field must not read as something owed before you can use it.
const UNLABELLED_TX: &str = "Optional — say what this payment was for, so it still \
                             means something in a year";

const UNNAMED_ADDRESS: &str = "Optional — name who this address is for, so their \
                               payment is recognisable when it arrives";

/// The navigation stack pages are pushed onto: BreakpointBin → ToastOverlay →
/// NavigationView. Written once, because two pages now need it and a second
/// copy of this walk would be a second thing to get wrong.
fn navigation_view(root: &adw::BreakpointBin) -> Option<adw::NavigationView> {
    let found = root
        .child()
        .and_then(|child| child.downcast::<adw::ToastOverlay>().ok())
        .and_then(|overlay| overlay.child())
        .and_then(|child| child.downcast::<adw::NavigationView>().ok());
    if found.is_none() {
        tracing::warn!("could not find the wallet's navigation view");
    }
    found
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// An identifier as Pango markup, in a monospaced face.
///
/// Addresses, transaction ids and block hashes are read character by character
/// or not at all, and a proportional face makes `1`/`l` and `0`/`O` the same
/// shape. Escaped, because an address arrives from outside.
fn mono(text: &str) -> String {
    format!("<tt>{}</tt>", gtk::glib::markup_escape_text(text))
}

/// Whether a transaction answers what was typed into the search box.
///
/// Matched against everything a person might have in front of them: what it
/// paid, who it paid, its own id, and the name they gave it. Case-insensitive,
/// and anywhere in the value rather than only at the start — an address is
/// usually copied from somewhere that shows only its middle, and the last
/// characters of a transaction id are as identifying as the first.
///
/// The amount is matched twice: once as it is written on screen, so what is
/// visible can be typed back, and once as plain satoshis, because that is the
/// number a person reads off a block explorer.
fn matches_search(
    tx: &crate::wallet::TxSummary,
    label: Option<&str>,
    query: &str,
    unit: crate::settings::Denomination,
    network: &str,
) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let has = |value: &str| value.to_lowercase().contains(&query);

    if has(&tx.txid) || label.is_some_and(has) {
        return true;
    }
    if tx.paid_to.iter().any(|(address, _)| has(address))
        || tx.paid_to_self.iter().any(|out| has(&out.address))
    {
        return true;
    }
    // What it published, if anything: a message is a thing people look for.
    if tx.data.iter().any(|(text, hex)| has(text) || has(hex)) {
        return true;
    }

    let magnitude = tx.net_sats.unsigned_abs();
    has(&unit.format(magnitude, network)) || has(&magnitude.to_string())
}

/// An address with an amount beside it, monospaced and wrapped without
/// hyphens — the same treatment addresses get everywhere else, because a
/// wrong character in an address is money.
fn address_row(address: &str, amount: &str, name: Option<&str>) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    // A name in front of the amount, when the address has one: "Alice · 0.01"
    // reads as a payment, where a bare address reads as a puzzle.
    row.set_title(&match name {
        Some(name) => format!("{} · {amount}", gtk::glib::markup_escape_text(name)),
        None => amount.to_owned(),
    });
    // Markup rather than a css class on the row: the class would take the
    // amount in the title with it.
    row.set_use_markup(true);
    row.set_subtitle(&mono(address));
    row.set_subtitle_lines(3);
    row.add_css_class("property");

    let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy.set_tooltip_text(Some("Copy address"));
    copy.set_valign(gtk::Align::Center);
    copy.add_css_class("flat");
    let to_copy = address.to_string();
    copy.connect_clicked(move |_| {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&to_copy);
        }
    });
    row.add_suffix(&copy);
    row
}

/// The same, for an address of this wallet's own, which can also say which key
/// produced it. The index is what makes one of your addresses distinguishable
/// from another, so it is shown rather than left to be worked out.
fn own_address_row(
    out: &crate::wallet::OwnOutput,
    amount: &str,
    name: Option<&str>,
) -> adw::ActionRow {
    let row = address_row(&out.address, amount, name);
    if let Some(path) = &out.path {
        // The address is the thing being checked; the path only says which of
        // your keys produced it, so it sits under the address and steps back.
        row.set_subtitle(&format!(
            "{}\n<span size=\"small\" alpha=\"60%\">{}</span>",
            mono(&out.address),
            mono(path)
        ));
        row.set_subtitle_lines(4);
    }
    row
}

fn plural_confirmations(n: u32) -> String {
    match n {
        0 => "Awaiting confirmation".into(),
        1 => "1 confirmation".into(),
        n => format!("{n} confirmations"),
    }
}

/// Group digits so six-figure block heights stay readable.
/// How far through the header chain a sync has got, against an estimated tip.
///
/// Never quite full: the estimate can be short, and a bar that sits at 100%
/// while work continues is worse than one that stops at 99.
fn header_fraction(height: u32, from: u32, tip: u32) -> Option<f64> {
    if tip <= from || height <= from {
        return None;
    }
    Some((f64::from(height - from) / f64::from(tip - from)).clamp(0.0, 0.99))
}

pub(crate) fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the block at the tip held, in one line.
///
/// Fees and fullness together, because the fee number on the Send screen is an
/// average across this block and means something different depending on how
/// full it was — the same average out of a third-full block is a quiet
/// network, and out of a full one it is a busy one.
fn tip_block_shape(
    block: &crate::wallet::node::TipBlock,
    unit: crate::settings::Denomination,
    network: &str,
) -> String {
    let fees = unit.format(block.total_fees_sat, network);
    format!(
        "{} transactions · {:.0}% of a full block · {} in fees",
        block.transactions,
        block.weight_used * 100.0,
        fees,
    )
}

/// How long ago, in the terms a person would use.
///
/// Recent payments are the ones being checked on, so they get relative time.
/// Past a week that stops meaning anything and it falls back to a date.
fn format_relative(seen_at: Option<u64>) -> String {
    let Some(seconds) = seen_at else {
        return "Confirmed".into();
    };
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
/// "40 minutes", "2 hours" — a gap, not a moment.
fn short_duration(seconds: u64) -> String {
    match seconds {
        0..=90 => format!("{seconds} seconds"),
        91..=5_400 => format!("{} minutes", seconds / 60),
        _ => format!("{} hours", seconds / 3_600),
    }
}

fn format_when(seen_at: Option<u64>) -> String {
    let Some(seconds) = seen_at else {
        return "Confirmed".into();
    };
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
    /// The block at the tip, as last read. Free with the fee estimate.
    tip_block: Option<crate::wallet::node::TipBlock>,
    peers_list: FactoryVecDeque<PeerRow>,
    /// The wallet is the root screen now, so it exists before anyone has
    /// proved they may look at it.
    locked: bool,
    name: String,
    transactions: FactoryVecDeque<TxRow>,
    /// The set of paths this wallet watches, as last seen.
    ///
    /// Only a change detector: the receive picker is four toggle buttons
    /// rather than a model-backed list, and what this guards is the default
    /// selection, which must not be recomputed on every sync.
    path_labels: Vec<String>,
    /// Which derivation path the activity list is limited to, or `None` for
    /// all of them together, which is what it opens on.
    activity_path: Option<crate::wallet::accounts::ScriptType>,
    /// What was typed into the search box. Empty shows everything.
    search: String,
    /// What each row of the activity filter means, by position. The source of
    /// truth for reading a selection back, since the model holds only labels.
    activity_choices: Vec<Option<crate::wallet::accounts::ScriptType>>,
    /// Backing model for that filter, held and spliced rather than rebuilt:
    /// swapping a dropdown's model resets its selection, and the selection is
    /// the filter.
    activity_model: gtk::StringList,
    receive_index: u32,
    /// Which path the receive view is showing. By type, not by position: the
    /// account list is rebuilt on every sync and an index would drift.
    receive_path: Option<crate::wallet::accounts::ScriptType>,
    /// The view stack, so locking can put it back on the one view that has
    /// something to say while locked.
    stack: Option<adw::ViewStack>,
    /// The receive screen's name field, held for the same reason the stack is:
    /// its text follows the address on show, and a `#[watch]` would rewrite it
    /// under someone who was typing.
    address_label_row: Option<adw::EntryRow>,
    /// The line that field hides behind until asked for.
    address_label_shown: Option<adw::ActionRow>,
    /// The header's menu, held only so choosing something can close it.
    main_menu: Option<gtk::MenuButton>,
    /// The send form, which owns its own form/review/sent states.
    send: Controller<SendForm>,
    /// How connections are being made, when they go through Tor.
    tor: Option<String>,
    /// The height this wallet scans from. What makes a sync long or short, and
    /// the difference between "slow" and "working".
    birthday: Option<u32>,
    /// How many blocks the last completed scan had to read, when there has
    /// been one. The only total available for the final phase of a sync.
    matched_blocks: Option<u32>,
    /// Names for transactions and addresses. Held for display; the app owns
    /// the file and is what writes to it.
    labels: crate::wallet::labels::Labels,
    /// This wallet holds no keys, so nothing here may offer to sign.
    watch_only: bool,
    /// Whether signing on this wallet needs a BIP-39 passphrase as well as the
    /// password. Held here as well as on the send form, because the fee-bump
    /// dialog is a second signing path and does not go through that form.
    has_passphrase: bool,
    /// Whether the password prompt is on screen. The locked notice and the
    /// dialog say the same thing, and saying it twice at once makes the page
    /// behind the dialog look like a page that failed.
    asking_to_unlock: bool,
    /// Which network this wallet is on, held separately from the summary.
    ///
    /// A summary arrives only when a scan produces one, which on a long sync is
    /// at the very end — and restarting a session clears it. Anything the sync
    /// view needs *while* syncing cannot be read from there. The header
    /// progress bar was, so it fell back to a spinner for the whole phase.
    network: Option<String>,
    /// Why nothing is connecting, when Tor is on and could not be started.
    tor_problem: Option<String>,
    /// The addresses currently in the list, so it is only rebuilt when they
    /// change.
    peer_addresses: Vec<String>,
    /// Machines, as opposed to connections. kyoto opens more than one
    /// connection to some peers, so the two numbers differ and saying only the
    /// first invites the reader to count the list and find it wrong.
    distinct_peers: usize,
    /// Whether this wallet's keys live on a device, which decides whether an
    /// address can be checked against one. Not *which* device: that is asked
    /// of the devices when it matters.
    device_backed: bool,
    /// The receive address is withheld until somebody says they will use it
    /// without a device confirming it. See `SetReceiveGated`.
    receive_gated: bool,
    /// Amounts replaced with dots, for reading the app where somebody can see
    /// the screen. Session-only: it is about the room, not the wallet.
    values_hidden: bool,
    /// A freshly revealed address, shown instead of the next unused one until
    /// the path or wallet changes. Giving the same unused address to two payers
    /// links them, so asking for a new one has to actually produce one.
    ///
    /// Carries its index, because verifying an address on a device asks for it
    /// by index — and the freshly revealed one is not always the summary's
    /// next unused one.
    fresh_address: Option<(String, u32)>,
}

#[derive(Debug)]
pub enum WalletPageMsg {
    Show(Summary),
    SetProgress(Progress),
    Peers {
        connected: usize,
        required: usize,
    },
    /// Something a person could actually act on. Routine peer churn is not this.
    Note(String),
    Failed(String),
    CopyAddress,
    /// Which unit amounts are shown in. Owned by the app, since the
    /// preferences dialog is where it is changed.
    SetDenomination(crate::settings::Denomination),
    /// `None` clears it — the setting was turned off, or the fetch failed.
    SetPrice(Option<crate::price::Price>),
    Toast(String),
    SetChain(Option<crate::wallet::node::ChainInfo>),
    /// The block at the tip, read whole — its shape and what it says about
    /// who made it.
    SetTipBlock(Option<Box<crate::wallet::node::TipBlock>>),
    /// Ask for the block at the tip, so the chain rows have something to say.
    ReadTipBlock,
    /// Start over from the birthday, after the app has asked.
    AskRescan,
    /// Look on the derivation paths this wallet is not watching.
    AskSearchPaths,
    /// How connections are being made: `Some` when they go through Tor.
    SetTor(Option<String>),
    /// The connected peers, arriving faster than the chain view can.
    SetPeers(Vec<crate::wallet::node::PeerInfo>),
    /// The height this wallet scans from.
    SetBirthday(u32),
    /// Which network it is on, known from metadata before any scan has
    /// produced a summary.
    SetNetwork(String),
    /// The password prompt has appeared, or gone away without unlocking.
    SetAskingToUnlock(bool),
    /// How many blocks the last scan of this wallet had to read.
    SetMatchedBlocks(Option<u32>),
    /// The wallet's labels, freshly read from disk.
    SetLabels(Box<crate::wallet::labels::Labels>),
    /// Name the address currently on the receive screen.
    NameAddress(String),
    /// Leave the address-label field without saving what is in it.
    CancelAddressLabel,
    /// What this program is, and whose work it stands on.
    ShowAbout,
    /// Every address this wallet has handed out.
    ShowAddresses,
    /// Hold a coin back, or release it.
    SetFrozen {
        outpoint: String,
        frozen: bool,
    },
    /// Name one coin.
    SetCoinLabel {
        outpoint: String,
        text: String,
    },
    /// Save a reviewed payment for a signer elsewhere.
    SaveUnsigned(Box<crate::wallet::send::Plan>),
    /// Sign it on the device this wallet came from, then broadcast.
    SignOnDevice(Box<crate::wallet::send::Plan>),
    /// Whether this wallet's keys live on a device, which is what decides
    /// whether signing and verifying on one are offered.
    SetDeviceBacked(bool),
    /// Ask the device to show the receive address on its own screen.
    VerifyAddress,
    /// Whether to withhold the receive address on this wallet.
    SetReceiveGated(bool),
    /// Ask, in words somebody has to type, before showing it.
    AskShowAddress,
    /// Show or hide every amount on screen.
    ToggleValues,
    /// Name a payment just made, from what its request called itself.
    NameTransaction {
        txid: String,
        text: String,
    },
    /// Narrow the activity list to what matches.
    Search(String),
    /// Tell one more peer about a payment that has not been seen.
    Rebroadcast(String),
    /// Ask for a new fee rate for an unconfirmed payment. `cancel` decides
    /// whether the replacement pays the same people or pays nobody; both go
    /// through one path so the two can never drift apart.
    AskBump {
        txid: String,
        cancel: bool,
    },
    /// Build the replacement at this rate.
    Bump {
        txid: String,
        fee_rate: f64,
        cancel: bool,
    },
    /// The replacement, built and waiting for a password.
    BumpPlanned(Box<Result<crate::wallet::send::Plan, String>>),
    /// A replacement was broadcast. The old payment is gone from the wallet's
    /// view, so the page showing it has to go too.
    Replaced {
        with: String,
    },
    /// This wallet holds no keys: the device that does is what signs.
    SetWatchOnly(bool),
    SetHasPassphrase(bool),
    /// Tor is on and could not be started, so nothing is connecting.
    TorProblem(Option<String>),
    RetryTor,
    /// From the send form, on its way to the app.
    PlanSend(Box<Draft>),
    SendNow {
        plan: Box<Plan>,
        password: Password,
        passphrase: Password,
    },
    /// The send form is on screen and wants a fee rate to start from.
    EstimateFee,
    /// From the app, on its way back to the send form.
    FeeSuggestion(f64, String),
    Planned(Box<Result<Plan, String>>),
    Sent(Box<Result<String, String>>),
    /// Limit the activity list to one derivation path, or lift the limit.
    FilterActivity(u32),
    SelectPath(crate::wallet::accounts::ScriptType),
    /// Ask for an address that has not been handed to anyone yet.
    NewAddress,
    /// Open the detail sheet for one transaction.
    ShowTransaction(String),
    /// The freshly revealed address came back.
    ShowFreshAddress(String, u32),
    /// Clear everything that belonged to a different wallet.
    Reset,
    SetLocked(bool),
    ShowPreferences,
    SetName(String),
    RequestUnlock,
}

impl WalletPage {
    /// What stands in for a number that is being kept off the screen.
    ///
    /// Dots rather than a blank: a row that loses its amount entirely reads as
    /// a row with nothing in it, and the shape of the list should not change
    /// when the values do.
    const HIDDEN: &'static str = "••••••";

    fn balance(&self) -> String {
        if self.values_hidden {
            return Self::HIDDEN.into();
        }
        match &self.summary {
            Some(s) => self
                .settings
                .denomination
                .format(s.balance_sats, &s.network),
            None => "—".into(),
        }
    }

    /// The address for the selected path, falling back to the wallet's primary
    /// one when there is no breakdown to choose from.
    fn address(&self) -> String {
        if let Some((fresh, _)) = &self.fresh_address {
            return fresh.clone();
        }
        let Some(summary) = &self.summary else {
            return "—".into();
        };
        self.receive_path
            .or_else(|| self.default_receive_path())
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
        // The chain decides the mark's colour, and an unsynced wallet has no
        // summary to ask — `brand` answers with bitcoin's own orange for
        // anything it does not recognise, which is the right guess for a
        // Bitcoin mark.
        let network = self
            .summary
            .as_ref()
            .map(|s| s.network.as_str())
            .unwrap_or_default();
        super::qr::texture(&super::qr::payment_uri(&address), network)
    }

    /// Whether this path is one the wallet actually watches.
    fn has_path(&self, path: crate::wallet::accounts::ScriptType) -> bool {
        self.summary
            .as_ref()
            .is_some_and(|s| s.accounts.iter().any(|a| a.script_type == path))
    }

    /// Standard paths this wallet is not watching.
    ///
    /// The four BIP-44/49/84/86 accounts are the whole universe here: a seed
    /// used in another wallet was used on one of them.
    fn unwatched_paths(&self) -> Vec<crate::wallet::accounts::ScriptType> {
        crate::wallet::accounts::ScriptType::ALL
            .into_iter()
            .filter(|path| !self.has_path(*path))
            .collect()
    }

    /// Whether it is worth offering to look on the other paths.
    ///
    /// Not for a watch-only wallet: its descriptors came from a device, and
    /// only that device can produce another. Offering a button that can only
    /// fail is worse than not offering it.
    fn can_search_paths(&self) -> bool {
        !self.locked && !self.watch_only && !self.unwatched_paths().is_empty()
    }

    /// What the search row says it would do.
    fn unwatched_note(&self) -> String {
        let names: Vec<&str> = self.unwatched_paths().iter().map(|p| p.label()).collect();
        format!(
            "This wallet watches only the paths it hands addresses out on. If its recovery \
             phrase has been used in another wallet, money may be sitting on {}.",
            match names.len() {
                0 => "another path".to_string(),
                1 => names[0].to_string(),
                _ => format!(
                    "{} or {}",
                    names[..names.len() - 1].join(", "),
                    names[names.len() - 1]
                ),
            }
        )
    }

    /// Whether the receive screen will offer this path.
    ///
    /// Watched is not the same as offered: a wallet imported from a seed
    /// watches BIP44 so money already there is found, and still never hands out
    /// a new `1…`. Every row on the receive picker asks this rather than
    /// `has_path`, so the two cannot come apart.
    fn offers_path(&self, path: crate::wallet::accounts::ScriptType) -> bool {
        path.can_receive() && self.has_path(path)
    }

    /// The path a fresh address should come from when nothing is chosen.
    ///
    /// **Native segwit, on every wallet that watches it.** Not the wallet's
    /// primary, and not taproot: `bc1q` is the address form nothing refuses,
    /// where `bc1p` is still turned away by a handful of exchanges and older
    /// services. The person who pays for that is the one being paid — they find
    /// out when a sender tells them the address was rejected, and have no way
    /// to know that the row one below would have worked.
    ///
    /// The cost is real and worth naming: a taproot key-path spend hides in the
    /// company of every other taproot spend, multisig and lightning included,
    /// where a `bc1q` spend says plainly that it was single-signature. Taproot
    /// stays one tap away and stays what the wallet is built on.
    ///
    /// Falls back to the primary, then to anything that can be handed out at
    /// all, so a wallet not watching native segwit still has an address.
    fn default_receive_path(&self) -> Option<crate::wallet::accounts::ScriptType> {
        let summary = self.summary.as_ref()?;
        summary
            .accounts
            .iter()
            .find(|a| {
                a.script_type == crate::wallet::accounts::ScriptType::NativeSegwit
                    && a.script_type.can_receive()
            })
            .or_else(|| {
                summary
                    .accounts
                    .iter()
                    .find(|a| a.next_address == summary.next_address && a.script_type.can_receive())
            })
            .or_else(|| {
                summary
                    .accounts
                    .iter()
                    .find(|a| a.script_type.can_receive())
            })
            .map(|a| a.script_type)
    }

    fn path_selected(&self, path: crate::wallet::accounts::ScriptType) -> bool {
        match self.receive_path {
            Some(selected) => selected == path,
            None => self.default_receive_path() == Some(path),
        }
    }

    /// The balance in dollars, when a price is on hand.
    ///
    /// Approximate by construction — one exchange's last trade — so it is
    /// marked as such rather than presented as a second exact figure beside
    /// an exact one.
    fn fiat(&self) -> Option<String> {
        if self.values_hidden {
            return None;
        }
        let price = self.price?;
        let summary = self.summary.as_ref()?;
        Some(format!(
            "≈ ${}",
            crate::price::usd(price.value_of(summary.balance_sats))
        ))
    }

    /// The line under the balance: what qualifies the number above it.
    ///
    /// Pending is the important half — a filter wallet cannot see the mempool,
    /// so anything pending here is already mined but shallow.
    fn balance_caption(&self) -> String {
        let Some(summary) = &self.summary else {
            return "Not yet synced".into();
        };

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
        // A wallet that has verified nothing is not "964,758 blocks behind" —
        // that is the height of the chain, not a distance it has fallen. It
        // has not started, and saying so is both true and less alarming.
        if summary.tip == 0 {
            return match chain.tip_height {
                0 => "Nothing verified yet".into(),
                tip => format!("Nothing verified yet — the chain is at {}", thousands(tip)),
            };
        }

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
        let Some(chain) = &self.chain else {
            return "—".into();
        };
        if chain.hashrate <= 0.0 {
            return "—".into();
        }
        for (limit, unit) in [
            (1e21, "ZH/s"),
            (1e18, "EH/s"),
            (1e15, "PH/s"),
            (1e12, "TH/s"),
        ] {
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
        let Some(chain) = &self.chain else {
            return "—".into();
        };
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

    /// The sync status, with the size of the job it is working through.
    ///
    /// A percentage alone does not say whether this is a minute or an hour. A
    /// wallet imported with an old birthday has a quarter of a million blocks
    /// of filters to check, and knowing that is the difference between a slow
    /// wallet and a broken one.
    fn progress_label(&self) -> String {
        let label = self.progress.label();
        if !self.syncing() {
            return label;
        }
        // The filters are done by then; "blocks of history to check" would be
        // counting work that has already happened.
        if matches!(self.progress, Progress::Blocks(_)) {
            return label;
        }

        let (Some(birthday), Some(chain)) = (self.birthday, self.chain.as_ref()) else {
            return label;
        };
        // From the birthday, or from wherever this wallet has already reached.
        // The wallet's own height does not move during a scan — BDK's
        // checkpoint advances when updates are applied, not while filters are
        // coming in — so this is the size of the job, not what is left of it.
        let from = self
            .summary
            .as_ref()
            .map(|s| s.tip.max(birthday))
            .unwrap_or(birthday);
        let total = chain.tip_height.saturating_sub(from);
        if total == 0 {
            return label;
        }

        // What remains, taken from the progress fraction, so the number moves
        // with the percentage beside it. A static total under a climbing
        // percentage reads as a stuck counter.
        let share = crate::wallet::node::FILTER_HEADER_SHARE;
        match self.progress.fraction() {
            // Only during the filter phase is "blocks left" a real number:
            // before it, no filter has been fetched, so all of them are left
            // however far the header count has come.
            Some(fraction) if fraction > share => {
                let done = (fraction - share) / (1.0 - share);
                let left = (total as f64 * (1.0 - done)).round() as u32;
                format!("{label} · about {} blocks left", thousands(left))
            }
            _ => format!("{label} · {} blocks of history to check", thousands(total)),
        }
    }

    /// How far along the sync is, including the header stage.
    ///
    /// kyoto's own fraction covers filters only — it has no idea how many
    /// block headers are still to come, because nothing tells it where the
    /// chain ends until it gets there. But blocks arrive every ten minutes, so
    /// a known block plus the clock estimates the tip closely enough to fill a
    /// bar, which beats a spinner that says nothing for a quarter of an hour.
    fn progress_fraction(&self) -> Option<f64> {
        // The last phase of a sync — fetching the blocks the filters matched —
        // has no total anywhere in the node: a filter match is only known when
        // the block arrives. But the same wallet over the same chain matches
        // the same blocks, so the count from the last scan is a measurement
        // rather than a guess, and the only honest bar available here.
        if let Progress::Blocks(read) = self.progress {
            let expected = self.matched_blocks? as f64;
            if expected <= 0.0 {
                return None;
            }
            return Some((read as f64 / expected).clamp(0.0, 0.99));
        }

        let Progress::Headers(height) = self.progress else {
            return self.progress.fraction();
        };

        let network: bdk_wallet::bitcoin::Network = self
            .network
            .clone()
            .or_else(|| self.summary.as_ref().map(|s| s.network.clone()))?
            .parse()
            .ok()?;
        let tip = crate::wallet::estimated_tip(network)?;
        header_fraction(height, self.birthday.unwrap_or(0), tip)
    }

    /// What the peers list says about itself.
    ///
    /// The two stages have different rules, and not saying so makes the peer
    /// list look broken: block headers come from any peer — filter support is
    /// irrelevant to them — and only once filters start does kyoto require it
    /// and drop everyone else. That is the eviction people watch and wonder
    /// about.
    fn peers_note(&self) -> String {
        if matches!(self.progress, Progress::Headers(_)) {
            return "Block headers come from any peer, so this stage does not ask for \
                    compact filters. When filters start, peers that cannot serve them \
                    are dropped."
                .into();
        }
        "Every peer here serves compact filters — the ones that cannot are dropped as \
         soon as filters are needed."
            .into()
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

    /// What the tip block held, or nothing when none has been read.
    fn tip_block_shape(&self) -> String {
        let Some(block) = &self.tip_block else {
            return String::new();
        };
        let network = self
            .summary
            .as_ref()
            .map(|s| s.network.clone())
            .unwrap_or_default();
        tip_block_shape(block, self.settings.denomination, &network)
    }

    /// Subsidy plus fees: what the block at the tip actually paid.
    fn tip_block_reward(&self) -> String {
        let Some(block) = &self.tip_block else {
            return String::new();
        };
        let network = self
            .summary
            .as_ref()
            .map(|s| s.network.clone())
            .unwrap_or_default();
        self.settings
            .denomination
            .format(block.reward_sat(), &network)
    }

    /// Who the tip block signs itself as, escaped.
    ///
    /// **Escaped, because this is bytes a stranger chose.** A coinbase is
    /// arbitrary data written by whoever made the block, an Adwaita row reads
    /// Pango markup, and a miner is perfectly free to put a `<span>` in there.
    /// `node::coinbase_text` already strips it to printable ASCII and caps the
    /// length; this closes the markup half.
    fn tip_block_signature(&self) -> Option<String> {
        let miner = self.tip_block.as_ref()?.miner.as_deref()?;
        Some(gtk::glib::markup_escape_text(miner).to_string())
    }

    /// The clock consensus uses, which is not the tip's own timestamp.
    fn median_time(&self) -> String {
        let Some(chain) = &self.chain else {
            return "Not yet known".into();
        };
        let Some(median) = chain.median_time_past else {
            return "Not yet known".into();
        };
        let when = gtk::glib::DateTime::from_unix_local(median as i64)
            .and_then(|d| d.format("%H:%M, %e %b"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "Not yet known".into());
        match chain.tip_time {
            // The gap is the interesting part: timelocks are measured against
            // the median, so it being behind the tip is normal and worth
            // seeing.
            Some(tip) if tip > median => {
                format!("{when} — {} behind the tip", short_duration(tip - median))
            }
            _ => when,
        }
    }

    fn subsidy(&self) -> String {
        let Some(chain) = &self.chain else {
            return "Not yet known".into();
        };
        let Some(summary) = &self.summary else {
            return "Not yet known".into();
        };
        let sats = crate::wallet::node::subsidy_sats(chain.tip_height);
        format!(
            "{} for each block mined",
            self.settings.denomination.format(sats, &summary.network)
        )
    }

    fn next_halving(&self) -> String {
        let Some(chain) = &self.chain else {
            return "Not yet known".into();
        };
        let at = crate::wallet::node::next_halving(chain.tip_height);
        let remaining = at.saturating_sub(chain.tip_height);

        // Estimated from the pace this wallet has actually measured, not from
        // the ten-minute target, so the guess moves with the network.
        let interval = chain.mean_interval.unwrap_or(600.0);
        let seconds = remaining as f64 * interval;
        let when = gtk::glib::DateTime::now_local()
            .and_then(|now| now.add_seconds(seconds))
            .and_then(|then| then.format("%b %Y"))
            .map(|s| s.trim().to_string())
            .ok();

        match when {
            Some(when) => format!(
                "Block {} — {} blocks away, around {when}",
                thousands(at),
                thousands(remaining)
            ),
            None => format!(
                "Block {} — {} blocks away",
                thousands(at),
                thousands(remaining)
            ),
        }
    }

    fn issued(&self) -> String {
        let Some(chain) = &self.chain else {
            return "Not yet known".into();
        };
        let Some(summary) = &self.summary else {
            return "Not yet known".into();
        };
        let sats = crate::wallet::node::issued_sats(chain.tip_height);
        let share = sats as f64 / (21_000_000.0 * 100_000_000.0) * 100.0;
        format!(
            "{} by the schedule — {share:.1}% of the 21 million",
            self.settings.denomination.format(sats, &summary.network)
        )
    }

    fn min_relay_fee(&self) -> String {
        match self.chain.as_ref().and_then(|c| c.min_relay_fee) {
            Some(rate) => format!("{rate:.2} sat/vB"),
            None => "—".into(),
        }
    }

    fn peer_count(&self) -> String {
        let Some(chain) = &self.chain else {
            return "Connecting…".into();
        };
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
        self.summary
            .as_ref()
            .is_some_and(|s| !s.transactions.is_empty())
    }

    /// Whether there is more than one path a fresh address could come from.
    ///
    /// Counted over what the picker will *offer*, not over what is watched: a
    /// wallet watching legacy and taproot has one address to hand out, and a
    /// picker with a single row on it is a control that asks a question with
    /// one answer.
    fn has_path_choice(&self) -> bool {
        self.summary.as_ref().is_some_and(|s| {
            s.accounts
                .iter()
                .filter(|a| a.script_type.can_receive())
                .count()
                > 1
        })
    }

    /// What the selected path's addresses look like, so the choice is
    /// recognisable without knowing BIP numbers.
    fn address_hint(&self) -> String {
        let Some(summary) = &self.summary else {
            return String::new();
        };
        let selected = self
            .receive_path
            .and_then(|path| summary.accounts.iter().find(|a| a.script_type == path));
        let Some(account) = selected.or_else(|| {
            summary
                .accounts
                .iter()
                .find(|a| a.next_address == summary.next_address)
        }) else {
            return String::new();
        };
        let network = summary
            .network
            .parse()
            .unwrap_or(bdk_wallet::bitcoin::Network::Signet);
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
        // One peer, on purpose, while block headers come in: kyoto asks for a
        // single connection until it is past that phase, then opens up to its
        // target for the filters. Watching the count sit at one for twenty
        // minutes with nothing saying why is maddening, and it is the most
        // normal thing the sync does.
        if matches!(self.progress, Progress::Headers(_)) {
            return match connections {
                0 => "Connecting…".into(),
                1 => "1 peer — all that headers need. More join for the filters".into(),
                n => format!("{n} peers — headers need one. More join for the filters"),
            };
        }

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
            Some(s) if s.pending_sats > 0 => self
                .settings
                .denomination
                .format(s.pending_sats, &s.network),
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
                    set_subtitle: model.summary.as_ref()
                        .map_or("", |s| crate::wallet::network_label_of(&s.network)),
                },

                // Preferences belong in a dialog reached from the header, not
                // in the switcher beside the things you actually do with money.
                // The hamburger opens a menu, because that is what the icon
                // promises: it used to go straight to preferences, which made
                // About unreachable and the icon a lie.
                #[name(main_menu)]
                pack_end = &gtk::MenuButton {
                    set_icon_name: "open-menu-symbolic",
                    set_tooltip_text: Some("Main menu"),

                    #[wrap(Some)]
                    set_popover = &gtk::Popover {
                        set_width_request: 200,

                        gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 2,

                            gtk::Button {
                                add_css_class: "flat",
                                #[wrap(Some)]
                                set_child = &gtk::Label {
                                    set_label: "Preferences",
                                    set_xalign: 0.0,
                                },
                                connect_clicked => WalletPageMsg::ShowPreferences,
                            },

                            gtk::Button {
                                add_css_class: "flat",
                                #[wrap(Some)]
                                set_child = &gtk::Label {
                                    set_label: "About Sieve",
                                    set_xalign: 0.0,
                                },
                                connect_clicked => WalletPageMsg::ShowAbout,
                            },
                        },
                    },
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
                set_title: &model.progress_label(),
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
                            // A list begins at the top; a single status page
                            // sits in the middle of what it is filling. Which
                            // of those this is depends on whether the wallet
                            // is open, so the alignment does too — pinned to
                            // the top, the locked screen looked like content
                            // that had failed to load.
                            #[watch]
                            set_valign: if model.locked && !model.asking_to_unlock {
                                gtk::Align::Center
                            } else {
                                gtk::Align::Start
                            },

                            adw::StatusPage {
                                set_icon_name: Some("changes-prevent-symbolic"),
                                set_title: "Wallet locked",
                                set_description: Some(
                                    "Unlock to see balances and addresses."
                                ),
                                #[watch]
                                set_visible: model.locked && !model.asking_to_unlock,

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
                            gtk::Overlay {
                                add_css_class: "card",
                                add_css_class: "balance-card",
                                set_margin_top: 12,
                                // Clips to the card's rounded rectangle, which
                                // is what cuts the mark off at the corner
                                // instead of letting it hang outside.
                                set_overflow: gtk::Overflow::Hidden,
                                #[watch]
                                set_visible: !model.locked,

                                // Behind the numbers, tucked into the corner
                                // and mostly outside it. Decoration, so it is
                                // barely there and takes its colour from the
                                // theme rather than being painted on.
                                add_overlay = &gtk::Label {
                                    // The desktop's accent on every chain. It
                                    // is decoration, and nothing about it now
                                    // depends on the wallet — which is what
                                    // makes it a single widget to delete if it
                                    // ever stops earning its place.
                                    add_css_class: "balance-mark",
                                    set_label: "₿",
                                    set_halign: gtk::Align::Start,
                                    set_valign: gtk::Align::End,
                                    set_can_target: false,
                                },

                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 2,

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
                            },

                            gtk::Box {
                                set_margin_top: 12,
                                set_spacing: 12,
                                #[watch]
                                set_visible: model.has_transactions() && !model.locked,

                                gtk::Label {
                                    add_css_class: "heading",
                                    set_halign: gtk::Align::Start,
                                    set_valign: gtk::Align::Center,
                                    set_hexpand: true,
                                    set_label: "Transactions",
                                },

                                // Not a secret, and not a setting: a way to
                                // read the app on a train. Session-only,
                                // because it is about the room rather than
                                // about the wallet.
                                gtk::ToggleButton {
                                    #[watch]
                                    set_icon_name: if model.values_hidden {
                                        "view-conceal-symbolic"
                                    } else {
                                        "view-reveal-symbolic"
                                    },
                                    set_valign: gtk::Align::Center,
                                    add_css_class: "flat",
                                    #[watch]
                                    set_tooltip_text: Some(if model.values_hidden {
                                        "Show amounts"
                                    } else {
                                        "Hide amounts"
                                    }),
                                    #[watch]
                                    #[block_signal(values_toggled)]
                                    set_active: model.values_hidden,
                                    connect_toggled[sender] => move |_| {
                                        sender.input(WalletPageMsg::ToggleValues);
                                    } @values_toggled,
                                },

                                #[name(search_button)]
                                gtk::ToggleButton {
                                    set_icon_name: "system-search-symbolic",
                                    set_valign: gtk::Align::Center,
                                    add_css_class: "flat",
                                    set_tooltip_text: Some("Search this list"),
                                },

                                // Only worth offering when the wallet watches
                                // more than one path.
                                gtk::DropDown {
                                    set_model: Some(&activity_model),
                                    set_valign: gtk::Align::Center,
                                    add_css_class: "flat",
                                    set_tooltip_text: Some("Show one derivation path"),
                                    #[watch]
                                    set_visible: model.has_path_choice(),
                                    connect_selected_notify[sender] => move |list| {
                                        sender.input(WalletPageMsg::FilterActivity(list.selected()));
                                    },
                                },
                            },

                            // Folded away behind the button above until it is
                            // wanted, which is the ordinary shape of a search
                            // in this desktop — and it takes the full width
                            // when it does appear, because an address is long
                            // and a field it does not fit in is one you cannot
                            // check what you pasted into.
                            #[name(search_bar)]
                            gtk::SearchBar {
                                set_margin_top: 6,
                                // GTK dresses a search bar for docking under a
                                // header bar: a filled box with a line along
                                // the bottom. In the middle of a page that
                                // reads as a stray rectangle around the field.
                                add_css_class: "bare-search",

                                #[wrap(Some)]
                                #[name(search_entry)]
                                set_child = &gtk::SearchEntry {
                                    set_hexpand: true,
                                    set_placeholder_text: Some(
                                        "Amount, address, transaction id or label"
                                    ),
                                    connect_search_changed[sender] => move |entry| {
                                        sender.input(
                                            WalletPageMsg::Search(entry.text().to_string())
                                        );
                                    },
                                },
                            },

                            #[local_ref]
                            tx_list -> gtk::ListBox {
                                add_css_class: "boxed-list",
                                set_selection_mode: gtk::SelectionMode::None,
                                set_margin_top: 12,
                                #[watch]
                                set_visible: model.has_transactions()
                                    && !model.locked
                                    && !model.transactions.is_empty(),
                            },

                            // The filter can empty the list without the wallet
                            // being empty, and that needs saying, or the
                            // controls look broken. Given the same treatment as
                            // a wallet with no transactions at all, because it
                            // is the same moment: a list with nothing in it and
                            // a question about why.
                            adw::StatusPage {
                                #[watch]
                                set_icon_name: Some(model.nothing_icon()),
                                set_title: "No results",
                                // Two ways to empty the list, and naming the
                                // wrong one sends somebody to the wrong
                                // control.
                                #[watch]
                                set_description: Some(&model.nothing_shown()),
                                #[watch]
                                set_visible: model.filtered_to_nothing() && !model.locked,
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
                            // Square, and made square by the layout rather
                            // than by a pair of matching numbers. The card
                            // used to take its width from the texture's own
                            // 512px natural size while its height stayed at
                            // the requested minimum, so it stretched sideways
                            // into whatever width was going. An AspectFrame
                            // allocates its child a square, whatever it is
                            // handed.
                            gtk::AspectFrame {
                                #[watch]
                                set_visible: !model.receive_gated,
                                set_ratio: 1.0,
                                set_obey_child: false,
                                set_halign: gtk::Align::Center,
                                set_valign: gtk::Align::Center,
                                // 240 rather than 280: still comfortably
                                // scannable across a table, and it buys the
                                // row of vertical space the address list needs
                                // without the screen having to scroll.
                                set_size_request: (240, 240),

                                gtk::Box {
                                    // Its own white ground, not the theme's
                                    // card: a card is dark in dark mode, which
                                    // is exactly when a QR code needs light.
                                    add_css_class: "qr-ground",
                                    set_overflow: gtk::Overflow::Hidden,

                                    gtk::Picture {
                                        set_hexpand: true,
                                        set_vexpand: true,
                                        // Clipped to the rounded ground, or
                                        // the code's white square fills the
                                        // corners the radius is trying to cut
                                        // away.
                                        set_overflow: gtk::Overflow::Hidden,
                                        set_content_fit: gtk::ContentFit::Contain,
                                        #[watch]
                                        set_paintable: model.qr().as_ref(),
                                    },
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
                                    set_visible: model.offers_path(ScriptType::Legacy),
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
                                    set_visible: model.offers_path(ScriptType::NestedSegwit),
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
                                    set_visible: model.offers_path(ScriptType::NativeSegwit),
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
                                    set_visible: model.offers_path(ScriptType::Taproot),
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
                                set_visible: !model.receive_gated,
                                #[watch]
                                set_label: &model.address_hint(),
                            },

                            // In place of the address, on a wallet whose keys
                            // are elsewhere and which has no device to ask.
                            //
                            // **A wall rather than a warning, because a
                            // warning beside an address is read after the
                            // address has been copied.** Watch-only is mostly
                            // a way to look at history; handing out an address
                            // Sieve cannot vouch for is the one thing on this
                            // screen that can lose money, and it should take a
                            // deliberate act rather than a glance.
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 12,
                                set_margin_top: 24,
                                set_margin_bottom: 24,
                                #[watch]
                                set_visible: model.receive_gated,

                                gtk::Image {
                                    set_icon_name: Some("channel-secure-symbolic"),
                                    set_pixel_size: 48,
                                    add_css_class: "dim-label",
                                },

                                gtk::Label {
                                    add_css_class: "title-4",
                                    set_wrap: true,
                                    set_justify: gtk::Justification::Center,
                                    set_label: "Take the address from the wallet that holds the keys",
                                },

                                gtk::Label {
                                    add_css_class: "dim-label",
                                    set_wrap: true,
                                    set_justify: gtk::Justification::Center,
                                    set_max_width_chars: 46,
                                    set_label: "Sieve is watching this wallet, not holding it. \
                                                It can show you an address derived from the \
                                                descriptor you imported, but nothing here can \
                                                confirm that address is really yours — only the \
                                                wallet with the keys can do that.",
                                },

                                gtk::Button {
                                    add_css_class: "pill",
                                    set_halign: gtk::Align::Center,
                                    set_label: "Show the address anyway",
                                    connect_clicked => WalletPageMsg::AskShowAddress,
                                },
                            },

                            gtk::Label {
                                add_css_class: "monospace",
                                set_wrap: true,
                                set_wrap_mode: gtk::pango::WrapMode::WordChar,
                                set_selectable: true,
                                #[watch]
                                set_visible: !model.receive_gated,
                                // A selectable label selects its *whole* text
                                // the moment it takes focus, so the address sat
                                // there looking as though somebody had dragged
                                // across it. On a screen whose job is to show
                                // one address clearly, a selection nobody made
                                // reads as a state and invites wondering what
                                // it means. Dragging across it still works —
                                // this only undoes the automatic one.
                                connect_has_focus_notify => move |label| {
                                    if label.has_focus() {
                                        label.select_region(0, 0);
                                    }
                                },
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
                                #[watch]
                                set_visible: !model.receive_gated,

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

                                // The one check this screen cannot make for
                                // itself. Sieve computes this address and
                                // draws it; software that has got onto the
                                // machine can do both differently. A device
                                // derives its own copy and shows it on a
                                // screen this computer cannot paint.
                                gtk::Button {
                                    add_css_class: "pill",
                                    set_label: "Verify on device",
                                    set_tooltip_text: Some(
                                        "Show this address on the device and compare the two"
                                    ),
                                    #[watch]
                                    set_visible: model.device_backed,
                                    connect_clicked => WalletPageMsg::VerifyAddress,
                                },
                            },

                            // Only on a wallet whose keys are elsewhere.
                            //
                            // **Sieve did not derive this address from a key it
                            // holds — it derived it from what was imported.**
                            // On a hardware wallet the device settles it, which
                            // is why the button above exists. On a pasted
                            // descriptor nothing can: a descriptor that is
                            // subtly wrong parses, derives perfectly good
                            // addresses, and none of them belong to anybody
                            // here. That is not hypothetical — a change chain
                            // built from only the first cosigner did exactly
                            // that.
                            //
                            // So the check is named, and it is one somebody can
                            // actually perform: compare against the wallet that
                            // exported this, and let the scan find the history
                            // you expect before you are paid at a new address.
                            gtk::Label {
                                add_css_class: "warning",
                                set_wrap: true,
                                set_justify: gtk::Justification::Center,
                                set_margin_top: 12,
                                set_max_width_chars: 44,
                                #[watch]
                                set_visible: model.watch_only && !model.receive_gated,
                                #[watch]
                                set_label: if model.device_backed {
                                    "This address came from what you imported, not from a \
                                     key held here. Show it on the device before giving it \
                                     to anybody."
                                } else {
                                    "This address came from the descriptor you imported, \
                                     and nothing here can confirm it. Check it against the \
                                     wallet that exported the descriptor before giving it \
                                     to anybody."
                                },
                            },

                            // Naming the address before handing it out is what
                            // makes the payment recognisable when it lands —
                            // and it is the moment you actually know who it is
                            // for. Offered, never demanded: an open field here
                            // reads as a question that must be answered before
                            // the address can be used.
                            // Gated with the address, not beside it. Naming
                            // an address is something you do to one you are
                            // about to hand out, and the list is every address
                            // this wallet has — which is a way round the gate
                            // rather than an exception to it.
                            adw::PreferencesGroup {
                                set_margin_top: 12,
                                #[watch]
                                set_visible: !model.receive_gated,

                                #[name(address_label_shown)]
                                adw::ActionRow {
                                    set_title: "Label",
                                    set_subtitle: UNNAMED_ADDRESS,
                                    set_subtitle_lines: 2,
                                    set_activatable: true,

                                    #[name(address_label_edit)]
                                    add_suffix = &gtk::Button {
                                        set_icon_name: "document-edit-symbolic",
                                        set_tooltip_text: Some("Name this address"),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                    },
                                },

                                #[name(address_label_row)]
                                adw::EntryRow {
                                    set_title: "Label (optional)",
                                    set_show_apply_button: true,
                                    set_visible: false,

                                    connect_apply[sender] => move |row| {
                                        sender.input(WalletPageMsg::NameAddress(
                                            row.text().to_string(),
                                        ));
                                    },
                                },

                                // Everything already handed out. Without this
                                // the only address you can ever see is the
                                // next one, which makes naming them pointless
                                // and reuse invisible.
                                adw::ActionRow {
                                    set_title: "Addresses",
                                    #[watch]
                                    set_subtitle: &model.addresses_note(),
                                    set_activatable: true,
                                    add_suffix = &gtk::Image {
                                        set_icon_name: Some("go-next-symbolic"),
                                        add_css_class: "dim-label",
                                    },
                                    connect_activated => WalletPageMsg::ShowAddresses,
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
                            set_label: "Rescan",
                            add_css_class: "flat",
                            set_valign: gtk::Align::Center,
                            set_tooltip_text: Some(
                                "Forget what has been scanned and check the chain again \
                                 from this wallet's birthday"
                            ),
                            // Nothing to rescan while the wallet is shut, and
                            // no node to restart while one is not running.
                            #[watch]
                            set_sensitive: !model.locked,
                            connect_clicked => WalletPageMsg::AskRescan,
                        },

                        // Recovery, not configuration. Framed as a question
                        // about where money might be rather than a set of
                        // tickboxes, which is what stops somebody switching on
                        // legacy "just in case" and then living with a row that
                        // finds nothing for ever.
                        adw::ActionRow {
                            set_title: "Search other derivation paths",
                            set_subtitle_lines: 4,
                            #[watch]
                            set_subtitle: &model.unwatched_note(),
                            #[watch]
                            set_visible: model.can_search_paths(),
                            set_activatable: true,
                            add_suffix = &gtk::Image {
                                set_icon_name: Some("go-next-symbolic"),
                                add_css_class: "dim-label",
                            },
                            connect_activated => WalletPageMsg::AskSearchPaths,
                        },

                        adw::ActionRow {
                            set_title: "Status",
                            set_subtitle_lines: 2,
                            #[watch]
                            set_subtitle: &model.progress_label(),

                            add_suffix = &gtk::Spinner {
                                set_valign: gtk::Align::Center,
                                // Only when there is genuinely nothing to
                                // measure. A spinner is what you show when you
                                // cannot say how far along you are, and for
                                // most of a sync we can.
                                #[watch]
                                set_visible: model.syncing()
                                    && model.progress_fraction().is_none(),
                                #[watch]
                                set_spinning: model.syncing()
                                    && model.progress_fraction().is_none(),
                            },

                            add_suffix = &gtk::ProgressBar {
                                set_valign: gtk::Align::Center,
                                set_width_request: 120,
                                #[watch]
                                set_visible: model.syncing()
                                    && model.progress_fraction().is_some(),
                                #[watch]
                                set_fraction: model.progress_fraction().unwrap_or(0.0),
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

                        adw::ActionRow {
                            set_title: "Median time",
                            set_subtitle_lines: 2,
                            #[watch]
                            set_subtitle: &model.median_time(),
                        },

                        // All three come out of the block the fee estimate
                        // already downloads. See `node::TipBlock`.
                        adw::ActionRow {
                            #[watch]
                            set_visible: model.tip_block.is_some(),
                            set_title: "Last block held",
                            #[watch]
                            set_subtitle: &model.tip_block_shape(),
                            set_subtitle_lines: 2,
                        },

                        adw::ActionRow {
                            #[watch]
                            set_visible: model.tip_block_signature().is_some(),
                            set_title: "Signed by",
                            // "Signed", not "mined by": the coinbase is
                            // whatever the miner chose to put there, nothing
                            // verifies it, and one pool may sign as another.
                            set_subtitle: "The coinbase says so; nothing proves it",

                            add_suffix = &gtk::Label {
                                #[watch]
                                set_label: model.tip_block_signature()
                                    .as_deref()
                                    .unwrap_or_default(),
                                add_css_class: "heading",
                            },
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

                    // Everything here is worked out from the height this
                    // wallet has verified for itself. No server was asked.
                    adw::PreferencesGroup {
                        set_title: "Issuance",

                        adw::ActionRow {
                            set_title: "Block subsidy",
                            #[watch]
                            set_subtitle: &model.subsidy(),
                        },

                        // The subsidy is the schedule; this is what the last
                        // block actually paid, fees included. The gap between
                        // the two is the fee market, in the one place it can
                        // be stated as a fact rather than an estimate.
                        adw::ActionRow {
                            #[watch]
                            set_visible: model.tip_block.is_some(),
                            set_title: "Last block paid",
                            #[watch]
                            set_subtitle: &model.tip_block_reward(),
                        },

                        adw::ActionRow {
                            set_title: "Next halving",
                            #[watch]
                            set_subtitle: &model.next_halving(),
                            set_subtitle_lines: 2,
                        },

                        adw::ActionRow {
                            set_title: "Coins issued",
                            #[watch]
                            set_subtitle: &model.issued(),
                            set_subtitle_lines: 2,
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
                        set_description: Some(&model.peers_note()),
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
        let transactions =
            FactoryVecDeque::builder()
                .launch_default()
                .forward(sender.input_sender(), |out| match out {
                    TxRowOutput::Selected(txid) => WalletPageMsg::ShowTransaction(txid),
                });
        let send = SendForm::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                SendOutput::Plan(draft) => WalletPageMsg::PlanSend(draft),
                SendOutput::Send {
                    plan,
                    password,
                    passphrase,
                } => WalletPageMsg::SendNow {
                    plan,
                    password,
                    passphrase,
                },
                SendOutput::Toast(message) => WalletPageMsg::Toast(message),
                SendOutput::NameTransaction { txid, text } => {
                    WalletPageMsg::NameTransaction { txid, text }
                }
                SendOutput::SetFrozen { outpoint, frozen } => {
                    WalletPageMsg::SetFrozen { outpoint, frozen }
                }
                SendOutput::SetCoinLabel { outpoint, text } => {
                    WalletPageMsg::SetCoinLabel { outpoint, text }
                }
                SendOutput::SaveUnsigned(plan) => WalletPageMsg::SaveUnsigned(plan),
                SendOutput::SignOnDevice(plan) => WalletPageMsg::SignOnDevice(plan),
            });

        let mut model = WalletPage {
            has_passphrase: false,
            settings: Settings::load(),
            stack: None,
            address_label_row: None,
            address_label_shown: None,
            main_menu: None,
            send,
            tor: None,
            birthday: None,
            network: None,
            watch_only: false,
            asking_to_unlock: false,
            matched_blocks: None,
            labels: crate::wallet::labels::Labels::default(),
            tor_problem: None,
            peer_addresses: Vec::new(),
            distinct_peers: 0,
            locked: true,
            price: None,
            toaster: Toaster::default(),
            chain: None,
            tip_block: None,
            peers_list,
            name: "Sieve".into(),
            transactions,
            path_labels: Vec::new(),
            activity_path: None,
            search: String::new(),
            activity_choices: Vec::new(),
            activity_model: gtk::StringList::new(&[]),
            receive_index: 0,
            receive_path: None,
            fresh_address: None,
            device_backed: false,
            receive_gated: false,
            values_hidden: false,
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
        let activity_model = model.activity_model.clone();
        let widgets = view_output!();

        // Both switchers are declared above the stack they drive, so the links
        // are made once the whole tree exists.
        model.stack = Some(widgets.view_stack.clone());
        model.address_label_row = Some(widgets.address_label_row.clone());
        model.address_label_shown = Some(widgets.address_label_shown.clone());
        model.main_menu = Some(widgets.main_menu.clone());

        // The button and the bar are one control. Bound both ways so that
        // Escape, which the search bar handles itself, also lifts the button —
        // a pressed-looking button over a hidden field is a control that has
        // stopped describing itself.
        widgets
            .search_button
            .bind_property("active", &widgets.search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();
        // Tells the bar which entry is its own, which is what makes Escape and
        // the clear button do anything.
        widgets.search_bar.connect_entry(&widgets.search_entry);
        {
            // Closing the search puts the list back. Leaving a query in place
            // while its field is out of sight is a wallet that appears to have
            // lost most of its history.
            let sender = sender.clone();
            let entry = widgets.search_entry.clone();
            widgets
                .search_bar
                .connect_search_mode_enabled_notify(move |bar| {
                    if bar.is_search_mode() {
                        entry.grab_focus();
                    } else if !entry.text().is_empty() {
                        entry.set_text("");
                        sender.input(WalletPageMsg::Search(String::new()));
                    }
                });
        }

        // The line hands over to the field, and the field hands back once the
        // name is saved — so the group is always one row tall and the QR above
        // it never moves.
        {
            let shown = widgets.address_label_shown.clone();
            let editing = widgets.address_label_row.clone();
            let open = move || {
                shown.set_visible(false);
                editing.set_visible(true);
                editing.grab_focus();
            };
            widgets.address_label_edit.connect_clicked({
                let open = open.clone();
                move |_| open()
            });
            widgets
                .address_label_shown
                .connect_activated(move |_| open());

            crate::ui::cancellable_edit(
                &widgets.address_label_row,
                Some("Stop naming this address"),
                {
                    let sender = sender.clone();
                    // Always something to do: the field is only on screen
                    // because it was opened, so Escape has a row to shut.
                    move || {
                        sender.input(WalletPageMsg::CancelAddressLabel);
                        true
                    }
                },
            );
        }
        widgets.send_slot.append(model.send.widget());

        // Both fee sources cost something — a block download or a disclosure —
        // so neither happens until a screen that needs one is actually open.
        //
        // Network as well as Send, because the Chain and Issuance groups now
        // describe the block at the tip, and that block *is* the fee estimate's
        // download. One request serves both, and `App::fee_estimate` caches it
        // against the tip height, so this costs one block per new block rather
        // than one per visit.
        widgets.view_stack.connect_visible_child_name_notify({
            let sender = sender.clone();
            move |stack| {
                match stack.visible_child_name().as_deref() {
                    Some("send") => sender.input(WalletPageMsg::EstimateFee),
                    // The chain rows want the block itself, whichever fee
                    // source is configured — they describe the chain, not
                    // fees, and tying them to that setting would make them
                    // vanish for anyone using mempool.space.
                    Some("network") => sender.input(WalletPageMsg::ReadTipBlock),
                    _ => {}
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
            WalletPageMsg::ShowTransaction(txid) => self.show_transaction(&txid, root, &sender),
            WalletPageMsg::SelectPath(path) => {
                self.receive_path = Some(path);
                // The fresh address belonged to the path being left.
                self.fresh_address = None;
                self.refresh_address_label();
            }
            WalletPageMsg::FilterActivity(index) => {
                let chosen = self.activity_choices.get(index as usize).copied().flatten();
                if chosen != self.activity_path {
                    self.activity_path = chosen;
                    if let Some(summary) = self.summary.clone() {
                        self.rebuild_transactions(&summary);
                    }
                }
            }
            WalletPageMsg::NewAddress => {
                if let Some(summary) = &self.summary
                    && let Some(account) = summary.accounts.get(self.receive_index as usize)
                {
                    let _ = sender.output(WalletPageOutput::NewAddress(account.script_type));
                }
            }
            WalletPageMsg::ShowFreshAddress(address, index) => {
                self.fresh_address = Some((address, index));
                // A brand new address has no name yet, and the field must not
                // keep showing the last one's.
                self.refresh_address_label();
            }
            WalletPageMsg::Toast(message) => self.toaster.add_toast(crate::ui::toast(&message)),
            WalletPageMsg::SetChain(chain) => {
                if let Some(info) = &chain {
                    self.distinct_peers = info.peers.len();
                    let addresses: Vec<String> = info
                        .peers
                        .iter()
                        .map(|peer| format!("{}/{:?}", peer.address, peer.serves_filters))
                        .collect();
                    if addresses != self.peer_addresses {
                        self.peer_addresses = addresses;
                        let mut guard = self.peers_list.guard();
                        guard.clear();
                        for peer in &info.peers {
                            guard.push_back(peer.clone());
                        }
                    }
                }
                // What peers will relay is the floor under the fee field.
                self.send.emit(SendMsg::SetMinFee(
                    chain.as_ref().and_then(|c| c.min_relay_fee),
                ));
                // **A new block is the evidence against "no new blocks".**
                //
                // kyoto warns about a stale tip and never withdraws it — there
                // is no "the tip is fine again" message to wait for — so the
                // note it raised stayed on screen for the life of the session.
                // Left running overnight, a wallet sat at the tip still saying
                // its connection might be stale, which is worse than saying
                // nothing: a warning that is wrong is a warning nobody reads
                // the next time.
                //
                // Cleared here rather than on a timer, because the thing that
                // makes it false is a block arriving, and that is exactly what
                // this message carries.
                if let (Some(new), Some(old)) = (chain.as_ref(), self.chain.as_ref())
                    && new.tip_height > old.tip_height
                {
                    self.note = None;
                }
                self.chain = chain;
            }
            WalletPageMsg::SetTor(tor) => self.tor = tor,

            WalletPageMsg::SetTipBlock(block) => self.tip_block = block.map(|b| *b),
            WalletPageMsg::SetBirthday(height) => self.birthday = Some(height),
            WalletPageMsg::SetNetwork(network) => self.network = Some(network),
            WalletPageMsg::SetAskingToUnlock(asking) => self.asking_to_unlock = asking,
            WalletPageMsg::SetMatchedBlocks(blocks) => self.matched_blocks = blocks,
            WalletPageMsg::NameTransaction { txid, text } => {
                let _ = sender.output(WalletPageOutput::SetLabel {
                    kind: crate::wallet::labels::Kind::Tx,
                    reference: txid,
                    text,
                });
            }
            WalletPageMsg::ShowAddresses => self.show_addresses(root, &sender),

            WalletPageMsg::Search(query) => {
                self.search = query;
                if let Some(summary) = self.summary.clone() {
                    self.rebuild_transactions(&summary);
                }
            }

            WalletPageMsg::Rebroadcast(txid) => {
                let Some(from) = self
                    .summary
                    .as_ref()
                    .and_then(|s| s.transactions.iter().find(|t| t.txid == txid))
                    .map(|tx| tx.script_type)
                else {
                    return;
                };
                self.toaster
                    .add_toast(crate::ui::toast("Telling another peer…"));
                let _ = sender.output(WalletPageOutput::Rebroadcast { txid, from });
            }

            WalletPageMsg::AskBump { txid, cancel } => self.ask_bump(&txid, cancel, root, &sender),

            WalletPageMsg::Bump {
                txid,
                fee_rate,
                cancel,
            } => {
                let Some(from) = self
                    .summary
                    .as_ref()
                    .and_then(|s| s.transactions.iter().find(|t| t.txid == txid))
                    .map(|tx| tx.script_type)
                else {
                    return;
                };
                self.toaster.add_toast(crate::ui::toast(if cancel {
                    "Building the cancellation…"
                } else {
                    "Building the replacement…"
                }));
                let _ = sender.output(WalletPageOutput::PlanBump {
                    txid,
                    from,
                    fee_rate,
                    cancel,
                });
            }

            WalletPageMsg::Replaced { with } => {
                // Off the page that was showing the payment that no longer
                // exists, and onto the one that took its place — which is
                // where somebody would go looking to check it worked.
                if let Some(nav) = navigation_view(root) {
                    nav.pop();
                }
                self.show_transaction(&with, root, &sender);

                let short: String = with.chars().take(12).collect();
                // Held no longer than any other. It used to sit for six
                // seconds because it names a transaction id, but the page
                // underneath is already the replacement's own — the id is
                // there to read for as long as anybody wants it, and the
                // toast is only saying which page it just opened.
                self.toaster.add_toast(crate::ui::toast(&format!(
                    "Fee raised — this is now {short}…"
                )));
                // Deliberately without the transaction ids. At info level
                // these reach the journal, where they identify the user's own
                // payments to anybody who can read it — and the journal is
                // readable by more people than the wallet file is.
                tracing::info!("replaced a payment with a higher fee");
            }

            WalletPageMsg::BumpPlanned(result) => match *result {
                Ok(plan) => self.confirm_bump(plan, root, &sender),
                Err(message) => {
                    self.toaster
                        .add_toast(crate::ui::toast(&crate::ui::send::capitalise(&message)));
                }
            },

            WalletPageMsg::ShowAbout => {
                self.close_menu();
                crate::about::present(root);
            }
            WalletPageMsg::NameAddress(text) => {
                let _ = sender.output(WalletPageOutput::SetLabel {
                    kind: crate::wallet::labels::Kind::Addr,
                    reference: self.address(),
                    text,
                });
            }
            // Reads the saved name back out, so leaving the field also undoes
            // whatever was typed into it — which is what cancelling means.
            WalletPageMsg::CancelAddressLabel => self.refresh_address_label(),

            WalletPageMsg::SetFrozen { outpoint, frozen } => {
                let _ = sender.output(WalletPageOutput::SetFrozen { outpoint, frozen });
            }

            WalletPageMsg::SaveUnsigned(plan) => {
                let _ = sender.output(WalletPageOutput::SaveUnsigned(plan));
            }

            WalletPageMsg::SignOnDevice(plan) => {
                let _ = sender.output(WalletPageOutput::SignOnDevice(plan));
            }

            // Straight onto the road every other label takes: the app owns the
            // file, so the app does the writing.
            WalletPageMsg::SetCoinLabel { outpoint, text } => {
                let _ = sender.output(WalletPageOutput::SetLabel {
                    kind: crate::wallet::labels::Kind::Output,
                    reference: outpoint,
                    text,
                });
            }

            WalletPageMsg::SetLabels(labels) => {
                self.send.emit(SendMsg::SetLabels(labels.clone()));
                self.labels = *labels;
                self.refresh_address_label();
                // The list carries labels in its rows, so it is rebuilt.
                if let Some(summary) = self.summary.clone() {
                    self.rebuild_transactions(&summary);
                }
            }

            WalletPageMsg::SetDeviceBacked(backed) => {
                self.device_backed = backed;
                self.send.emit(SendMsg::SetDeviceBacked(backed));
            }

            WalletPageMsg::SetReceiveGated(gated) => self.receive_gated = gated,

            WalletPageMsg::ToggleValues => {
                self.values_hidden = !self.values_hidden;
                // The rows carry formatted text, so they are rebuilt the same
                // way a change of denomination rebuilds them.
                if let Some(summary) = self.summary.clone() {
                    self.rebuild_transactions(&summary);
                }
            }

            WalletPageMsg::AskShowAddress => {
                // Typed, not clicked. A button says "I would like to get on
                // with it"; a sentence somebody has to write says they read
                // the one above it. This is the only place in Sieve that asks
                // for that, and it is here because the mistake it guards
                // against — being paid at an address nobody here can vouch
                // for — is not visible when it happens and not recoverable
                // afterwards.
                const PHRASE: &str = "I understand";

                let dialog =
                    adw::AlertDialog::new(Some("Show an address nothing here can confirm?"), None);
                dialog.set_body(
                    "This wallet's keys are somewhere else, and Sieve derived this address \
                     from the descriptor you imported. If that descriptor is wrong in any \
                     way, the address belongs to nobody and a payment to it is gone.\n\n\
                     The wallet that holds the keys can show you the address itself. That \
                     is the only check there is.\n\n\
                     Type “I understand” to show it here anyway.",
                );

                let group = adw::PreferencesGroup::new();
                let entry = adw::EntryRow::new();
                entry.set_title("Type: I understand");
                group.add(&entry);
                dialog.set_extra_child(Some(&group));

                dialog.add_response("cancel", "Cancel");
                dialog.add_response("show", "Show it anyway");
                dialog.set_response_appearance("show", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                // Enabled only once the words are actually there.
                dialog.set_response_enabled("show", false);
                {
                    let dialog = dialog.clone();
                    entry.connect_changed(move |row| {
                        let typed = row.text().trim().eq_ignore_ascii_case(PHRASE);
                        dialog.set_response_enabled("show", typed);
                    });
                }
                {
                    let sender = sender.clone();
                    let entry = entry.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "show" {
                            return;
                        }
                        if !entry.text().trim().eq_ignore_ascii_case(PHRASE) {
                            return;
                        }
                        let _ = sender.output(WalletPageOutput::AcknowledgeReceive);
                    });
                }
                dialog.present(Some(root));
            }

            WalletPageMsg::VerifyAddress => {
                // The index of the address actually on screen, which is the
                // freshly revealed one when there is one and the next unused
                // otherwise. Asking the device for a different index would
                // show a mismatch on a perfectly good address.
                let Some(path) = self.receive_path.or_else(|| self.default_receive_path()) else {
                    return;
                };
                let index = match &self.fresh_address {
                    Some((_, index)) => Some(*index),
                    None => self
                        .summary
                        .as_ref()
                        .and_then(|s| s.accounts.iter().find(|a| a.script_type == path))
                        .map(|a| a.next_index),
                };
                let Some(index) = index else { return };
                let _ = sender.output(WalletPageOutput::VerifyAddress { path, index });
            }

            WalletPageMsg::SetWatchOnly(watch_only) => {
                // Kept here too: the transaction page offers to replace a
                // payment, and a wallet with no keys cannot sign one.
                self.watch_only = watch_only;
                self.send.emit(SendMsg::SetWatchOnly(watch_only));
            }

            WalletPageMsg::SetHasPassphrase(has) => {
                self.has_passphrase = has;
                self.send.emit(SendMsg::SetHasPassphrase(has));
            }

            WalletPageMsg::TorProblem(problem) => self.tor_problem = problem,
            WalletPageMsg::RetryTor => {
                let _ = sender.output(WalletPageOutput::RetryTor);
            }

            WalletPageMsg::SetPeers(peers) => {
                self.distinct_peers = peers.len();
                // Only when the set has actually changed. Tearing down eight
                // rows of widgets and building them again for an identical
                // list is work the main thread cannot afford at the rate
                // connection warnings arrive.
                //
                // What a peer *serves* is part of that set, not just where it
                // is. A peer's flags are unknown when it first connects and
                // arrive later; comparing addresses alone left the row saying
                // "has not said what it serves" about a peer that had since
                // said exactly that.
                let addresses: Vec<String> = peers
                    .iter()
                    .map(|peer| format!("{}/{:?}", peer.address, peer.serves_filters))
                    .collect();
                if addresses == self.peer_addresses {
                    return;
                }
                self.peer_addresses = addresses;

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
            WalletPageMsg::AskSearchPaths => {
                self.close_menu();
                let _ = sender.output(WalletPageOutput::AskSearchPaths);
            }

            WalletPageMsg::AskRescan => {
                let _ = sender.output(WalletPageOutput::AskRescan);
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
                self.close_menu();
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
                self.birthday = None;
                self.network = None;
                self.matched_blocks = None;
                self.labels = crate::wallet::labels::Labels::default();
                self.distinct_peers = 0;
                self.peer_addresses.clear();
                self.note = None;
                self.error = None;
                self.receive_index = 0;
                self.receive_path = None;
                self.fresh_address = None;
                self.path_labels.clear();
                self.activity_path = None;
                self.activity_choices.clear();
                self.activity_model
                    .splice(0, self.activity_model.n_items(), &[]);
                self.transactions.guard().clear();
                // The chain belongs to a network, not just a wallet: leaving
                // it up meant a signet wallet briefly showing mainnet's
                // difficulty and peers.
                self.chain = None;
                self.peers_list.guard().clear();
                self.price = None;
            }
            WalletPageMsg::Show(summary) => {
                // Rebuild rather than diff: four rows, and the set only changes
                // when a sync lands.
                self.sync_path_picker(&summary);
                self.rebuild_transactions(&summary);
                self.send.emit(SendMsg::Show(Box::new(summary.clone())));
                self.summary = Some(summary);
                self.refresh_address_label();
            }

            WalletPageMsg::PlanSend(draft) => {
                let _ = sender.output(WalletPageOutput::PlanSend(draft));
            }
            WalletPageMsg::SendNow {
                plan,
                password,
                passphrase,
            } => {
                let _ = sender.output(WalletPageOutput::Send {
                    plan,
                    password,
                    passphrase,
                });
            }
            WalletPageMsg::ReadTipBlock => {
                let _ = sender.output(WalletPageOutput::ReadTipBlock);
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
                        .add_toast(crate::ui::toast(&format!("Payment sent — {short}…")));
                }
                self.send.emit(SendMsg::Sent(result));
            }
            WalletPageMsg::SetProgress(progress) => {
                self.progress = progress;
                self.error = None;
            }
            WalletPageMsg::Peers {
                connected,
                required,
            } => self.peers = Some((connected, required)),
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
        let hidden = self.values_hidden;
        self.sync_activity_filter(summary);

        // Worth naming on each row only when there is more than one path for a
        // transaction to be on, and not when the list is already down to one.
        let show_path = summary.accounts.len() > 1 && self.activity_path.is_none();

        let mut guard = self.transactions.guard();
        guard.clear();
        for tx in &summary.transactions {
            if self
                .activity_path
                .is_some_and(|only| only != tx.script_type)
            {
                continue;
            }
            let label = self
                .labels
                .get(crate::wallet::labels::Kind::Tx, &tx.txid)
                .map(str::to_owned);
            if !matches_search(
                tx,
                label.as_deref(),
                &self.search,
                self.settings.denomination,
                &summary.network,
            ) {
                continue;
            }
            guard.push_back((
                tx.clone(),
                self.settings.denomination,
                summary.tip,
                self.price,
                summary.network.clone(),
                show_path,
                label,
                hidden,
            ));
        }
    }

    /// Whether the filter is hiding everything the wallet has.
    fn filtered_to_nothing(&self) -> bool {
        self.has_transactions() && self.transactions.is_empty()
    }

    /// Why the list is empty when the wallet is not. The search is named
    /// first: it is the thing just typed, and so the thing to undo.
    fn nothing_shown(&self) -> String {
        match (!self.search.trim().is_empty(), self.activity_path) {
            // The two filters compose, so a search runs inside whichever path
            // is being shown. Said plainly when it is what emptied the list:
            // otherwise the answer to "is this payment in my wallet" is no,
            // and it is wrong.
            (true, Some(_)) if self.matches_another_path() => format!(
                "Nothing on this derivation path matches “{}” — but something on another \
                 one does. Choose All paths to see it.",
                self.search.trim()
            ),
            (true, _) => format!(
                "Nothing here matches “{}”. Try part of an address, an amount, or a name \
                 you gave a payment.",
                self.search.trim()
            ),
            (false, Some(_)) => {
                "This wallet has transactions, but none on the derivation path being shown."
                    .to_string()
            }
            (false, None) => "Nothing to show.".to_string(),
        }
    }

    /// Whether what was searched for is in this wallet, only on a path the
    /// filter is hiding.
    fn matches_another_path(&self) -> bool {
        let Some(summary) = &self.summary else {
            return false;
        };
        summary.transactions.iter().any(|tx| {
            self.activity_path
                .is_some_and(|only| only != tx.script_type)
                && matches_search(
                    tx,
                    self.labels.get(crate::wallet::labels::Kind::Tx, &tx.txid),
                    &self.search,
                    self.settings.denomination,
                    &summary.network,
                )
        })
    }

    /// Which of the two emptied it, said in a picture as well as in words.
    fn nothing_icon(&self) -> &'static str {
        match self.search.trim().is_empty() {
            false => "system-search-symbolic",
            true => "document-open-recent-symbolic",
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
        let Some(nav) = navigation_view(root) else {
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
            self.settings
                .denomination
                .format(magnitude, &summary.network)
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
                "≈ ${}",
                crate::price::usd(price.value_of(magnitude))
            )));
            value.add_css_class("dim-label");
            value.add_css_class("numeric");
            stack.append(&value);
        }
        headline.add(&stack);
        page.add(&headline);

        // What this payment was for, in your words. A field standing open at
        // the top read as something waiting to be filled in; this reads as a
        // fact about the payment, which is what it is once written.
        page.add(&self.label_group(&tx.txid, sender));

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
        if tx.height.is_none() {
            // Only worth saying while it can still happen.
            let replaceable = detail_row(
                "Replaceable",
                if tx.replaceable {
                    "Yes — signals BIP-125, so it can be replaced by a higher fee"
                } else {
                    "No — it cannot be replaced, only waited for"
                },
            );

            status.add(&replaceable);

            // Already raised once. Worth saying plainly, because without it
            // the only way to find out is to open the dialog and be told the
            // floor is higher than you expected.
            if !tx.replaces.is_empty() {
                let history = detail_row(
                    "Already raised",
                    "This payment is itself a replacement — its fee has been raised once \
                     already. Only one of them can ever confirm.",
                );
                history.set_subtitle_lines(3);
                if let Some(earlier) = tx.replaces.first() {
                    history.set_tooltip_text(Some(&format!("It replaced {earlier}")));
                }
                status.add(&history);
            }

            // Its own row rather than a button crushed into the suffix of a
            // two-line one: the phrase fits by construction, and it reads as
            // the action it is instead of an afterthought. A payment somebody
            // else made is not ours to replace — we cannot sign its inputs —
            // and neither is one on a wallet that holds no keys.
            if !incoming && !self.watch_only {
                // Above the fee rows on purpose. A payment nobody has seen is
                // usually not one that needs to pay more: kyoto announces to
                // exactly one peer, and a peer that will not relay it is
                // silent about that. Telling another costs nothing, where
                // raising the fee costs money and rebuilds the same
                // transaction into the same wall.
                let again = adw::ActionRow::new();
                again.set_title("Broadcast again");
                again.set_subtitle(
                    "Tell another peer. A payment is announced to one peer at a time, and a \
                     peer that will not pass it on does not say so",
                );
                again.set_subtitle_lines(3);
                again.set_activatable(true);
                again.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                {
                    let sender = sender.clone();
                    let txid = tx.txid.clone();
                    again.connect_activated(move |_| {
                        sender.input(WalletPageMsg::Rebroadcast(txid.clone()));
                    });
                }
                status.add(&again);
            }

            if tx.replaceable && !incoming && !self.watch_only {
                let raise = adw::ActionRow::new();
                raise.set_title("Raise the fee");
                raise.set_subtitle("Replace it with a payment that pays more");
                raise.set_activatable(true);
                raise.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                {
                    let sender = sender.clone();
                    let txid = tx.txid.clone();
                    raise.connect_activated(move |_| {
                        sender.input(WalletPageMsg::AskBump {
                            txid: txid.clone(),
                            cancel: false,
                        });
                    });
                }
                status.add(&raise);

                // Bitcoin has no undo. What it has is a second payment that
                // spends the same coins and pays nobody, which the network may
                // prefer — so the row says "try", and the dialog behind it
                // says why that word is the honest one.
                let call_off = adw::ActionRow::new();
                call_off.set_title("Try to cancel it");
                call_off.set_subtitle("Replace it with one that pays nobody");
                call_off.set_activatable(true);
                call_off.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                {
                    let sender = sender.clone();
                    let txid = tx.txid.clone();
                    call_off.connect_activated(move |_| {
                        sender.input(WalletPageMsg::AskBump {
                            txid: txid.clone(),
                            cancel: true,
                        });
                    });
                }
                status.add(&call_off);
            }
        }
        // What it published, if it published anything. Both ways, as the
        // review dialog showed them: the text is what somebody meant and the
        // hex is what is actually on the chain, and this is the only screen
        // where either can be read again.
        // Numbered only when there is more than one, so the ordinary case
        // does not read as an excerpt from a longer list.
        for (position, (text, hex)) in tx.data.iter().enumerate() {
            // Named for the mechanism rather than for the intent. Every
            // other row on this page is the technical fact — block hash,
            // virtual size, derivation path — and somebody reading a
            // transaction here wants to know what carried the bytes.
            let title = match tx.data.len() {
                1 => "OP_RETURN".to_string(),
                _ => format!("OP_RETURN {}", position + 1),
            };
            // Escaped by `mono`, which matters more here than anywhere else in
            // the app: this is the one field carrying bytes somebody else
            // chose into markup.
            let message = mono_row(&title, &format!("{text}\n{hex}"));
            message.set_subtitle_lines(4);
            message.add_css_class("full-contrast");
            status.add(&message);
        }
        if let Some(hash) = &tx.block_hash {
            status.add(&mono_row("Block hash", hash));
        }
        page.add(&status);

        // Where the money actually went. The headline nets everything into one
        // number; this is the part that says who was paid, which is the thing
        // worth checking against what you meant to do.
        //
        // On a payment you made that is the headline question, so it goes
        // first. On one you received it is somebody else's business — the
        // other outputs of a transaction that happened to pay you — so it
        // follows what you actually received.
        let others_paid = (!tx.paid_to.is_empty()).then(|| {
            let sent_to = adw::PreferencesGroup::new();
            sent_to.set_title(if incoming { "Also paid" } else { "Paid to" });
            for (address, sats) in &tx.paid_to {
                sent_to.add(&address_row(
                    address,
                    &self.settings.denomination.format(*sats, &summary.network),
                    self.labels.get(crate::wallet::labels::Kind::Addr, address),
                ));
            }
            sent_to
        });
        if !incoming && let Some(group) = &others_paid {
            page.add(group);
        }

        if !tx.paid_to_self.is_empty() {
            let mine = adw::PreferencesGroup::new();
            mine.set_title(if incoming {
                "Received at"
            } else {
                "Change back to you"
            });
            if !incoming {
                mine.set_description(Some(
                    "A payment rarely matches a coin exactly, so the remainder returns \
                     to a fresh address of yours.",
                ));
            }
            for out in &tx.paid_to_self {
                mine.add(&own_address_row(
                    out,
                    &self
                        .settings
                        .denomination
                        .format(out.sats, &summary.network),
                    self.labels
                        .get(crate::wallet::labels::Kind::Addr, &out.address),
                ));
            }
            page.add(&mine);
        }

        // Money you received comes first; the rest of the transaction's
        // outputs are somebody else's business and follow it.
        if incoming && let Some(group) = &others_paid {
            page.add(group);
        }

        // A privacy observation, not a warning: nothing is broken, but reuse
        // is what ties one payment to another for anybody reading the chain.
        if tx.reused_address {
            let note = adw::PreferencesGroup::new();
            let row = adw::ActionRow::new();
            row.set_title("An address here has been used more than once");
            row.set_subtitle(
                "Reused addresses let anyone watching the chain tie these payments \
                 together. Sieve hands out a fresh address each time you ask.",
            );
            row.set_subtitle_lines(3);
            // Not a padlock: this is a privacy observation, and a padlock
            // says the opposite of what it means.
            row.add_prefix(&gtk::Image::from_icon_name("view-reveal-symbolic"));
            note.add(&row);
            page.add(&note);
        }

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
        if let Some(rate) = tx.fee_rate() {
            // What the fee worked out at, which is the number to compare
            // against what was chosen when sending.
            detail.add(&detail_row("Fee rate", &format!("{rate:.2} sat/vB")));
        }
        detail.add(&detail_row(
            "Size",
            &format!("{} vB", thousands(tx.vsize as u32)),
        ));
        detail.add(&detail_row(
            "Inputs and outputs",
            &format!(
                "{} in, {} out",
                plural(tx.inputs, "coin", "coins"),
                plural(tx.outputs, "payment", "payments"),
            ),
        ));
        if !incoming && tx.change_sats() > 0 {
            detail.add(&detail_row(
                "Change",
                &self
                    .settings
                    .denomination
                    .format(tx.change_sats(), &summary.network),
            ));
        }
        // The account, then what it means. An imported descriptor can name an
        // account Sieve would not have chosen, so this is read from the
        // descriptor rather than assumed from the script type.
        detail.add(&detail_row(
            "Derivation path",
            &match &tx.account_path {
                Some(path) => format!("{path} · {}", tx.script_type),
                None => tx.script_type.to_string(),
            },
        ));

        let txid_row = adw::ActionRow::new();
        txid_row.set_use_markup(true);
        txid_row.set_title("Transaction ID");
        txid_row.set_subtitle(&mono(&tx.txid));
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
            explorer.set_subtitle(
                "Opens your browser, and tells the explorer you looked at this transaction",
            );
            explorer.set_subtitle_lines(2);
            explorer.set_activatable(true);
            explorer.add_suffix(&gtk::Image::from_icon_name("web-browser-symbolic"));

            let launcher_parent = root.clone();
            let sender = sender.clone();
            // The page already knows, because the network view describes it.
            let tor_on = self.tor.is_some();
            explorer.connect_activated(move |_| {
                // Asked before it happens, not described after. This is the
                // only outbound request in Sieve that Tor cannot cover — it
                // leaves the process entirely for the system browser, carrying
                // this machine's real address and whatever the browser says
                // about it, alongside a transaction that is *yours*. That
                // pairing is the strongest thing anybody could learn here, and
                // it is one click away in a screen people open to read a fee.
                //
                // The subtitle above says the same thing, and a subtitle is
                // read once while the row stays clickable forever.
                let dialog =
                    adw::AlertDialog::new(Some("Open this transaction on mempool.space?"), None);
                dialog.set_body_use_markup(false);
                dialog.set_body(if tor_on {
                    "This opens your web browser, which is outside Sieve — so it does not \
                     go through Tor even though Tor is on. mempool.space will learn your \
                     IP address and that you looked up this particular transaction, which \
                     is one of yours.\n\nYour wallet's own sync does not tell anyone that."
                } else {
                    "This opens your web browser. mempool.space will learn your IP address \
                     and that you looked up this particular transaction, which is one of \
                     yours.\n\nYour wallet's own sync does not tell anyone that."
                });
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("open", "Open browser");
                // Not destructive — nothing is lost — but not suggested
                // either: the safe answer is the one that stays here.
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                {
                    let url = url.clone();
                    let launcher_parent = launcher_parent.clone();
                    let sender = sender.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "open" {
                            return;
                        }
                        let sender = sender.clone();
                        crate::ui::browser::open(&url, &launcher_parent, move |message| {
                            sender.input(WalletPageMsg::Toast(message));
                        });
                    });
                }
                dialog.present(Some(&launcher_parent));
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

    /// How many receive addresses this wallet is watching, and how many have
    /// been paid to.
    ///
    /// **Not "handed out", which is what this used to say and could not know.**
    /// The list is the revealed range of the receive chain, and on an imported
    /// wallet that range comes from the scan: it reaches at least as far as the
    /// highest index a payment was found at. A wallet that gave out twenty
    /// addresses and was paid at two of them therefore shows twenty-odd here,
    /// none of which Sieve watched being given to anybody. Whether one was
    /// handed out is a fact about a conversation somewhere else.
    fn addresses_note(&self) -> String {
        let Some(summary) = &self.summary else {
            return "None yet".into();
        };
        let all = summary.addresses.len();
        if all == 0 {
            return "None yet".into();
        }
        let used = summary.addresses.iter().filter(|a| a.payments > 0).count();
        format!(
            "Watching {}, {used} paid to",
            plural(all, "address", "addresses")
        )
    }

    /// Every receive address this wallet watches, as a page over the wallet.
    ///
    /// Receive addresses only. Change belongs to transactions rather than to
    /// anybody, and listing it here would offer addresses to hand out that
    /// were never meant to be handed out.
    fn show_addresses(&self, root: &adw::BreakpointBin, sender: &ComponentSender<Self>) {
        let Some(summary) = &self.summary else { return };
        let Some(nav) = navigation_view(root) else {
            return;
        };

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        // Not "handed out": on an imported wallet these are the addresses the
        // scan revealed, and which of them somebody was actually given is a
        // fact this program never saw. See `addresses_note`.
        group.set_title("Receive addresses");
        // Which paths are in the list, since a wallet imported from a device
        // watches four and they are interleaved here by age rather than kept
        // apart.
        let mut paths: Vec<&'static str> = summary
            .addresses
            .iter()
            .map(|entry| entry.script_type.label())
            .collect();
        paths.dedup();
        group.set_description(Some(&format!(
            "Every address this wallet has given out, oldest first, across {}. Change \
             addresses are not here: they belong to a payment rather than to anybody.",
            paths.join(", ")
        )));

        // Oldest first within a path, and paths in the order the ecosystem
        // grew — the same order everything else in Sieve lists them.
        for entry in &summary.addresses {
            let row = adw::ActionRow::new();
            row.set_use_markup(true);

            let name = self
                .labels
                .get(crate::wallet::labels::Kind::Addr, &entry.address);
            row.set_title(&match name {
                Some(name) => gtk::glib::markup_escape_text(name).to_string(),
                None => format!("Address {}", entry.index),
            });

            // The address, then the path that produced it, a size down and
            // stepped back: the address is what gets checked, the path is what
            // explains it.
            row.set_subtitle(&match &entry.path {
                Some(path) => format!(
                    "{}\n<span size=\"small\" alpha=\"60%\">{}</span>",
                    mono(&entry.address),
                    mono(path)
                ),
                None => mono(&entry.address),
            });
            row.set_subtitle_lines(4);
            row.add_css_class("property");

            // What happened to it, which is the question the list answers.
            let state = gtk::Box::new(gtk::Orientation::Vertical, 0);
            state.set_valign(gtk::Align::Center);
            state.set_halign(gtk::Align::End);

            let amount = gtk::Label::new(None);
            amount.set_halign(gtk::Align::End);
            amount.add_css_class("numeric");
            match entry.payments {
                0 => {
                    amount.set_label("Unused");
                    amount.add_css_class("dim-label");
                }
                _ => {
                    amount.set_label(
                        &self
                            .settings
                            .denomination
                            .format(entry.received_sats, &summary.network),
                    );
                    amount.add_css_class("success");
                }
            }
            state.append(&amount);

            // Reuse is the fact this screen can state that no other screen
            // can, so it is said in words under the amount rather than left to
            // an icon: a glyph here has to be recognised before it can be
            // understood, and the wrong one reads as reassurance.
            if entry.payments > 1 {
                let reused = gtk::Label::new(Some(&format!("Paid {} times", entry.payments)));
                reused.set_halign(gtk::Align::End);
                reused.add_css_class("caption");
                reused.add_css_class("warning");
                reused.set_tooltip_text(Some(
                    "Anyone watching the chain can tie those payments to each other. \
                     Press New address to hand out a fresh one instead.",
                ));
                state.append(&reused);
            }
            row.add_suffix(&state);

            let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
            copy.set_tooltip_text(Some("Copy address"));
            copy.set_valign(gtk::Align::Center);
            copy.add_css_class("flat");
            {
                let address = entry.address.clone();
                let sender = sender.clone();
                copy.connect_clicked(move |_| {
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&address);
                    }
                    sender.input(WalletPageMsg::Toast("Address copied".into()));
                });
            }
            row.add_suffix(&copy);
            group.add(&row);
        }

        page.add(&group);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        nav.push(&adw::NavigationPage::new(&toolbar, "Addresses"));
    }

    /// Ask what fee rate to replace an unconfirmed payment at.
    ///
    /// The floor is what the network will actually relay: a replacement must
    /// pay the original's fee *and* at least a satoshi per virtual byte for
    /// its own size. Below that it is dropped by every node it reaches, which
    /// looks exactly like the network ignoring you.
    fn ask_bump(
        &self,
        txid: &str,
        cancel: bool,
        root: &adw::BreakpointBin,
        sender: &ComponentSender<Self>,
    ) {
        let Some(summary) = &self.summary else { return };
        let Some(tx) = summary.transactions.iter().find(|t| t.txid == txid) else {
            return;
        };
        let Some(window) = root.root() else { return };

        let was = tx.fee_rate().unwrap_or(1.0);
        let floor = (was + 1.0).max(2.0);

        let body = match cancel {
            false => format!(
                "This payment is paying {was:.2} sat/vB. A replacement spends the same coins, \
                 so only one of them can ever confirm — and if the original confirms first, \
                 the replacement simply never happens and costs nothing.\n\nSieve cannot see \
                 the mempool, so it cannot tell you which one the network prefers. The chain \
                 decides."
            ),
            // Said plainly, because the word "cancel" promises something
            // Bitcoin cannot give. A sent payment is out on the network and
            // anybody can mine it.
            true => format!(
                "This does not undo the payment. It sends the same coins back to your own \
                 wallet at a higher fee, and the two cannot both confirm — so if the network \
                 prefers this one, the original never happens.\n\nIt may not. The original is \
                 paying {was:.2} sat/vB and is already out there; if it is mined first the \
                 money is gone as it was meant to be, and this costs nothing. Sieve cannot \
                 see the mempool, so it cannot tell you which way it will go.\n\nYou pay the \
                 fee either way if this one wins."
            ),
        };

        let dialog = adw::AlertDialog::new(
            Some(match cancel {
                false => "Raise the fee?",
                true => "Try to cancel this payment?",
            }),
            Some(&body),
        );
        dialog.add_response("cancel", "Back");
        dialog.add_response(
            "bump",
            match cancel {
                false => "Replace it",
                true => "Try to cancel it",
            },
        );
        dialog.set_response_appearance("bump", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let group = adw::PreferencesGroup::new();
        let rate = adw::SpinRow::new(
            Some(&gtk::Adjustment::new(floor, floor, 5_000.0, 1.0, 10.0, 0.0)),
            1.0,
            1,
        );
        rate.set_title("New fee rate");
        rate.set_subtitle(&format!(
            "At least {floor:.2} sat/vB — a replacement has to pay for its own size on top of \
             what the original paid"
        ));
        rate.set_subtitle_lines(3);
        group.add(&rate);
        dialog.set_extra_child(Some(&group));

        {
            let sender = sender.clone();
            let txid = txid.to_owned();
            dialog.connect_response(None, move |_, response| {
                if response == "bump" {
                    sender.input(WalletPageMsg::Bump {
                        txid: txid.clone(),
                        fee_rate: rate.value(),
                        cancel,
                    });
                }
            });
        }
        dialog.present(Some(&window));
    }

    /// The last step before a replacement is signed and broadcast.
    ///
    /// Every number restated, as the send confirmation does: what the
    /// recipient still gets, what the fee was, what it becomes, and what the
    /// difference costs.
    fn confirm_bump(
        &self,
        plan: crate::wallet::send::Plan,
        root: &adw::BreakpointBin,
        sender: &ComponentSender<Self>,
    ) {
        let Some(window) = root.root() else { return };
        let Some(summary) = &self.summary else { return };
        let unit = self.settings.denomination;
        let network = &summary.network;

        let was = plan.was_fee.unwrap_or(0);
        let now = plan.fee.to_sat();
        let rate = plan
            .fee_rate()
            .map(|rate| format!(" · about {:.1} sat/vB", rate.to_sat_per_vb_ceil()))
            .unwrap_or_default();
        let mut body = if plan.cancels {
            // The kept amount leads, because it is the number this is for.
            // The fee follows, because it is what the attempt costs whether or
            // not anybody thinks of it as a cost.
            format!(
                "{} stops being paid to {}.\n\nFee was {}\nFee becomes {}{rate}\nComing \
                 back to you {}",
                unit.format(plan.spend().to_sat(), network),
                crate::ui::send::shorten(&plan.to()),
                unit.format(was, network),
                unit.format(now, network),
                unit.format(
                    plan.change.map(|change| change.to_sat()).unwrap_or(0),
                    network
                ),
            )
        } else {
            let mut body = format!(
                "The recipient still gets {}.\n\nFee was {}\nFee becomes {}{rate}\nThat is {} \
                 more.",
                unit.format(plan.spend().to_sat(), network),
                unit.format(was, network),
                unit.format(now, network),
                unit.format(now.saturating_sub(was), network),
            );
            // Where the extra comes from, which is the part somebody would want
            // to check: normally the change, and if there is none, another coin.
            match plan.change {
                Some(change) => body.push_str(&format!(
                    "\n\nComing back to you {}",
                    unit.format(change.to_sat(), network)
                )),
                None => body.push_str(
                    "\n\nNothing comes back to you from this one — the fee is taking what \
                     change there was.",
                ),
            }
            body
        };
        body.push_str(match plan.cancels {
            false => {
                "\n\nThe replacement spends the same coins as the original, so only one of \
                 them can confirm."
            }
            // Repeated here on purpose. This is the last screen before it is
            // signed, and "cancel" is a word that promises more than this can
            // do.
            true => {
                "\n\nBoth spend the same coins, so only one can confirm. If the original is \
                 mined first, it stands and this costs nothing."
            }
        });

        let dialog = adw::AlertDialog::new(
            Some(match plan.cancels {
                false => "Replace this payment?",
                true => "Try to cancel this payment?",
            }),
            Some(&body),
        );
        dialog.add_response("cancel", "Back");
        dialog.add_response(
            "send",
            match plan.cancels {
                false => "Replace",
                true => "Try to cancel it",
            },
        );
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        // The password buys one signature, here as everywhere else.
        let password = gtk::PasswordEntry::new();
        password.set_show_peek_icon(true);
        password.set_placeholder_text(Some("Wallet password"));
        password.set_margin_top(6);

        // A replacement is signed exactly like the payment it replaces, so a
        // wallet with a passphrase needs it here as well. This dialog was
        // missed once already — it is a second signing path that does not go
        // through the send form.
        let passphrase = gtk::PasswordEntry::new();
        passphrase.set_show_peek_icon(true);
        passphrase.set_placeholder_text(Some("BIP-39 passphrase"));
        passphrase.set_margin_top(6);
        passphrase.set_visible(self.has_passphrase);

        let fields = gtk::Box::new(gtk::Orientation::Vertical, 0);
        fields.append(&password);
        fields.append(&passphrase);
        dialog.set_extra_child(Some(&fields));

        {
            let sender = sender.clone();
            let password = password.clone();
            let passphrase = passphrase.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "send" {
                    let _ = sender.output(WalletPageOutput::Send {
                        plan: Box::new(plan.clone()),
                        password: Password(zeroize::Zeroizing::new(password.text().to_string())),
                        passphrase: Password(zeroize::Zeroizing::new(
                            passphrase.text().to_string(),
                        )),
                    });
                }
            });
        }
        dialog.present(Some(&window));
    }

    /// The transaction's name: shown as a line, edited when asked for.
    ///
    /// Two rows in one group, one visible at a time. An entry that is always
    /// open invites typing into it and makes an unlabelled payment look
    /// unfinished; a line that says what the payment is, with a pencil beside
    /// it, says the same thing without the demand.
    fn label_group(&self, txid: &str, sender: &ComponentSender<Self>) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let existing = self
            .labels
            .get(crate::wallet::labels::Kind::Tx, txid)
            .unwrap_or_default()
            .to_owned();

        let shown = adw::ActionRow::new();
        shown.set_title("Label");
        shown.set_subtitle(if existing.is_empty() {
            UNLABELLED_TX
        } else {
            &existing
        });
        shown.set_subtitle_lines(2);
        shown.set_activatable(true);

        let edit = gtk::Button::from_icon_name("document-edit-symbolic");
        edit.set_tooltip_text(Some("Edit this label"));
        edit.set_valign(gtk::Align::Center);
        edit.add_css_class("flat");
        shown.add_suffix(&edit);

        let editing = adw::EntryRow::new();
        editing.set_title("Label (optional)");
        editing.set_text(&existing);
        editing.set_show_apply_button(true);
        editing.set_visible(false);

        // Either row hands over to the other, so the group is always exactly
        // one row tall and nothing below it moves.
        let open = {
            let shown = shown.clone();
            let editing = editing.clone();
            move || {
                shown.set_visible(false);
                editing.set_visible(true);
                editing.grab_focus();
            }
        };
        edit.connect_clicked({
            let open = open.clone();
            move |_| open()
        });
        shown.connect_activated(move |_| open());

        crate::ui::cancellable_edit(&editing, Some("Stop editing this label"), {
            let editing = editing.clone();
            let shown = shown.clone();
            let existing = existing.clone();
            move || {
                // The saved label, not what was typed: closing the row without
                // putting it back would show the abandoned text next time.
                editing.set_text(&existing);
                editing.set_visible(false);
                shown.set_visible(true);
                true
            }
        });

        editing.connect_apply({
            let sender = sender.clone();
            let txid = txid.to_owned();
            let shown = shown.clone();
            move |row| {
                let text = row.text().to_string();
                let _ = sender.output(WalletPageOutput::SetLabel {
                    kind: crate::wallet::labels::Kind::Tx,
                    reference: txid.clone(),
                    text: text.clone(),
                });
                shown.set_subtitle(if text.trim().is_empty() {
                    UNLABELLED_TX
                } else {
                    text.trim()
                });
                row.set_visible(false);
                shown.set_visible(true);
            }
        });

        group.add(&shown);
        group.add(&editing);
        group
    }

    /// Shut the header menu.
    ///
    /// A GTK popover does not close because something inside it was clicked,
    /// so every menu action has to say so. Doing it in one place rather than
    /// in each handler: Preferences was added without it and left the menu
    /// hanging over the dialog, swallowing scroll until it was clicked away.
    fn close_menu(&self) {
        if let Some(popover) = self.main_menu.as_ref().and_then(|button| button.popover()) {
            popover.popdown();
        }
    }

    /// Put the current address's name on screen, and close the editor.
    ///
    /// Called whenever the address changes as well as when a name is saved: a
    /// name belongs to one address, and leaving the last one's on screen would
    /// claim something false about this one.
    fn refresh_address_label(&self) {
        let name = self
            .labels
            .get(crate::wallet::labels::Kind::Addr, &self.address())
            .unwrap_or_default();

        if let Some(row) = &self.address_label_row {
            if row.text() != name {
                row.set_text(name);
            }
            row.set_visible(false);
        }
        if let Some(shown) = &self.address_label_shown {
            shown.set_subtitle(if name.is_empty() {
                UNNAMED_ADDRESS
            } else {
                name
            });
            shown.set_visible(true);
        }
    }

    /// Offer the paths this wallet actually watches, and no others.
    ///
    /// Rewritten only when that set changes, for the same reason the receive
    /// picker is: splicing the model resets the selection, and the selection
    /// is the filter.
    fn sync_activity_filter(&mut self, summary: &Summary) {
        let mut choices: Vec<Option<crate::wallet::accounts::ScriptType>> = vec![None];
        choices.extend(summary.accounts.iter().map(|a| Some(a.script_type)));
        if choices == self.activity_choices {
            return;
        }

        let labels: Vec<String> = std::iter::once("All paths".to_string())
            .chain(summary.accounts.iter().map(|a| a.script_type.to_string()))
            .collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.activity_model
            .splice(0, self.activity_model.n_items(), &refs);
        self.activity_choices = choices;

        // A path the wallet has stopped watching cannot stay selected.
        if self
            .activity_path
            .is_some_and(|only| !self.activity_choices.contains(&Some(only)))
        {
            self.activity_path = None;
        }
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
/// A detail row whose value is an identifier rather than prose.
fn mono_row(title: &str, value: &str) -> adw::ActionRow {
    let row = detail_row(title, "");
    row.set_use_markup(true);
    row.set_subtitle(&mono(value));
    row
}

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

    fn a_transaction() -> crate::wallet::TxSummary {
        crate::wallet::TxSummary {
            txid: "9894a16a8388c63e8005d6d7fd97a47ad0c2b59fbe2c26edf03921de13282c08".into(),
            net_sats: -20_553,
            fee_sats: Some(553),
            height: Some(900_000),
            seen_at: None,
            script_type: crate::wallet::accounts::ScriptType::NativeSegwit,
            vsize: 141,
            inputs: 1,
            outputs: 2,
            paid_to: vec![("bc1qexampleaddresshere".into(), 20_000)],
            paid_to_self: Vec::new(),
            account_path: None,
            replaceable: true,
            data: vec![("sieve was here".into(), "7369657665".into())],
            replaces: Vec::new(),
            reused_address: false,
            block_hash: None,
        }
    }

    #[test]
    fn search_matches_what_a_person_has_in_front_of_them() {
        let tx = a_transaction();
        let unit = crate::settings::Denomination::Sats;
        let find = |query: &str| matches_search(&tx, Some("Boom"), query, unit, "bitcoin");

        // Nothing typed shows everything, or the list empties on first focus.
        assert!(find(""));
        assert!(find("   "));

        // The middle and the end of an id, not only the start: an id is
        // usually copied from something that shows neither end whole.
        assert!(find("9894a16a"));
        assert!(find("13282c08"));
        assert!(find("D6D7FD"), "case must not matter");

        assert!(find("bc1qexample"), "an address");
        assert!(find("Boom"), "the name given to it");
        assert!(find("sieve was here"), "what it published");

        // The amount both ways: as written on screen, and as the plain
        // satoshis a block explorer shows.
        assert!(find("20553"));
        assert!(find(&unit.format(20_553, "bitcoin")));

        assert!(!find("bc1qsomebodyelse"));
        assert!(!find("41"), "a number that is in no field of this one");
    }

    #[test]
    fn header_progress_is_measured_from_the_birthday() {
        // Half the blocks between the birthday and the tip.
        let half = header_fraction(500_000, 0, 1_000_000).unwrap();
        assert!((half - 0.5).abs() < 0.001, "{half}");

        // A wallet born recently is not 99% synced the moment it starts.
        let from_birthday = header_fraction(900_100, 900_000, 900_200).unwrap();
        assert!((from_birthday - 0.5).abs() < 0.001, "{from_birthday}");

        // Nothing to measure yet, or an estimate the chain has overtaken.
        assert!(header_fraction(900_000, 900_000, 1_000_000).is_none());
        assert!(header_fraction(1_000_000, 900_000, 899_000).is_none());

        // Past the estimate, hold short of full rather than claiming done.
        assert_eq!(header_fraction(1_000_000, 0, 900_000), Some(0.99));
    }

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
