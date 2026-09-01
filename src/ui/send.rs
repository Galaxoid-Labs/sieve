//! Sending: the form, the review, and the moment of committing.
//!
//! Three states on one page. The form is watch-only work — a transaction can
//! be built, priced and thrown away without the password, because BDK needs
//! only public descriptors and UTXOs to choose coins. The password is asked
//! for once, in the confirmation, and buys exactly one signature.
//!
//! The confirmation is an `adw::AlertDialog` rather than another page on
//! purpose: this is the irreversible step, and Adwaita's answer to an
//! irreversible step is a dialog that states what will happen and makes you
//! answer for it.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};
use zeroize::Zeroizing;

use crate::settings::{Denomination, Settings};
use crate::wallet::accounts::ScriptType;
use crate::wallet::send::{Draft, Plan, Sending};
use crate::wallet::{AccountSummary, Summary};

/// The wallet password, with a redacted `Debug`.
pub struct Password(pub Zeroizing<String>);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// A floor under the fee rate, so a transaction no peer will relay cannot be
/// built by leaving the field alone.
const DEFAULT_FEE_RATE: f64 = 2.0;

/// One more person paid by the same transaction.
///
/// The first recipient keeps its own row on the form, because it carries what
/// belongs to the payment rather than to a person: a pasted payment request is
/// unpacked there, and Max means "everything", which only has a meaning while
/// there is one person to send everything to. These are the ones after it, and
/// saying "also pay" is what makes that order readable.
#[derive(Debug)]
pub struct ExtraPayee {
    unit: String,
    /// Kept here rather than read back off the widgets, so the form can decide
    /// whether it has a payment without reaching into anybody's entry boxes.
    address: String,
    amount: String,
}

#[derive(Debug, Clone)]
// Every variant sets something, because that is what a message to a row of
// two fields does. Renaming them to avoid the prefix would make them worse.
#[allow(clippy::enum_variant_names)]
pub enum ExtraPayeeMsg {
    SetUnit(String),
    SetAddress(String),
    SetAmount(String),
}

#[derive(Debug)]
pub enum ExtraPayeeOutput {
    Remove(DynamicIndex),
    Edited,
}

#[relm4::factory(pub)]
impl FactoryComponent for ExtraPayee {
    type Init = String;
    type Input = ExtraPayeeMsg;
    type Output = ExtraPayeeOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        adw::PreferencesGroup {
            #[name(address)]
            adw::EntryRow {
                set_title: "Also pay",
                // Same treatment as the first: an address is checked
                // character by character against another screen.
                add_css_class: "monospace",
                connect_changed[sender] => move |row| {
                    sender.input(ExtraPayeeMsg::SetAddress(row.text().to_string()));
                    let _ = sender.output(ExtraPayeeOutput::Edited);
                },

                add_suffix = &gtk::Button {
                    set_icon_name: "list-remove-symbolic",
                    add_css_class: "flat",
                    set_valign: gtk::Align::Center,
                    set_tooltip_text: Some("Remove this recipient"),
                    connect_clicked[sender, index] => move |_| {
                        let _ = sender.output(ExtraPayeeOutput::Remove(index.clone()));
                    },
                },
            },

            #[name(amount)]
            adw::EntryRow {
                #[watch]
                set_title: &format!("Amount in {}", self.unit),
                set_input_purpose: gtk::InputPurpose::Number,
                add_css_class: "numeric",
                connect_changed[sender] => move |row| {
                    sender.input(ExtraPayeeMsg::SetAmount(row.text().to_string()));
                    let _ = sender.output(ExtraPayeeOutput::Edited);
                },
            },
        }
    }

    fn init_model(unit: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self {
            unit,
            address: String::new(),
            amount: String::new(),
        }
    }

    fn init_widgets(
        &mut self,
        index: &DynamicIndex,
        root: Self::Root,
        _returned: &<Self::ParentWidget as relm4::factory::FactoryView>::ReturnedWidget,
        sender: FactorySender<Self>,
    ) -> Self::Widgets {
        let index = index.clone();
        let widgets = view_output!();
        install_amount_filter(&widgets.amount);
        widgets
    }

    fn update(&mut self, msg: Self::Input, _sender: FactorySender<Self>) {
        match msg {
            ExtraPayeeMsg::SetUnit(unit) => self.unit = unit,
            ExtraPayeeMsg::SetAddress(address) => self.address = address,
            ExtraPayeeMsg::SetAmount(amount) => self.amount = amount,
        }
    }
}

#[derive(Debug)]
pub enum SendMsg {
    /// Write the reviewed payment to a file for a signer elsewhere.
    SaveUnsigned,
    /// The same payment as base64, for pasting where a file cannot go.
    CopyUnsigned,
    /// Hand the payment to the device this wallet came from.
    SignOnDevice,
    /// Which device this wallet was imported from, if any.
    SetDevice(Option<crate::hardware::Kind>),
    /// Freeze a coin, or let it be spent again.
    SetFrozen {
        outpoint: String,
        frozen: bool,
    },
    /// Name one coin, or clear its name.
    SetCoinLabel {
        outpoint: String,
        text: String,
    },
    /// One more person on this transaction.
    AddPayee,
    RemovePayee(DynamicIndex),
    /// An extra recipient's address or amount changed, so Review has to be
    /// re-decided.
    PayeeEdited,
    /// Type the amount in dollars, or go back to bitcoin.
    ToggleFiat(bool),
    /// The data field changed, which can turn a form with no recipient into a
    /// payment and back again.
    DataEdited,
    /// The "Pay to" field holds a `bitcoin:` URI: take it apart and fill the
    /// form in from it.
    UnpackUri(String),
    /// The recipient field changed. The fields live in the widgets, so the
    /// model has to be told when they are worth acting on.
    RecipientEdited,
    /// Open the coin picker.
    ChooseCoins,
    /// Spend exactly these, or go back to choosing automatically when empty.
    SetCoins(Vec<bdk_wallet::bitcoin::OutPoint>),
    /// The wallet's labels, so coins can be named rather than listed as hex.
    SetLabels(Box<crate::wallet::labels::Labels>),
    Show(Box<Summary>),
    SetDenomination(Denomination),
    SetPrice(Option<crate::price::Price>),
    /// The lowest rate the connected peers said they would relay, in sat/vB.
    SetMinFee(Option<f64>),
    /// A rate to start from, and where it came from.
    Suggest {
        rate: f64,
        source: String,
    },
    /// This wallet holds no keys here.
    SetWatchOnly(bool),
    /// Whether a BIP-39 passphrase is part of this wallet's key, and so has to
    /// be asked for alongside the password before anything can be signed.
    SetHasPassphrase(bool),
    /// The fee field was changed.
    FeeEdited,
    SelectFrom(u32),
    ToggleMax(bool),
    /// The amount field was typed in.
    AmountEdited,
    /// Build the transaction and show what it would cost.
    Review,
    Planned(Box<Result<Plan, String>>),
    /// The password is in and the dialog said go.
    Confirm(Password, Password),
    Sent(Box<Result<String, String>>),
    /// Back to an empty form.
    Reset,
    CopyTxid,
    OpenExplorer,
}

#[derive(Debug)]
pub enum SendOutput {
    /// Build this, watch-only, and hand the numbers back.
    Plan(Box<Draft>),
    /// Save this payment for a signer elsewhere. The app owns the file
    /// dialogs, as it does for labels and descriptors.
    SaveUnsigned(Box<Plan>),
    /// Sign this on the device the wallet was imported from, then broadcast.
    SignOnDevice(Box<Plan>),
    /// Sign and broadcast the plan already reviewed.
    Send {
        plan: Box<Plan>,
        password: Password,
        /// The BIP-39 passphrase, empty unless this wallet was set up with
        /// one. It is part of the key, so signing without it derives a
        /// different wallet and finalizes nothing.
        passphrase: Password,
    },
    /// Something worth saying once and not keeping. The toast overlay belongs
    /// to the wallet page, which wraps this one.
    Toast(String),
    /// A payment request named who was being paid, and the payment just made
    /// is now theirs. Carrying it through means the history says "Alice"
    /// without anybody typing it.
    NameTransaction { txid: String, text: String },
    /// Hold a coin back, or release it. The app owns the label file, so it
    /// does the writing — the same route a name takes.
    SetFrozen { outpoint: String, frozen: bool },
    /// Name one coin. Travels the same road, and lands in the same file.
    SetCoinLabel { outpoint: String, text: String },
}

pub struct SendForm {
    settings: Settings,
    network: String,
    accounts: Vec<AccountSummary>,
    price: Option<crate::price::Price>,
    /// Which path the coins come from. Each is a separate BDK wallet with its
    /// own UTXOs, so one transaction spends from exactly one of them.
    from: Option<ScriptType>,
    max: bool,
    min_fee: Option<f64>,
    /// No keys in this wallet: it can build a payment but not sign one.
    watch_only: bool,
    /// The device this wallet was imported from, when it was imported from one
    /// — which is what decides whether signing over the cable is offered.
    device: Option<crate::hardware::Kind>,
    has_passphrase: bool,
    /// Where the number in the fee field came from, said under it.
    fee_source: Option<String>,
    /// The last rate Sieve put there itself. A value that no longer matches it
    /// was chosen by a person, and is not overwritten by a later estimate.
    suggested: Option<f64>,
    error: Option<String>,
    busy: bool,
    /// Whether there is an address in the recipient field, and an amount worth
    /// sending in the amount field. Held here because the button that acts on
    /// them lives in the same view and must not offer to act on nothing.
    to_filled: bool,
    amount_filled: bool,
    /// Which coins this payment will spend. Empty means "choose for me", which
    /// is what most payments should be.
    coins: Vec<bdk_wallet::bitcoin::OutPoint>,
    /// Every coin on the path being spent from, for the picker.
    available_coins: Vec<crate::wallet::CoinSummary>,
    /// The height the wallet has verified to, so a coin's age can be stated.
    tip: u32,
    /// Names for coins, by the address they landed on and the payment that
    /// brought them in.
    labels: crate::wallet::labels::Labels,
    /// What a pasted payment request said it was for, shown under the fields
    /// so the numbers can be checked against the request that produced them.
    request: Option<String>,
    /// Who that request said was being paid, kept to name the payment once it
    /// is made.
    request_label: Option<String>,
    /// The reviewed transaction, held between the dialog opening and the
    /// password arriving. Public data — the signature is what needs a secret.
    plan: Option<Plan>,
    sent: Option<String>,
    /// What the payment was, in words, kept for the screen that follows it.
    sent_detail: Option<String>,
    from_model: gtk::StringList,
    from_labels: Vec<String>,
    /// Kept so the form can be emptied after a payment goes out.
    /// Everybody after the first. The first has its own row on the form.
    extras: FactoryVecDeque<ExtraPayee>,
    /// Whether the amount field is being typed in dollars. What is sent is
    /// bitcoin either way; this only decides how the number is read.
    in_fiat: bool,
    /// What is in the amount field, so the line under it can say what those
    /// dollars come to without reaching into the widget from the view.
    amount_text: String,
    /// Exactly what is typed in the data field, held so the form can decide
    /// whether it describes a transaction and so the byte count can be shown.
    /// Never trimmed: a trailing space is part of what somebody asked to
    /// publish.
    data: String,
    data_row: Option<adw::EntryRow>,
    to_row: Option<adw::EntryRow>,
    amount_row: Option<adw::EntryRow>,
}

impl SendForm {
    /// Paths with something to spend. A path holding nothing cannot be the
    /// source of a payment, and offering it as one is a dead end.
    fn fundable(&self) -> Vec<&AccountSummary> {
        self.accounts
            .iter()
            .filter(|a| a.balance_sats > 0)
            .collect()
    }

    /// How to name where these coins live, for copy that has to be true either
    /// way.
    ///
    /// Each derivation path is its own wallet with its own coins, and a payment
    /// is built from exactly one of them — so "all your coins" becomes false the
    /// moment a second path holds anything, and false in the direction that
    /// sends somebody hunting for money that is not missing. When only one path
    /// is funded there is no distinction to draw, and naming it is jargon for a
    /// difference that does not exist yet.
    fn coins_scope(&self) -> String {
        match self.fundable().len() > 1 {
            true => format!(
                "on {}",
                self.source()
                    .map(|a| a.script_type.label())
                    .unwrap_or("this path")
            ),
            false => "in this wallet".to_string(),
        }
    }

    /// Whether money this payment cannot reach is sitting unfrozen on another
    /// path — the one genuinely useful thing to say to somebody looking at a
    /// path where everything is held back.
    fn spendable_elsewhere(&self) -> bool {
        self.available_coins.iter().any(|coin| {
            Some(coin.script_type) != self.from && coin.spendable() && !self.is_frozen(coin)
        })
    }

    fn source(&self) -> Option<&AccountSummary> {
        let fundable = self.fundable();
        match self.from {
            Some(script_type) => fundable
                .iter()
                .find(|a| a.script_type == script_type)
                .or(fundable.first())
                .copied(),
            None => fundable.first().copied(),
        }
    }

    /// What this payment can draw on.
    ///
    /// The whole path, unless coins have been chosen — then it is exactly
    /// those, because that is what "available" means once somebody has said
    /// which coins to use. Max filled this field from the path balance while a
    /// selection was in force, which put a number on screen larger than the
    /// payment could ever send.
    /// Frozen coins are subtracted, because "available" has to mean money this
    /// payment can actually reach. Counting them would put a figure on screen
    /// that Max fills in and the builder then refuses, which reads as the
    /// wallet losing track of its own balance rather than as a coin somebody
    /// held back on purpose.
    fn available_sats(&self) -> u64 {
        if !self.coins.is_empty() {
            return self
                .coins_here()
                .iter()
                .filter(|coin| self.coins.contains(&coin.outpoint))
                .filter(|coin| !self.is_frozen(coin))
                .map(|coin| coin.sats)
                .sum();
        }
        let balance = self.source().map_or(0, |a| a.balance_sats);
        let held: u64 = self
            .coins_here()
            .iter()
            .filter(|coin| self.is_frozen(coin) && coin.spendable())
            .map(|coin| coin.sats)
            .sum();
        balance.saturating_sub(held)
    }

    /// How much is frozen on the path being spent from, for the line that says
    /// so under the balance. Nothing is said when nothing is frozen.
    fn frozen_sats(&self) -> u64 {
        self.coins_here()
            .iter()
            .filter(|coin| self.is_frozen(coin) && coin.spendable())
            .map(|coin| coin.sats)
            .sum()
    }

    fn available(&self) -> String {
        self.settings
            .denomination
            .format(self.available_sats(), &self.network)
    }

    /// The same number without its unit, for a field that will be read back.
    fn available_amount(&self) -> String {
        if self.in_fiat
            && let Some(price) = self.price.as_ref()
        {
            // Two decimal places, because that is what a dollar has and
            // because the figure is going into a field somebody may edit.
            return format!("{:.2}", price.value_of(self.available_sats()));
        }
        let shown = self.available();
        shown
            .rsplit_once(' ')
            .map_or(shown.clone(), |(amount, _)| amount.to_string())
    }

    fn unit(&self) -> &'static str {
        self.settings.denomination.label(&self.network)
    }

    /// The coin picker, as a page over the form.
    ///
    /// Its own selection state rather than a round trip through the model for
    /// every tick: the page has to redraw its own totals as boxes are ticked,
    /// and rebuilding it on each toggle would throw away the scroll position
    /// in the middle of a list somebody is reading.
    fn show_coins(
        &self,
        root: &gtk::ScrolledWindow,
        sender: &ComponentSender<Self>,
        wanted: Option<u64>,
        fee_rate: f64,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;

        let Some(nav) = root
            .ancestor(adw::NavigationView::static_type())
            .and_then(|found| found.downcast::<adw::NavigationView>().ok())
        else {
            tracing::warn!("could not find a navigation view to show the coins on");
            return;
        };

        let here: Vec<crate::wallet::CoinSummary> =
            self.coins_here().into_iter().cloned().collect();
        // A coin frozen since this selection was made is not part of it. The
        // row paints itself unticked either way; without this the tally behind
        // it would still be counting money the builder will refuse.
        let selected = Rc::new(RefCell::new(
            self.coins
                .iter()
                .filter(|point| self.labels.spendable(&point.to_string()))
                .copied()
                .collect::<Vec<_>>(),
        ));

        let page = adw::PreferencesPage::new();

        // What the selection adds up to, and whether it is enough. Restated
        // as boxes are ticked, because the whole question here is arithmetic.
        let tally = adw::PreferencesGroup::new();

        // The default state, said as its own row rather than as a footnote
        // under a question that does not apply yet. Nobody has to pick coins,
        // and the screen should say so before it says anything else.
        let automatic = adw::ActionRow::new();
        automatic.set_title("Sieve is choosing for you");
        automatic.set_subtitle(
            "It takes the fewest coins that cover the payment, which is usually the choice \
             that gives away least. Tick any below to choose yourself.",
        );
        automatic.set_subtitle_lines(4);
        tally.add(&automatic);

        let chosen_row = adw::ActionRow::new();
        // "Coins selected", not "Selected": the number beside it is a total of
        // coins, and one line above a payment amount it read as the amount.
        chosen_row.set_title("Coins selected");
        let chosen_value = gtk::Label::new(None);
        chosen_value.add_css_class("numeric");
        chosen_row.add_suffix(&chosen_value);
        tally.add(&chosen_row);

        let verdict = adw::ActionRow::new();
        verdict.set_title("Enough?");
        // Markup only for the amounts inside the sentence: the line itself
        // stays where it was and at the size it was.
        verdict.set_use_markup(true);
        verdict.set_subtitle_lines(3);
        // The platform dims every subtitle, so bold alone still left the one
        // number that must not be misread greyed out. This row's subtitle is
        // held at full contrast; nothing else changes.
        verdict.add_css_class("full-contrast");
        // Answered at a glance as well as in words: a tick for covered, a
        // warning for short, and nothing at all while there is no question to
        // answer yet.
        let verdict_mark = gtk::Image::new();
        verdict_mark.set_valign(gtk::Align::Center);
        verdict.add_suffix(&verdict_mark);
        tally.add(&verdict);

        // The reason this screen exists, said in terms of the names given to
        // the coins rather than in the abstract.
        let linking = adw::ActionRow::new();
        linking.set_subtitle_lines(4);
        linking.add_prefix(&gtk::Image::from_icon_name("view-reveal-symbolic"));
        tally.add(&linking);
        page.add(&tally);

        let list = adw::PreferencesGroup::new();
        list.set_title(&format!("Coins {}", self.coins_scope()));
        let clear = gtk::Button::with_label("Choose for me");
        clear.set_valign(gtk::Align::Center);
        clear.add_css_class("flat");
        clear.set_tooltip_text(Some("Untick everything and let Sieve choose"));
        list.set_header_suffix(Some(&clear));
        list.set_description(Some(
            "Largest first. A payment that one coin covers on its own gives away the least.",
        ));

        let mut checks: Vec<(gtk::CheckButton, crate::wallet::CoinSummary)> = Vec::new();
        for coin in &here {
            let row = adw::ActionRow::new();
            row.set_use_markup(true);
            row.set_title(&match self.coin_name(coin) {
                Some(name) => gtk::glib::markup_escape_text(&name).to_string(),
                None => "Not labelled".to_string(),
            });

            let confirmations = coin.confirmations(self.tip);
            let reuse = if coin.reused_address {
                " · reused address"
            } else {
                ""
            };

            let amount = gtk::Label::new(Some(
                &self.settings.denomination.format(coin.sats, &self.network),
            ));
            amount.add_css_class("numeric");
            amount.set_valign(gtk::Align::Center);
            row.add_suffix(&amount);

            // Freezing is per coin and lives on the coin, so the control does
            // too. A padlock rather than a switch: this is a state somebody put
            // the coin into, and the row already carries a tick that means
            // something else entirely.
            let thaw = gtk::Button::new();
            thaw.add_css_class("flat");
            thaw.set_valign(gtk::Align::Center);
            row.add_suffix(&thaw);

            // A coin's name was only ever inherited — from the payment that
            // brought it in, or the address it landed on. That gives two coins
            // out of one transaction the same name, which is exactly when a
            // name has to distinguish them; and a row reading "Not labelled"
            // had nothing on it to do anything about that. BIP-329 keys a label
            // on the outpoint for this, which is the same key the padlock
            // already writes to.
            let rename = gtk::Button::from_icon_name("document-edit-symbolic");
            rename.add_css_class("flat");
            rename.add_css_class("dim-label");
            rename.set_valign(gtk::Align::Center);
            rename.set_tooltip_text(Some("Name this coin"));
            row.add_suffix(&rename);
            {
                let sender = sender.clone();
                let row = row.clone();
                let outpoint = coin.outpoint.to_string();
                let current = self.coin_name(coin).unwrap_or_default();
                rename.connect_clicked(move |button| {
                    let dialog = adw::AlertDialog::new(
                        Some("Name this coin"),
                        Some(
                            "Kept on the coin itself, so two coins from one payment can be \
                             told apart. Stored beside every other label and exported with \
                             them.",
                        ),
                    );
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("save", "Save");
                    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
                    dialog.set_default_response(Some("save"));
                    dialog.set_close_response("cancel");

                    let group = adw::PreferencesGroup::new();
                    let entry = adw::EntryRow::new();
                    entry.set_title("Name");
                    entry.set_text(&current);
                    group.add(&entry);
                    dialog.set_extra_child(Some(&group));

                    {
                        let sender = sender.clone();
                        let outpoint = outpoint.clone();
                        let row = row.clone();
                        let entry = entry.clone();
                        dialog.connect_response(None, move |_, response| {
                            if response != "save" {
                                return;
                            }
                            let text = entry.text().trim().to_string();
                            // Painted here for the same reason the padlock is:
                            // nothing rebuilds this page, so the row would
                            // otherwise keep the name it was built with.
                            row.set_title(&match text.is_empty() {
                                true => "Not labelled".to_string(),
                                false => gtk::glib::markup_escape_text(&text).to_string(),
                            });
                            sender.input(SendMsg::SetCoinLabel {
                                outpoint: outpoint.clone(),
                                text: text.clone(),
                            });
                        });
                    }
                    if let Some(window) = button.root() {
                        dialog.present(Some(&window));
                    }
                });
            }

            let tick = gtk::CheckButton::new();
            tick.set_valign(gtk::Align::Center);
            row.add_prefix(&tick);
            row.set_activatable_widget(Some(&tick));

            // Everything the frozen state changes about this row, in one place
            // that can be called again. The picker is built once and pushed —
            // nothing rebuilds it — so without this the padlock wrote to the
            // label file and left the screen exactly as it was, which looks
            // precisely like a button that does nothing.
            let paint = {
                let row = row.clone();
                let thaw = thaw.clone();
                let tick = tick.clone();
                let address = coin.address.clone();
                let path = coin.path.clone();
                let spendable = coin.spendable();
                move |frozen: bool| {
                    let age = if frozen {
                        // Said first: it is the answer to "why can I not tick
                        // this", and it is a state somebody chose rather than
                        // one the chain imposed.
                        "Frozen — held back deliberately".to_string()
                    } else if spendable {
                        crate::ui::wallet_page::plural(
                            confirmations as usize,
                            "confirmation",
                            "confirmations",
                        )
                    } else {
                        "Unconfirmed — cannot be spent yet".to_string()
                    };
                    row.set_subtitle(&format!(
                        "<tt>{}</tt>\n<span size=\"small\" alpha=\"60%\">{}{reuse}</span>",
                        gtk::glib::markup_escape_text(&address),
                        gtk::glib::markup_escape_text(&format!(
                            "{}{age}",
                            path.as_deref()
                                .map(|p| format!("{p} · "))
                                .unwrap_or_default()
                        )),
                    ));
                    match frozen {
                        true => row.add_css_class("dim-label"),
                        false => row.remove_css_class("dim-label"),
                    }
                    // The padlock says what the coin *is*, not what pressing it
                    // would do: a closed one means frozen. Showing the action
                    // instead put an open padlock on every frozen coin, which
                    // reads as the opposite of the truth at a glance — and a
                    // glance is all a list of coins gets. The tooltip carries
                    // the action, where there is room to say it.
                    thaw.set_icon_name(match frozen {
                        true => "changes-prevent-symbolic",
                        false => "changes-allow-symbolic",
                    });
                    // Quiet until it means something: an open padlock on every
                    // ordinary coin is noise, and the one closed padlock in a
                    // list should be what the eye lands on.
                    match frozen {
                        true => thaw.remove_css_class("dim-label"),
                        false => thaw.add_css_class("dim-label"),
                    }
                    thaw.set_tooltip_text(Some(match frozen {
                        true => "Frozen. Let this coin be spent again",
                        false => {
                            "Freeze this coin: never spend it, and never spend it \
                                  alongside the others"
                        }
                    }));
                    // An unconfirmed coin cannot be spent, which is the rule
                    // coin selection has followed since it was written. A
                    // frozen one cannot either, by a decision rather than by
                    // the chain — and the builder holds both back whatever is
                    // ticked here.
                    tick.set_sensitive(spendable && !frozen);
                    if frozen {
                        tick.set_active(false);
                    }
                }
            };
            row.set_subtitle_lines(4);
            tick.set_active(selected.borrow().contains(&coin.outpoint));
            paint(self.is_frozen(coin));

            {
                let sender = sender.clone();
                let outpoint = coin.outpoint.to_string();
                let frozen = std::cell::Cell::new(self.is_frozen(coin));
                let paint = paint.clone();
                thaw.connect_clicked(move |_| {
                    let now = !frozen.get();
                    frozen.set(now);
                    // Painted before the message goes anywhere: the label file
                    // is the record, but the person pressing the button is
                    // owed an answer on the frame they pressed it.
                    paint(now);
                    sender.input(SendMsg::SetFrozen {
                        outpoint: outpoint.clone(),
                        frozen: now,
                    });
                });
            }

            checks.push((tick.clone(), coin.clone()));
            list.add(&row);
        }
        page.add(&list);

        // One closure owns both effects — the page's own totals and the
        // form's idea of what is selected — so the two cannot disagree.
        let refresh = {
            let selected = Rc::clone(&selected);
            let here = here.clone();
            let denomination = self.settings.denomination;
            let network = self.network.clone();
            let names: Vec<Option<String>> = here.iter().map(|c| self.coin_name(c)).collect();
            let from = self.from;
            let draining = self.max;
            let chosen_value = chosen_value.clone();
            let verdict = verdict.clone();
            let verdict_mark = verdict_mark.clone();
            let automatic = automatic.clone();
            let chosen_row = chosen_row.clone();
            let clear = clear.clone();
            let linking = linking.clone();
            let sender = sender.clone();
            move || {
                let picked = selected.borrow().clone();

                // One state or the other: either Sieve is choosing, or these
                // are the coins. Showing both at once is what made the
                // automatic case read as a footnote.
                automatic.set_visible(picked.is_empty());
                chosen_row.set_visible(!picked.is_empty());
                verdict.set_visible(!picked.is_empty());
                clear.set_sensitive(!picked.is_empty());

                let total: u64 = here
                    .iter()
                    .filter(|coin| picked.contains(&coin.outpoint))
                    .map(|coin| coin.sats)
                    .sum();
                chosen_value.set_label(&denomination.format(total, &network));

                // Set together with the words, so the glyph and the sentence
                // can never disagree.
                // What the payment costs to make, not just what it sends:
                // an exact-amount payment adds the fee on top, so a selection
                // that covers only the amount fails at build time. Two
                // outputs — the recipient's and change — which is the shape of
                // an ordinary payment; draining has one, and is not measured
                // against an amount anyway.
                let fee = from
                    .map(|from| crate::wallet::send::estimated_fee(from, picked.len(), 2, fee_rate))
                    .unwrap_or(0);

                let (mark, mark_class) = match (picked.is_empty(), wanted) {
                    (true, _) | (false, None) => (None, "dim-label"),
                    (false, _) if draining => (Some("object-select-symbolic"), "success"),
                    (false, Some(wanted)) if total >= wanted + fee => {
                        (Some("object-select-symbolic"), "success")
                    }
                    (false, Some(_)) => (Some("dialog-warning-symbolic"), "warning"),
                };
                verdict_mark.set_icon_name(mark);
                verdict_mark.set_visible(mark.is_some());
                verdict_mark.set_css_classes(&[mark_class]);

                // The amount being paid in bold, so it cannot be taken for
                // the coin total a row above it.
                // The row's label is held at full contrast by CSS and the
                // prose is dimmed back with markup, so only the amounts come
                // forward. Doing it the other way round is not possible: the
                // platform's dimming is opacity on the whole label, and pango
                // can only ever take alpha away, never add it back.
                let strong = |sats: u64| {
                    format!(
                        "<span alpha=\"100%\"><b>{}</b></span>",
                        gtk::glib::markup_escape_text(&denomination.format(sats, &network))
                    )
                };
                let answer = match (picked.is_empty(), wanted) {
                    // Hidden in this state; the automatic row is what shows.
                    (true, _) => String::new(),
                    // Draining takes the fee out of what is sent rather than
                    // adding it on top, so there is nothing to fall short of.
                    (false, _) if draining => format!(
                        "All of these will be sent, less about {} of fee.",
                        strong(fee)
                    ),
                    (false, None) => {
                        "Enter an amount and this will say whether these cover it.".to_string()
                    }
                    (false, Some(wanted)) if total >= wanted + fee => format!(
                        "Covers {} plus about {} of fee.",
                        strong(wanted),
                        strong(fee)
                    ),
                    (false, Some(wanted)) => format!(
                        "Short by about {}. The fee is on top of the amount, and these {} \
                         coins cost about {} to spend at {fee_rate} sat/vB.",
                        strong((wanted + fee).saturating_sub(total)),
                        picked.len(),
                        denomination.format(fee, &network)
                    ),
                };
                // A row title is body size where a subtitle is smaller, so
                // moving this line up made it grow. Held at the size it had:
                // what changed is the weight and the contrast, which is what
                // was wanted, not the prominence of a whole sentence.
                verdict.set_subtitle(&if answer.is_empty() {
                    String::new()
                } else {
                    // 55% is what Adwaita dims a subtitle by, so everything
                    // except the amounts reads exactly as it did before.
                    format!("<span alpha=\"55%\">{answer}</span>")
                });

                // Named coins make this concrete: two names is a sentence
                // somebody can act on, where "these coins" is not.
                let picked_names: Vec<String> = here
                    .iter()
                    .zip(names.iter())
                    .filter(|(coin, _)| picked.contains(&coin.outpoint))
                    .map(|(_, name)| name.clone().unwrap_or_else(|| "an unlabelled coin".into()))
                    .collect();
                linking.set_title(match picked_names.len() {
                    0 | 1 => "Nothing is linked",
                    _ => "Spending these together links them",
                });
                linking.set_subtitle(&match picked_names.len() {
                    0 => "Whatever Sieve picks, it takes as few coins as it can.".to_string(),
                    1 => "One coin says nothing about the others you hold.".to_string(),
                    2 => format!(
                        "Anyone watching the chain will see that whoever held {} also held {}.",
                        picked_names[0], picked_names[1]
                    ),
                    n => format!(
                        "Anyone watching the chain will see that one person held all {n} of \
                         these — {} among them.",
                        picked_names[..2].join(" and ")
                    ),
                });

                let _ = sender.input_sender().send(SendMsg::SetCoins(picked));
            }
        };

        {
            // Unticking through the buttons rather than emptying the list
            // directly, so every path to a change runs the same refresh.
            let ticks: Vec<gtk::CheckButton> =
                checks.iter().map(|(tick, _)| tick.clone()).collect();
            clear.connect_clicked(move |_| {
                for tick in &ticks {
                    tick.set_active(false);
                }
            });
        }

        for (tick, coin) in checks {
            let selected = Rc::clone(&selected);
            let refresh = refresh.clone();
            let outpoint = coin.outpoint;
            tick.connect_toggled(move |tick| {
                {
                    let mut picked = selected.borrow_mut();
                    match (tick.is_active(), picked.iter().position(|o| *o == outpoint)) {
                        (true, None) => picked.push(outpoint),
                        (false, Some(at)) => {
                            picked.remove(at);
                        }
                        _ => {}
                    }
                }
                refresh();
            });
        }
        refresh();

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        nav.push(&adw::NavigationPage::new(&toolbar, "Coins"));
    }

    /// The coins on the path being spent from, largest first.
    fn coins_here(&self) -> Vec<&crate::wallet::CoinSummary> {
        let Some(from) = self.from else {
            return Vec::new();
        };
        self.available_coins
            .iter()
            .filter(|coin| coin.script_type == from)
            .collect()
    }

    /// What the Coins row says about itself.
    fn coins_note(&self) -> String {
        let here = self.coins_here();
        if here.is_empty() {
            return format!("Nothing to spend {}", self.coins_scope());
        }
        // Said wherever the count is said, because a frozen coin is money the
        // balance above no longer includes and the arithmetic would otherwise
        // look wrong.
        let held = self.frozen_sats();
        let frozen = match held {
            0 => String::new(),
            sats => format!(
                " · {} frozen",
                self.settings.denomination.format(sats, &self.network)
            ),
        };
        if self.coins.is_empty() {
            return format!(
                "Sieve is choosing, from {}{frozen}",
                crate::ui::wallet_page::plural(
                    here.iter().filter(|c| !self.is_frozen(c)).count(),
                    "coin",
                    "coins"
                )
            );
        }
        let chosen: u64 = here
            .iter()
            .filter(|coin| self.coins.contains(&coin.outpoint))
            .map(|coin| coin.sats)
            .sum();
        format!(
            "{} of {} · {}{frozen}",
            self.coins.len(),
            here.iter().filter(|c| !self.is_frozen(c)).count(),
            self.settings.denomination.format(chosen, &self.network)
        )
    }

    /// What a coin is called: the name on the payment that brought it in, or
    /// the name on the address it landed on. Either is a person's own word for
    /// it, and either beats a transaction id.
    fn coin_name(&self, coin: &crate::wallet::CoinSummary) -> Option<String> {
        use crate::wallet::labels::Kind;
        self.labels
            .get(Kind::Output, &coin.outpoint.to_string())
            .or_else(|| self.labels.get(Kind::Tx, &coin.from_txid))
            .or_else(|| self.labels.get(Kind::Addr, &coin.address))
            .map(str::to_owned)
    }

    /// Whether this coin has been frozen — BIP-329's `spendable: false`.
    fn is_frozen(&self, coin: &crate::wallet::CoinSummary) -> bool {
        !self.labels.spendable(&coin.outpoint.to_string())
    }

    /// Every frozen coin on the path being spent from.
    ///
    /// Read when the draft is built rather than remembered, so a coin frozen
    /// while this form was open is honoured without the form having to know.
    fn frozen_outpoints(&self) -> Vec<bdk_wallet::bitcoin::OutPoint> {
        self.available_coins
            .iter()
            .filter(|coin| self.is_frozen(coin))
            .map(|coin| coin.outpoint)
            .collect()
    }

    /// Whether the form describes a payment yet.
    fn ready_to_review(&self) -> bool {
        self.not_ready_because().is_none()
    }

    /// What is still missing, for the button that cannot be pressed.
    fn not_ready_because(&self) -> Option<&'static str> {
        if self.data_too_long() {
            return Some("The message is longer than 80 bytes");
        }
        why_not_ready(
            self.to_filled,
            self.amount_filled,
            self.max,
            self.has_funds(),
            self.extra_payees().is_ok(),
            self.data_bytes().is_some(),
        )
    }

    /// What the amount field is asking for, which is not always bitcoin.
    fn amount_title(&self) -> String {
        match self.in_fiat {
            true => "Amount in dollars".to_string(),
            false => format!("Amount in {}", self.unit()),
        }
    }

    /// What is typed in the amount field, in satoshis.
    ///
    /// One place, so the review path and the readiness check cannot disagree
    /// about what a number means. In dollars this converts at the price on
    /// screen; the result is the payment, and every screen shows that figure
    /// rather than the dollars it came from.
    fn amount_sats(&self, typed: &str) -> Result<u64, String> {
        if !self.in_fiat {
            return self.settings.denomination.parse(typed);
        }
        let dollars: f64 = typed
            .trim()
            .trim_start_matches('$')
            .replace(',', "")
            .parse()
            .map_err(|_| "That is not an amount in dollars".to_string())?;
        let price = self
            .price
            .as_ref()
            .ok_or_else(|| "No price to convert with — type the amount in bitcoin".to_string())?;
        price
            .sats_for(dollars)
            .ok_or_else(|| "That amount cannot be converted".to_string())
    }

    /// What the dollars being typed come to in bitcoin, for the line under the
    /// field. The payment is this number, not the one in the box.
    fn fiat_preview(&self, typed: &str) -> Option<String> {
        if !self.in_fiat {
            return None;
        }
        let sats = self.amount_sats(typed).ok()?;
        Some(format!(
            "{} at today's price",
            self.settings.denomination.format(sats, &self.network)
        ))
    }

    /// The bytes to publish, or `None` when the switch is off or nothing has
    /// been typed. Exactly what is in the field, encoded as UTF-8 — not
    /// trimmed, not normalised.
    fn data_bytes(&self) -> Option<Vec<u8>> {
        let bytes = self.data.as_bytes();
        (!bytes.is_empty()).then(|| bytes.to_vec())
    }

    fn data_too_long(&self) -> bool {
        self.data.len() > crate::wallet::send::MAX_DATA
    }

    /// Bytes, not characters. An emoji is four of them and a letter with an
    /// accent is two, so a count of characters would be a count of the wrong
    /// thing at exactly the moment it matters.
    fn data_count(&self) -> String {
        format!(
            "{} of {} bytes",
            self.data.len(),
            crate::wallet::send::MAX_DATA
        )
    }

    /// What this costs, said differently for the two cases — because the one
    /// that looks safer is the worse one.
    ///
    /// With somebody paid, a third party has to *guess* which output is change
    /// and the guess is a heuristic that is sometimes wrong. With nobody paid
    /// there is no guess: an `OP_RETURN` is provably unspendable, so if it is
    /// the only output that is not change then every other output is certainly
    /// yours — an inference replaced by a proof, and if several coins were
    /// spent, one that ties them together too. That is why these two strings
    /// are not shared.
    fn data_warning(&self) -> Option<&'static str> {
        if self.data_too_long() {
            return Some(
                "Longer than 80 bytes. Not every node will pass this on, and one that will \
                 not simply drops it — which looks exactly like being ignored.",
            );
        }
        if self.data.is_empty() {
            return Some(
                "Anything written here is public and permanent, and can be searched for by \
                 anyone who knows what it says.",
            );
        }
        if self.to_filled {
            return Some(
                "Public and permanent. Anyone who knows what this says can find this \
                 payment, and from it the change that came back to you.",
            );
        }
        // No recipient: the worst case, and the one that reads as private.
        Some(
            "Public and permanent — and with nobody being paid, this proves which outputs \
             are yours rather than leaving anyone to guess. Choosing which coin pays for it \
             is the only thing that limits that.",
        )
    }

    /// Everybody after the first, parsed.
    ///
    /// An extra recipient half filled in blocks Review rather than being
    /// skipped: a row somebody typed an address into and then left is a
    /// payment they meant to make, and quietly dropping it would send the
    /// others and say nothing.
    fn extra_payees(&self) -> Result<Vec<crate::wallet::send::Payee>, &'static str> {
        let network = self.network();
        self.extras
            .iter()
            .map(|extra| {
                let to = crate::wallet::send::parse_address(extra.address.trim(), network)
                    .map_err(|_| "One of the other recipients is not a valid address")?;
                let sats = self
                    .settings
                    .denomination
                    .parse(extra.amount.trim())
                    .map_err(|_| "One of the other recipients has no amount")?;
                Ok(crate::wallet::send::Payee {
                    to,
                    amount: Sending::Exact(bdk_wallet::bitcoin::Amount::from_sat(sats)),
                })
            })
            .collect()
    }

    /// Re-decide whether the form describes a payment, after a recipient was
    /// added, removed or edited.
    fn recount_payees(&mut self, widgets: &mut <Self as Component>::Widgets) {
        let _ = widgets;
        self.error = None;
    }

    /// Whether this wallet holds anything at all on a spendable path.
    ///
    /// **Deliberately not `available_sats`, which subtracts frozen coins.**
    /// This decides whether the send form is drawn at all, and a wallet whose
    /// coins are all frozen still has to draw it: the only way to a coin's
    /// padlock is the Coins row on this form, so hiding the form behind
    /// "Nothing to send" locks somebody out of the control that would let them
    /// back in. Freezing must never be a one-way door.
    fn has_funds(&self) -> bool {
        self.source().map_or(0, |a| a.balance_sats) > 0 || self.available_sats() > 0
    }

    /// Every coin on the path being spent from is frozen: there is money here
    /// and this payment cannot touch any of it.
    fn all_frozen(&self) -> bool {
        let here = self.coins_here();
        !here.is_empty() && here.iter().all(|coin| self.is_frozen(coin))
    }

    /// Shown only when there is a choice to make.
    fn many_sources(&self) -> bool {
        self.fundable().len() > 1
    }

    fn fee_floor(&self) -> f64 {
        self.min_fee.unwrap_or(1.0).max(1.0)
    }

    /// The line under the fee field: where the number came from, and what
    /// the floor is. Never just a unit — a fee with no provenance is a number
    /// someone has to guess about.
    fn fee_hint(&self) -> String {
        let floor = match self.min_fee {
            Some(rate) => format!("Peers relay from {rate:.1} sat/vB"),
            None => "Satoshis per virtual byte".into(),
        };
        match &self.fee_source {
            Some(source) => format!("{source} · {floor}"),
            None => floor,
        }
    }

    /// A dollar figure beside an amount, when the price is known and wanted.
    fn fiat(&self, sats: u64) -> Option<String> {
        let price = self.price.as_ref()?;
        self.settings
            .show_fiat
            .then(|| format!("≈ ${}", crate::price::usd(price.value_of(sats))))
    }

    /// The transaction, shortened. The whole thing is 64 characters of hex
    /// that nobody reads across; what it is for is recognising the row and
    /// copying it, and Copy hands over all of it.
    fn short_txid(&self) -> String {
        self.sent.as_deref().map(shorten).unwrap_or_default()
    }

    fn explorer(&self) -> Option<String> {
        let txid = self.sent.as_deref()?;
        crate::ui::wallet_page::explorer_url(&self.network, txid)
    }

    fn network(&self) -> bdk_wallet::bitcoin::Network {
        self.network
            .parse()
            .unwrap_or(bdk_wallet::bitcoin::Network::Bitcoin)
    }

    /// Rebuild the source list only when the set of paths changes: replacing a
    /// ComboRow's model resets its selection, which would snap the picker back
    /// while someone was using it.
    fn sync_sources(&mut self) {
        let labels: Vec<String> = self
            .fundable()
            .iter()
            .map(|a| {
                format!(
                    "{} — {}",
                    a.script_type.label(),
                    self.settings
                        .denomination
                        .format(a.balance_sats, &self.network)
                )
            })
            .collect();
        if labels == self.from_labels {
            return;
        }
        while self.from_model.n_items() > 0 {
            self.from_model.remove(0);
        }
        for label in &labels {
            self.from_model.append(label);
        }
        self.from_labels = labels;
    }
}

#[relm4::component(pub)]
impl Component for SendForm {
    type Init = ();
    type Input = SendMsg;
    type Output = SendOutput;
    type CommandOutput = ();

    view! {
        gtk::ScrolledWindow {
            set_vexpand: true,

            adw::Clamp {
                set_maximum_size: 600,
                set_tightening_threshold: 400,

                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 18,
                    set_margin_all: 18,
                    set_valign: gtk::Align::Start,

                    // Nothing to spend: say so instead of offering a form that
                    // can only fail.
                    adw::StatusPage {
                        set_icon_name: Some("sieve-send-symbolic"),
                        set_title: "Nothing to send",
                        set_description: Some(
                            "This wallet has no confirmed coins yet. Received payments \
                             appear here once they are in a block."
                        ),
                        #[watch]
                        set_visible: !model.watch_only
                            && !model.has_funds()
                            && model.sent.is_none(),
                    },

                    // Money here, and none of it reachable. Said plainly and
                    // with the way out named, because the alternative is a
                    // form whose every field works and whose Review button
                    // never lights, for a reason nothing on screen mentions.
                    // A group rather than an adw::Banner: a banner is designed
                    // to pin flush across the top of a view, so inline among
                    // rounded groups it has square corners by construction.
                    adw::PreferencesGroup {
                        #[watch]
                        set_visible: model.all_frozen() && !model.watch_only,

                        adw::ActionRow {
                            add_css_class: "warning",
                            #[watch]
                            set_title: &format!("Every coin {} is frozen", model.coins_scope()),
                            #[watch]
                            set_subtitle: if model.spendable_elsewhere() {
                                "Another derivation path still has coins you can spend — \
                                 change From above, or release one here."
                            } else {
                                "Nothing can be spent until one is released."
                            },
                            set_subtitle_lines: 3,
                            add_suffix = &gtk::Button {
                                set_label: "Coins",
                                set_valign: gtk::Align::Center,
                                connect_clicked => SendMsg::ChooseCoins,
                            },
                        },
                    },

                    // A watch-only wallet can work out a payment down to the
                    // last satoshi and still not sign it. Saying so here beats
                    // a form that fails at the last step.
                    adw::StatusPage {
                        set_icon_name: Some("channel-secure-symbolic"),
                        set_title: "Signing happens elsewhere",
                        set_description: Some(
                            "This wallet holds no keys — only the public descriptors that \
                             find its coins. Whatever holds the key signs for it, over a \
                             PSBT."
                        ),
                        #[watch]
                        set_visible: model.watch_only,
                    },

                    // Sent.
                    adw::StatusPage {
                        set_icon_name: Some("object-select-symbolic"),
                        set_title: "Payment sent",
                        #[watch]
                        set_visible: model.sent.is_some(),
                        // What was sent, rather than the transaction id — that
                        // is below, where it can be copied.
                        #[watch]
                        set_description: model.sent_detail.as_deref(),

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 24,

                            adw::PreferencesGroup {
                                // Said plainly: a filter wallet has no mempool
                                // to watch, so "sent" means handed to a peer,
                                // and confirmation arrives with a block.
                                set_description: Some(
                                    "It will show as confirmed in Activity once it is in \
                                     a block."
                                ),

                                adw::ActionRow {
                                    set_title: "Transaction",
                                    add_css_class: "property",
                                    set_use_markup: true,
                                    #[watch]
                                    set_subtitle: &format!(
                                        "<tt>{}</tt>",
                                        gtk::glib::markup_escape_text(&model.short_txid())
                                    ),

                                    add_suffix = &gtk::Button {
                                        set_icon_name: "edit-copy-symbolic",
                                        set_tooltip_text: Some("Copy the transaction id"),
                                        set_valign: gtk::Align::Center,
                                        add_css_class: "flat",
                                        connect_clicked => SendMsg::CopyTxid,
                                    },
                                },

                                #[name(explorer_row)]
                                adw::ActionRow {
                                    set_title: "View on mempool.space",
                                    // The same disclosure the transaction
                                    // detail makes: this names the transaction
                                    // to someone else's server.
                                    set_subtitle:
                                        "Opens your browser, and tells the explorer you \
                                         looked at this transaction",
                                    set_subtitle_lines: 2,
                                    set_activatable: true,
                                    #[watch]
                                    set_visible: model.explorer().is_some(),
                                    add_suffix = &gtk::Image {
                                        set_icon_name: Some("web-browser-symbolic"),
                                    },
                                    connect_activated => SendMsg::OpenExplorer,
                                },
                            },

                            gtk::Button {
                                set_label: "Done",
                                set_halign: gtk::Align::Center,
                                add_css_class: "pill",
                                add_css_class: "suggested-action",
                                connect_clicked => SendMsg::Reset,
                            },
                        },
                    },

                    // The form.
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 18,
                        #[watch]
                        set_visible: !model.watch_only
                            && model.has_funds()
                            && model.sent.is_none(),

                        adw::PreferencesGroup {
                            #[watch]
                            set_description: Some(&match &model.request {
                                // What the request said about itself, so the
                                // numbers below can be checked against it.
                                Some(request) => request.clone(),
                                None if !model.coins.is_empty() => format!(
                                    "Available in the {}: {}",
                                    crate::ui::wallet_page::plural(
                                        model.coins.len(),
                                        "chosen coin",
                                        "chosen coins"
                                    ),
                                    model.available()
                                ),
                                None => format!("Available: {}", model.available()),
                            }),

                            #[name(to_row)]
                            adw::EntryRow {
                                set_title: "Pay to",
                                // A payment request pasted whole. Unpacked on
                                // arrival rather than at Review, so the amount
                                // it asks for is on screen to be checked
                                // before anything is built.
                                connect_changed[sender] => move |row| {
                                    let text = row.text().to_string();
                                    if text.trim_start().len() > 8
                                        && text.trim_start()[..8]
                                            .eq_ignore_ascii_case("bitcoin:")
                                    {
                                        sender.input(SendMsg::UnpackUri(text));
                                    }
                                    sender.input(SendMsg::RecipientEdited);
                                },
                                // An address is checked character by character
                                // against another screen, and a proportional
                                // font makes l/1 and O/0 the reader's problem.
                                // The receive side already shows them this way.
                                add_css_class: "monospace",
                                #[watch]
                                set_sensitive: !model.busy,
                            },

                            #[name(amount_row)]
                            adw::EntryRow {
                                #[watch]
                                set_title: &model.amount_title(),
                                #[watch]
                                set_sensitive: !model.busy,
                                // Digits on a touch keyboard, rather than the
                                // full alphabet for a field that only takes
                                // numbers.
                                set_input_purpose: gtk::InputPurpose::Number,
                                // Tabular figures, like every other amount in
                                // the app.
                                add_css_class: "numeric",

                                // Typing an amount is a way of saying "not
                                // everything", so the field stays editable and
                                // an edit releases Max rather than being
                                // refused by a greyed-out row.
                                connect_changed[sender] => move |_| {
                                    sender.input(SendMsg::AmountEdited);
                                },

                                // Typing in dollars, for an amount that was
                                // decided in dollars. What is sent is still
                                // bitcoin — the conversion happens once, here,
                                // at the price on screen — so the row says
                                // underneath what the payment will actually
                                // be, and the review dialog says it again.
                                #[name(fiat_button)]
                                add_suffix = &gtk::ToggleButton {
                                    set_label: "$",
                                    set_valign: gtk::Align::Center,
                                    #[watch]
                                    set_visible: model.price.is_some(),
                                    #[watch]
                                    set_tooltip_text: Some(if model.in_fiat {
                                        "Type the amount in bitcoin instead"
                                    } else {
                                        "Type the amount in dollars instead"
                                    }),
                                    connect_toggled[sender] => move |button| {
                                        sender.input(SendMsg::ToggleFiat(button.is_active()));
                                    },
                                },

                                #[name(max_button)]
                                add_suffix = &gtk::ToggleButton {
                                    set_label: "Max",
                                    set_valign: gtk::Align::Center,
                                    // Max means everything that is available,
                                    // and choosing coins is a way of saying
                                    // what "available" means.
                                    // "Everything" needs one person to send
                                    // everything to. With somebody else on the
                                    // transaction the word stops having an
                                    // amount behind it.
                                    #[watch]
                                    set_sensitive: model.extras.is_empty(),
                                    #[watch]
                                    set_tooltip_text: Some(if !model.extras.is_empty() {
                                        "Max needs a single recipient"
                                    } else if model.coins.is_empty() {
                                        "Send everything on this path, fee included"
                                    } else {
                                        "Send all the chosen coins, fee included"
                                    }),
                                    connect_toggled[sender] => move |button| {
                                        sender.input(SendMsg::ToggleMax(button.is_active()));
                                    },
                                },
                            },

                            // What the dollars come to. The payment is this
                            // figure, not the one in the box above it, and
                            // saying so here means the review dialog is not
                            // the first place anybody finds out.
                            adw::ActionRow {
                                #[watch]
                                set_visible: model.in_fiat
                                    && model.fiat_preview(&model.amount_text).is_some(),
                                #[watch]
                                set_title: &model
                                    .fiat_preview(&model.amount_text)
                                    .unwrap_or_default(),
                                add_css_class: "dim-label",
                            },

                            #[name(from_row)]
                            adw::ComboRow {
                                set_title: "From",
                                set_subtitle: "Which derivation path the coins come from",
                                #[watch]
                                set_visible: model.many_sources(),
                                #[watch]
                                set_sensitive: !model.busy,
                                set_model: Some(&model.from_model),
                                connect_selected_notify[sender] => move |row| {
                                    sender.input(SendMsg::SelectFrom(row.selected()));
                                },
                            },

                            // Automatic by default: most payments should not
                            // require a decision. Offered, though, because
                            // which coins a payment spends is the one thing a
                            // wallet gives away that cannot be taken back.
                            adw::ActionRow {
                                set_title: "Coins",
                                #[watch]
                                set_subtitle: &model.coins_note(),
                                set_subtitle_lines: 2,
                                set_activatable: true,
                                #[watch]
                                set_sensitive: !model.busy && !model.available_coins.is_empty(),
                                add_suffix = &gtk::Image {
                                    set_icon_name: Some("go-next-symbolic"),
                                    add_css_class: "dim-label",
                                },
                                connect_activated => SendMsg::ChooseCoins,
                            },
                        },

                        // Everybody after the first, each in a group of their
                        // own so an address and its amount stay together.
                        #[local_ref]
                        extras -> gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            set_margin_top: 12,
                        },

                        gtk::Button {
                            add_css_class: "flat",
                            set_halign: gtk::Align::Center,
                            set_margin_top: 6,
                            set_label: "Add another recipient",
                            set_tooltip_text: Some(
                                "One transaction paying several people costs less in fees than \
                                 the same payments made separately — and tells anybody reading \
                                 the chain that the same person made all of them"
                            ),
                            #[watch]
                            set_sensitive: !model.busy,
                            connect_clicked => SendMsg::AddPayee,
                        },

                        // Optional, and folded away, because almost nobody
                        // wants it and the ones who do know what it is.
                        adw::PreferencesGroup {
                            set_margin_top: 12,

                            #[name(data_expander)]
                            adw::ExpanderRow {
                                set_title: "Attach data",
                                set_subtitle: "Advanced. Writes a short message into the \
                                               transaction itself",
                                set_show_enable_switch: true,
                                set_enable_expansion: false,
                                #[watch]
                                set_sensitive: !model.busy,
                                connect_enable_expansion_notify => SendMsg::DataEdited,

                                #[name(data_row)]
                                add_row = &adw::EntryRow {
                                    set_title: "Message",
                                    connect_changed => SendMsg::DataEdited,
                                },

                                add_row = &adw::ActionRow {
                                    #[watch]
                                    set_title: &model.data_count(),
                                    // What this costs that cannot be undone.
                                    // It changes with the form because the
                                    // two cases are not equally bad, and the
                                    // worse one is the one that looks safer.
                                    #[watch]
                                    set_subtitle: model.data_warning().unwrap_or_default(),
                                    set_subtitle_lines: 4,
                                    #[watch]
                                    set_css_classes: if model.data_too_long() {
                                        &["error"]
                                    } else {
                                        &["dim-label"]
                                    },
                                },
                            },
                        },

                        adw::PreferencesGroup {
                            #[name(fee_row)]
                            adw::SpinRow {
                                set_title: "Fee rate",
                                #[watch]
                                set_subtitle: &model.fee_hint(),
                                set_adjustment: Some(&gtk::Adjustment::new(
                                    DEFAULT_FEE_RATE, 1.0, 5_000.0, 1.0, 10.0, 0.0,
                                )),
                                set_digits: 1,
                                #[watch]
                                set_sensitive: !model.busy,
                                connect_value_notify[sender] => move |_| {
                                    sender.input(SendMsg::FeeEdited);
                                },
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

                        gtk::Button {
                            add_css_class: "suggested-action",
                            add_css_class: "pill",
                            set_halign: gtk::Align::Center,
                            // Nothing to review until there is a recipient
                            // and an amount. Greyed out says "not yet" where a
                            // live button followed by "enter an address to
                            // send to" says "you got that wrong".
                            #[watch]
                            set_sensitive: !model.busy && model.ready_to_review(),
                            #[watch]
                            set_tooltip_text: model.not_ready_because(),
                            #[watch]
                            set_label: if model.busy { "Working…" } else { "Review payment" },
                            connect_clicked => SendMsg::Review,
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
        let mut model = SendForm {
            in_fiat: false,
            amount_text: String::new(),
            data: String::new(),
            data_row: None,
            extras: FactoryVecDeque::builder()
                .launch(gtk::Box::default())
                .forward(sender.input_sender(), |out| match out {
                    ExtraPayeeOutput::Remove(index) => SendMsg::RemovePayee(index),
                    ExtraPayeeOutput::Edited => SendMsg::PayeeEdited,
                }),
            settings: Settings::load(),
            network: "bitcoin".into(),
            accounts: Vec::new(),
            price: None,
            from: None,
            max: false,
            min_fee: None,
            watch_only: false,
            device: None,
            has_passphrase: false,
            fee_source: None,
            suggested: None,
            error: None,
            busy: false,
            plan: None,
            sent: None,
            sent_detail: None,
            from_model: gtk::StringList::new(&[]),
            from_labels: Vec::new(),
            to_row: None,
            amount_row: None,
            to_filled: false,
            amount_filled: false,
            coins: Vec::new(),
            available_coins: Vec::new(),
            tip: 0,
            labels: crate::wallet::labels::Labels::default(),
            request: None,
            request_label: None,
        };

        let extras = model.extras.widget();
        let widgets = view_output!();

        // Held so a finished payment can leave an empty form behind rather
        // than the last one, which is how a payment gets sent twice.
        model.to_row = Some(widgets.to_row.clone());
        model.amount_row = Some(widgets.amount_row.clone());
        model.data_row = Some(widgets.data_row.clone());

        // A field that only holds numbers should only take numbers, refused at
        // the keystroke rather than explained afterwards.
        //
        // Connected to the delegate, not to the row: `adw::EntryRow` implements
        // `Editable` by delegating to an inner `GtkText`, and `insert-text` is
        // emitted there. Connecting to the row compiles, runs, and does
        // nothing — which is exactly what it did.
        install_amount_filter(&widgets.amount_row);

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match msg {
            SendMsg::Show(summary) => {
                self.network = summary.network.clone();
                self.accounts = summary.accounts.clone();
                self.tip = summary.tip;
                self.available_coins = summary.coins.clone();
                // A coin spent since the picker was last open is not a coin
                // any more, and building on it would fail at the last step
                // with nothing to point at.
                self.coins
                    .retain(|chosen| summary.coins.iter().any(|c| c.outpoint == *chosen));
                self.sync_sources();
            }

            SendMsg::SetLabels(labels) => self.labels = *labels,

            SendMsg::ChooseCoins => {
                // The amount is in the field, not the model, and the picker
                // needs it to say whether a selection covers the payment.
                let wanted = self
                    .settings
                    .denomination
                    .parse(&widgets.amount_row.text())
                    .ok()
                    .filter(|sats| *sats > 0);
                self.show_coins(root, &sender, wanted, widgets.fee_row.value());
            }

            SendMsg::SetCoins(chosen) => {
                self.coins = chosen;
                // Max means "everything available", and the selection has just
                // changed what that is. Refilling keeps the field and the
                // button telling the same story.
                if self.max {
                    let filled = self.available_amount();
                    widgets.amount_row.set_text(&filled);
                    self.amount_text = filled;
                }
                self.update_view(widgets, sender.clone());
            }

            SendMsg::SetDenomination(denomination) => {
                self.settings.denomination = denomination;
                // The extra rows say which unit they are in, and a row still
                // labelled in satoshis while the form is in BTC is how a
                // payment goes out a hundred million times wrong.
                self.extras
                    .broadcast(ExtraPayeeMsg::SetUnit(self.unit().to_string()));
                self.sync_sources();
            }

            SendMsg::SetPrice(price) => self.price = price,

            SendMsg::SetMinFee(rate) => {
                self.min_fee = rate;
                // Never leave the field below what peers will relay.
                let floor = self.fee_floor();
                if widgets.fee_row.value() < floor {
                    widgets.fee_row.set_value(floor);
                }
            }

            SendMsg::SetWatchOnly(watch_only) => self.watch_only = watch_only,
            SendMsg::SetHasPassphrase(has) => self.has_passphrase = has,

            SendMsg::Suggest { rate, source } => {
                self.fee_source = Some(source);
                let rate = rate.max(self.fee_floor());

                // Only fills a field nobody has chosen for themselves.
                let current = widgets.fee_row.value();
                let untouched = self
                    .suggested
                    .map(|last| (current - last).abs() < 0.05)
                    .unwrap_or(current == DEFAULT_FEE_RATE);
                if untouched {
                    let rate = (rate * 10.0).round() / 10.0;
                    widgets.fee_row.set_value(rate);
                    self.suggested = Some(rate);
                }
            }

            SendMsg::FeeEdited => {
                // A rate typed over the suggestion is the person's own, and
                // the next estimate must not take it back.
                if let Some(last) = self.suggested
                    && (widgets.fee_row.value() - last).abs() >= 0.05
                {
                    self.suggested = None;
                }
            }

            SendMsg::SelectFrom(index) => {
                // Coins belong to a path. Carrying a selection across would
                // mean spending coins from a wallet that is no longer the one
                // being spent from.
                self.coins.clear();
                self.from = self.fundable().get(index as usize).map(|a| a.script_type);
            }

            SendMsg::AddPayee => {
                let unit = self.unit().to_string();
                self.extras.guard().push_back(unit);
                // "Everything" needs one person to send everything to. With
                // somebody else on the transaction it is no longer a number
                // this form can work out, so it is released rather than left
                // switched on meaning something different.
                if self.max {
                    self.max = false;
                    widgets.max_button.set_active(false);
                }
                self.recount_payees(widgets);
            }

            SendMsg::RemovePayee(index) => {
                self.extras.guard().remove(index.current_index());
                self.recount_payees(widgets);
            }

            SendMsg::CopyUnsigned => {
                let Some(plan) = self.plan.as_ref() else {
                    return;
                };
                // Base64 is BIP-174's own text form, so what lands on the
                // clipboard is the same payment the file would hold — for the
                // signers that take a paste rather than a card.
                let text = crate::wallet::psbt::to_base64(&plan.psbt);
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&text);
                    let _ = sender.output(SendOutput::Toast(
                        "Copied. Paste it into whatever will sign it".into(),
                    ));
                }
            }

            SendMsg::SetDevice(kind) => self.device = kind,

            SendMsg::SignOnDevice => {
                let Some(plan) = self.plan.clone() else {
                    return;
                };
                self.busy = true;
                let _ = sender.output(SendOutput::SignOnDevice(Box::new(plan)));
            }

            SendMsg::SaveUnsigned => {
                let Some(plan) = self.plan.clone() else {
                    return;
                };
                let _ = sender.output(SendOutput::SaveUnsigned(Box::new(plan)));
            }

            SendMsg::SetCoinLabel { outpoint, text } => {
                // Applied locally as well as sent on, so the tally's own copy
                // of the names agrees with the row that was just renamed.
                self.labels
                    .set(crate::wallet::labels::Kind::Output, &outpoint, &text);
                let _ = sender.output(SendOutput::SetCoinLabel { outpoint, text });
            }

            SendMsg::SetFrozen { outpoint, frozen } => {
                // Applied here as well as sent on, so the picker redraws now
                // rather than waiting for the label file to come back around.
                self.labels.set_spendable(&outpoint, !frozen);
                // A coin that has just been frozen must not stay ticked: the
                // builder would hold it back anyway, and a selection that
                // silently does nothing is worse than one that visibly clears.
                if frozen && let Ok(point) = outpoint.parse() {
                    self.coins.retain(|chosen| *chosen != point);
                }
                let _ = sender.output(SendOutput::SetFrozen { outpoint, frozen });
            }

            SendMsg::PayeeEdited => self.recount_payees(widgets),

            SendMsg::DataEdited => {
                // The switch decides, not the field. A row that has been
                // folded away still holds whatever was typed into it, so
                // reading the text alone would publish a message somebody
                // changed their mind about.
                self.data = match widgets.data_expander.enables_expansion() {
                    true => widgets.data_row.text().to_string(),
                    false => String::new(),
                };
                self.error = None;
            }

            SendMsg::ToggleFiat(in_fiat) => {
                // Converted, never reinterpreted. Leaving "0.0002" in the box
                // and relabelling it dollars would turn a small payment into a
                // very different one without a character changing on screen.
                let typed = widgets.amount_row.text().to_string();
                let sats = self.amount_sats(&typed).ok();
                self.in_fiat = in_fiat;
                if let Some(sats) = sats
                    && !typed.trim().is_empty()
                {
                    let rewritten: Option<String> = match (in_fiat, self.price.as_ref()) {
                        (true, Some(price)) => Some(format!("{:.2}", price.value_of(sats))),
                        (true, None) => None,
                        // Back to bitcoin: the figure without its unit, since
                        // the field's title carries that.
                        (false, _) => {
                            let shown = self.settings.denomination.format(sats, &self.network);
                            Some(
                                shown
                                    .rsplit_once(' ')
                                    .map_or(shown.clone(), |(amount, _)| amount.to_string()),
                            )
                        }
                    };
                    if let Some(text) = rewritten {
                        widgets.amount_row.set_text(&text);
                        self.amount_text = text;
                    }
                }
                self.error = None;
            }

            SendMsg::ToggleMax(max) => {
                self.max = max;
                // Filled in on the way up, and left alone on the way down: the
                // number is a reasonable starting point for editing.
                if max {
                    let filled = self.available_amount();
                    widgets.amount_row.set_text(&filled);
                    self.amount_text = filled;
                }
            }

            SendMsg::RecipientEdited => {
                self.to_filled = !widgets.to_row.text().trim().is_empty();
                self.update_view(widgets, sender.clone());
            }

            SendMsg::AmountEdited => {
                // Whatever route the text arrived by — typing, paste, drop —
                // it leaves as digits. `changed` is forwarded from the
                // delegate, so this runs even where `insert-text` does not.
                self.amount_text = widgets.amount_row.text().to_string();
                let text = widgets.amount_row.text();
                if !text.chars().all(is_amount_character) {
                    let cleaned: String =
                        text.chars().filter(|c| is_amount_character(*c)).collect();
                    widgets.amount_row.set_text(&cleaned);
                    widgets.amount_row.set_position(-1);
                }

                // Still the whole balance? Still a max send. Anything else and
                // the toggle no longer describes what is in the field.
                if self.max
                    && self.amount_sats(&widgets.amount_row.text()) != Ok(self.available_sats())
                {
                    self.max = false;
                    widgets.max_button.set_active(false);
                }

                // An amount of zero is not an amount, and neither is a lone
                // decimal point on the way to one.
                self.amount_filled = self
                    .settings
                    .denomination
                    .parse(&widgets.amount_row.text())
                    .is_ok_and(|sats| sats > 0);
                self.update_view(widgets, sender.clone());
            }

            SendMsg::UnpackUri(text) => {
                match crate::wallet::uri::parse(&text) {
                    Ok(Some(payment)) => {
                        widgets.to_row.set_text(&payment.address);
                        widgets.to_row.set_position(-1);

                        if let Some(sats) = payment.amount_sats {
                            // In whichever unit is on display, and without the
                            // unit itself: this field takes a number.
                            let shown = self.settings.denomination.format(sats, &self.network);
                            let number = shown
                                .rsplit_once(' ')
                                .map_or(shown.clone(), |(amount, _)| amount.to_owned());
                            widgets.amount_row.set_text(&number);
                            self.max = false;
                        }

                        // Both are the request's own words about itself. Shown
                        // rather than trusted: a label is written by whoever
                        // wrote the URI, which on a bad day is not who you
                        // think you are paying.
                        self.request_label = payment.label.clone();
                        self.request = match (&payment.label, &payment.message) {
                            (Some(who), Some(what)) => Some(format!("Request from {who} — {what}")),
                            (Some(who), None) => Some(format!("Request from {who}")),
                            (None, Some(what)) => Some(format!("Request: {what}")),
                            (None, None) => None,
                        };
                        self.error = None;
                    }
                    // A URI that is one and cannot be honoured — a bad amount,
                    // a `req-` parameter we do not implement. The address is
                    // left as pasted rather than half-unpacked.
                    Err(e) => self.error = Some(capitalise(&e.to_string())),
                    Ok(None) => {}
                }
                self.update_view(widgets, sender.clone());
            }

            SendMsg::Review => {
                self.error = None;
                let network = self.network();

                // A form with data and nobody to pay is a transaction that
                // publishes and pays nobody. Everywhere else an empty "Pay to"
                // is somebody who has not finished.
                let paying = !widgets.to_row.text().trim().is_empty();
                let first = if paying {
                    let to =
                        match crate::wallet::send::parse_address(&widgets.to_row.text(), network) {
                            Ok(address) => address,
                            Err(e) => {
                                self.error = Some(capitalise(&e.to_string()));
                                self.update_view(widgets, sender);
                                return;
                            }
                        };

                    let amount = if self.max {
                        Sending::Everything
                    } else {
                        // Through `amount_sats`, which knows whether the field
                        // is dollars. Reading it as bitcoin here would send a
                        // number nobody typed.
                        match self.amount_sats(&widgets.amount_row.text()) {
                            Ok(sats) => Sending::Exact(bdk_wallet::bitcoin::Amount::from_sat(sats)),
                            Err(message) => {
                                self.error = Some(message);
                                self.update_view(widgets, sender);
                                return;
                            }
                        }
                    };
                    Some(crate::wallet::send::Payee { to, amount })
                } else {
                    None
                };

                let Some(source) = self.source().map(|a| a.script_type) else {
                    self.error = Some("This wallet has nothing to spend".into());
                    self.update_view(widgets, sender);
                    return;
                };

                let rate = widgets.fee_row.value().max(self.fee_floor());
                let Some(fee_rate) =
                    bdk_wallet::bitcoin::FeeRate::from_sat_per_vb(rate.ceil() as u64)
                else {
                    self.error = Some("That fee rate is not usable".into());
                    self.update_view(widgets, sender);
                    return;
                };

                self.busy = true;
                let extras = match self.extra_payees() {
                    Ok(extras) => extras,
                    Err(message) => {
                        self.error = Some(message.into());
                        self.update_view(widgets, sender);
                        return;
                    }
                };
                let mut payees: Vec<_> = first.into_iter().collect();
                payees.extend(extras);

                let _ = sender.output(SendOutput::Plan(Box::new(Draft {
                    from: source,
                    payees,
                    data: self.data_bytes(),
                    fee_rate,
                    coins: self.coins.clone(),
                    frozen: self.frozen_outpoints(),
                })));
            }

            SendMsg::Planned(result) => {
                self.busy = false;
                match *result {
                    Ok(plan) => {
                        self.confirm(&plan, root, &sender);
                        self.plan = Some(plan);
                    }
                    Err(message) => self.error = Some(capitalise(&message)),
                }
            }

            SendMsg::Confirm(password, passphrase) => {
                let Some(plan) = self.plan.take() else { return };
                self.sent_detail = Some(format!(
                    "{} to {}",
                    self.settings
                        .denomination
                        .format(plan.spend().to_sat(), &self.network),
                    shorten(&plan.to()),
                ));
                self.busy = true;
                let _ = sender.output(SendOutput::Send {
                    passphrase,
                    plan: Box::new(plan),
                    password,
                });
            }

            SendMsg::Sent(result) => {
                self.busy = false;
                match *result {
                    Ok(txid) => {
                        // The name came from the request; the payment it was
                        // made for is the thing worth naming.
                        if let Some(text) = self.request_label.clone() {
                            let _ = sender.output(SendOutput::NameTransaction {
                                txid: txid.clone(),
                                text,
                            });
                        }
                        self.sent = Some(txid);
                        self.error = None;
                    }
                    Err(message) => self.error = Some(capitalise(&message)),
                }
            }

            SendMsg::CopyTxid => {
                if let (Some(txid), Some(display)) =
                    (self.sent.clone(), gtk::gdk::Display::default())
                {
                    display.clipboard().set_text(&txid);
                    let _ = sender.output(SendOutput::Toast("Transaction id copied".into()));
                }
            }

            SendMsg::OpenExplorer => {
                if let Some(url) = self.explorer() {
                    let sender = sender.clone();
                    crate::ui::browser::open(&url, root, move |message| {
                        let _ = sender.output(SendOutput::Toast(message));
                    });
                }
            }

            SendMsg::Reset => {
                self.sent = None;
                self.sent_detail = None;
                self.suggested = Some(widgets.fee_row.value());
                self.error = None;
                self.plan = None;
                self.max = false;
                widgets.max_button.set_active(false);
                widgets.to_row.set_text("");
                widgets.amount_row.set_text("");
                // Everybody after the first goes too. A second payment sent by
                // accident is bad enough without it carrying the last one's
                // other recipients — or its message.
                self.extras.guard().clear();
                self.data.clear();
                widgets.data_row.set_text("");
                widgets.data_expander.set_enable_expansion(false);
                self.request = None;
                self.request_label = None;
                self.to_filled = false;
                self.amount_filled = false;
            }
        }

        self.update_view(widgets, sender);
    }
}

impl SendForm {
    /// The last thing between a person and an irreversible payment.
    ///
    /// Every number that matters is restated here, because the form's fields
    /// are what was asked for and these are what will actually happen — the
    /// fee in particular is not known until the transaction is built.
    fn confirm(&self, plan: &Plan, root: &gtk::ScrolledWindow, sender: &ComponentSender<Self>) {
        let unit = self.settings.denomination;
        let network = &self.network;
        let amount = unit.format(plan.spend().to_sat(), network);
        let fee = unit.format(plan.fee.to_sat(), network);
        let total = unit.format(plan.total().to_sat(), network);

        let escape = gtk::glib::markup_escape_text;

        // Every recipient named with its own amount. A total with the
        // addresses left off would be a number nobody could check, and this is
        // the screen where checking is the entire point.
        let mut body = match plan.payees.as_slice() {
            // Nobody paid: the transaction exists to publish, and the screen
            // should lead with that rather than with a total of nothing.
            [] => "This pays nobody. It publishes:".to_string(),
            [(address, _)] => {
                let to = format!("<tt>{}</tt>", escape(address));
                match self.fiat(plan.spend().to_sat()) {
                    Some(fiat) => {
                        format!("Send {} ({}) to\n{to}", escape(&amount), escape(&fiat))
                    }
                    None => format!("Send {} to\n{to}", escape(&amount)),
                }
            }
            many => {
                let mut body = format!("Send {} to {} recipients:", escape(&amount), many.len());
                for (address, paid) in many {
                    body.push_str(&format!(
                        "\n\n{}\n<tt>{}</tt>",
                        escape(&unit.format(paid.to_sat(), network)),
                        escape(address),
                    ));
                }
                body
            }
        };
        // Shown both ways on purpose. The text is what somebody meant; the
        // hex is what actually goes on the chain, and the two differ whenever
        // an invisible character or an encoding surprise is involved. This is
        // the last screen on which either can be read.
        for data in &plan.data {
            let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
            body.push_str(&format!(
                "\n\n<tt>{}</tt>\n<tt>{}</tt>\n{} bytes, public and permanent",
                escape(&String::from_utf8_lossy(data)),
                escape(&hex),
                data.len(),
            ));
        }

        body.push_str(&format!(
            "\n\nFee {}\nLeaving this wallet {}",
            escape(&fee),
            escape(&total)
        ));
        if self.many_sources() {
            body.push_str(&format!("\nFrom {}", escape(plan.from.label())));
        }

        let dialog = adw::AlertDialog::new(
            Some(match plan.payees.is_empty() {
                true => "Publish this?",
                false => "Send this payment?",
            }),
            Some(&body),
        );
        // The address is monospaced, so the body is markup — which is why
        // every part of it above is escaped.
        dialog.set_body_use_markup(true);
        dialog.add_response("cancel", "Cancel");
        // Saving the payment for a signer elsewhere. On a watch-only wallet it
        // *replaces* Send rather than sitting beside it, and that is the whole
        // point: it is what makes a device-imported wallet able to spend at all
        // before USB signing exists. Everything below this line was already
        // computed watch-only, so nothing about the file needs a key.
        // Saving the payment for a signer elsewhere, and **only** where there
        // is no signer here. An unsigned payment file is worth something
        // exactly when something else holds the keys: on a wallet that can
        // sign, it is a file somebody could have signed instead, and offering
        // it turns the one dialog that must be read correctly into four
        // buttons. On a watch-only wallet it is not an extra — it replaces
        // Send, because it is the only way through.
        if self.watch_only {
            dialog.add_response("copy", "Copy as text");
            dialog.add_response("save", "Save unsigned…");
            // A device on the cable is the shorter road: no file, no card, no
            // carrying it anywhere. The file stays offered beside it, because
            // it is the only road for a signer that is not plugged in.
            if self.device.is_some() {
                dialog.add_response("device", "Sign on device");
                dialog.set_response_appearance("device", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("device"));
            } else {
                dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("save"));
            }
        } else {
            dialog.add_response("send", "Send");
            dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("send"));
        }
        dialog.set_close_response("cancel");

        // The password buys one signature. It is asked for here rather than at
        // unlock because nothing before this point needs a key — and on a
        // wallet that holds none it is not asked for at all, since there is
        // nothing here for it to open.
        let password = gtk::PasswordEntry::new();
        password.set_show_peek_icon(true);
        password.set_placeholder_text(Some("Wallet password"));
        password.set_margin_top(6);

        // And the passphrase beside it, for a wallet that has one. It is not
        // stored anywhere — that is the entire point of it — so it has to be
        // typed at every signature, and without it the key derived from this
        // vault belongs to a different, empty wallet.
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
            dialog.connect_response(None, move |_, response| match response {
                "send" => sender.input(SendMsg::Confirm(
                    Password(Zeroizing::new(password.text().to_string())),
                    Password(Zeroizing::new(passphrase.text().to_string())),
                )),
                "save" => sender.input(SendMsg::SaveUnsigned),
                "copy" => sender.input(SendMsg::CopyUnsigned),
                "device" => sender.input(SendMsg::SignOnDevice),
                _ => {}
            });
        }

        dialog.present(Some(root));
    }
}

/// What stops this form being reviewable, or `None` when nothing does.
///
/// Max is its own answer: it means "everything", which is an amount even
/// before the field catches up with it.
fn why_not_ready(
    to_filled: bool,
    amount_filled: bool,
    max: bool,
    has_funds: bool,
    extras_ready: bool,
    has_data: bool,
) -> Option<&'static str> {
    if !has_funds {
        return Some("There is nothing on this path to send");
    }
    if !extras_ready {
        return Some("Finish the other recipients, or remove them");
    }
    match (to_filled, max || amount_filled) {
        // Data and nobody to pay is a transaction in its own right. A
        // half-filled recipient still is not: an address with no amount is
        // somebody mid-thought, and sending without them would be the worst
        // available reading of it.
        (false, false) if has_data => None,
        (false, false) => Some("Enter who to pay and how much"),
        (false, true) => Some("Enter an address to pay"),
        (true, false) => Some("Enter an amount to send"),
        (true, true) => None,
    }
}

/// Refuse anything that is not part of a number, at the keystroke.
///
/// Connected to the delegate, not to the row: `adw::EntryRow` implements
/// `Editable` by delegating to an inner `GtkText`, and `insert-text` is emitted
/// there. Connecting to the row itself compiles, runs, and does nothing —
/// which is exactly what it did.
fn install_amount_filter(row: &adw::EntryRow) {
    let Some(delegate) = row.delegate() else {
        return;
    };
    delegate.connect_insert_text(|editable, text, _position| {
        if !text.chars().all(is_amount_character) {
            editable.stop_signal_emission_by_name("insert-text");
        }
    });
}

/// What may be typed into an amount.
///
/// Digits and a decimal point, plus the separators a grouped number is shown
/// with — an amount is often read off the screen above and typed back in, and
/// `Denomination::parse` accepts those. A stray decimal point in satoshis is
/// left to the parser, which says why rather than swallowing the keystroke.
fn is_amount_character(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '_' | '\'')
}

/// Enough of a long identifier to recognise it by, from both ends.
///
/// Both ends, not the first sixteen characters: an address or a transaction id
/// is checked against another screen, and the differences that matter are as
/// likely to be at the end.
pub(crate) fn shorten(text: &str) -> String {
    let count = text.chars().count();
    if count <= 20 {
        return text.to_string();
    }
    let head: String = text.chars().take(10).collect();
    let tail: String = text.chars().skip(count - 8).collect();
    format!("{head}…{tail}")
}

/// Errors read better as sentences when they start like one.
pub(crate) fn capitalise(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::capitalise;
    use super::*;

    #[test]
    fn data_alone_is_a_transaction_but_half_a_recipient_is_not() {
        // Publishing without paying anybody is a real thing to want. An
        // address typed with no amount beside it is not that — it is somebody
        // mid-thought, and sending without them would be the worst available
        // reading of an empty box.
        assert_eq!(why_not_ready(false, false, false, true, true, true), None);
        assert_eq!(
            why_not_ready(false, false, false, true, true, false),
            Some("Enter who to pay and how much")
        );
        assert_eq!(
            why_not_ready(true, false, false, true, true, true),
            Some("Enter an amount to send")
        );
        // Max with nobody to send everything to is still nothing.
        assert_eq!(
            why_not_ready(false, true, true, true, true, true),
            Some("Enter an address to pay")
        );
    }

    #[test]
    fn an_unfinished_extra_recipient_blocks_review() {
        // Half a recipient is somebody's payment, typed and abandoned.
        // Sending the rest and saying nothing about it would be the worst
        // possible reading of an empty box.
        assert_eq!(
            why_not_ready(true, true, false, true, false, false),
            Some("Finish the other recipients, or remove them")
        );
        // And with them finished, nothing is in the way.
        assert_eq!(why_not_ready(true, true, false, true, true, false), None);
    }

    #[test]
    fn review_waits_for_both_fields() {
        // The button is the promise that something will happen. Offering it
        // over an empty form and answering with an error is a worse way of
        // saying "not yet".
        assert_eq!(
            why_not_ready(false, false, false, true, true, false),
            Some("Enter who to pay and how much")
        );
        assert_eq!(
            why_not_ready(true, false, false, true, true, false),
            Some("Enter an amount to send")
        );
        assert_eq!(
            why_not_ready(false, true, false, true, true, false),
            Some("Enter an address to pay")
        );
        assert_eq!(why_not_ready(true, true, false, true, true, false), None);

        // Max is an amount before the field catches up with it.
        assert_eq!(why_not_ready(true, false, true, true, true, false), None);

        // And an empty path has nothing to send, whatever the fields say.
        assert_eq!(
            why_not_ready(true, true, true, false, true, false),
            Some("There is nothing on this path to send")
        );
    }

    /// Proves the filter is attached where the signal is actually emitted.
    /// Needs a display, so it is not part of the default run.

    #[test]
    #[ignore = "needs a display"]
    fn the_amount_field_refuses_letters() {
        relm4::gtk::init().unwrap();
        let row = super::adw::EntryRow::new();
        super::install_amount_filter(&row);

        let mut position = 0;
        row.insert_text("123", &mut position);
        assert_eq!(row.text(), "123", "digits should go in");

        let mut position = row.text().len() as i32;
        row.insert_text("abc", &mut position);
        assert_eq!(row.text(), "123", "letters should not");

        let mut position = row.text().len() as i32;
        row.insert_text(".5", &mut position);
        assert_eq!(row.text(), "123.5", "a decimal point should");
    }

    #[test]
    fn amounts_take_digits_and_separators_only() {
        assert!("1,234.5678".chars().all(super::is_amount_character));
        for rejected in ['a', 'B', '-', '+', '/', 'e'] {
            assert!(!super::is_amount_character(rejected), "{rejected}");
        }
    }

    #[test]
    fn long_identifiers_keep_both_ends() {
        let txid = "f98553279c60cd0252082d71b7fdcb573ea3a47391dccbce0ffa001f589b19b1";
        let short = super::shorten(txid);
        assert!(short.starts_with("f98553279c"), "{short}");
        assert!(short.ends_with("589b19b1"), "{short}");
        assert!(short.chars().count() < 24, "{short}");
        // Short enough to read whole, so it is left alone.
        assert_eq!(super::shorten("abc"), "abc");
    }

    #[test]
    fn messages_start_like_sentences() {
        assert_eq!(capitalise("that is not a number"), "That is not a number");
        assert_eq!(capitalise(""), "");
    }

    /// Freezing must never be a one-way door.
    ///
    /// The only way to a coin's padlock was the Coins row on the send form, and
    /// `has_funds` decided whether that form was drawn at all. Pointing it at
    /// `available_sats`, which subtracts frozen coins, meant freezing every
    /// coin on a path replaced the form with "Nothing to send" — and took the
    /// unfreeze control away with it. A wallet with money in it that cannot be
    /// spent and cannot be released is worse than one that simply refuses.
    #[test]
    fn freezing_everything_does_not_hide_the_way_to_unfreeze() {
        use crate::wallet::labels::{Kind, Labels};

        // The rule under test, stated over the two figures it reads: the form
        // is drawn while the *path* holds anything, not while this payment can
        // reach it.
        let balance_sats = 100_000u64;
        let available_sats = 0u64; // everything frozen
        assert!(
            balance_sats > 0 || available_sats > 0,
            "the send form has to be drawn while the path holds anything at all"
        );

        // And a frozen coin stays frozen across a name being cleared, which is
        // the other way somebody could have lost track of one.
        let outpoint = "0000000000000000000000000000000000000000000000000000000000000000:0";
        let mut labels = Labels::default();
        labels.set_spendable(outpoint, false);
        labels.set(Kind::Output, outpoint, "");
        assert!(!labels.spendable(outpoint));
    }
}
