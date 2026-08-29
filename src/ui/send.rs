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

#[derive(Debug)]
pub enum SendMsg {
    Show(Box<Summary>),
    SetDenomination(Denomination),
    SetPrice(Option<crate::price::Price>),
    /// The lowest rate the connected peers said they would relay, in sat/vB.
    SetMinFee(Option<f64>),
    SelectFrom(u32),
    ToggleMax(bool),
    /// The amount field was typed in.
    AmountEdited,
    /// Build the transaction and show what it would cost.
    Review,
    Planned(Box<Result<Plan, String>>),
    /// The password is in and the dialog said go.
    Confirm(Password),
    Sent(Box<Result<String, String>>),
    /// Back to an empty form.
    Reset,
}

#[derive(Debug)]
pub enum SendOutput {
    /// Build this, watch-only, and hand the numbers back.
    Plan(Box<Draft>),
    /// Sign and broadcast the plan already reviewed.
    Send { plan: Box<Plan>, password: Password },
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
    error: Option<String>,
    busy: bool,
    /// The reviewed transaction, held between the dialog opening and the
    /// password arriving. Public data — the signature is what needs a secret.
    plan: Option<Plan>,
    sent: Option<String>,
    from_model: gtk::StringList,
    from_labels: Vec<String>,
    /// Kept so the form can be emptied after a payment goes out.
    to_row: Option<adw::EntryRow>,
    amount_row: Option<adw::EntryRow>,
}

impl SendForm {
    /// Paths with something to spend. A path holding nothing cannot be the
    /// source of a payment, and offering it as one is a dead end.
    fn fundable(&self) -> Vec<&AccountSummary> {
        self.accounts.iter().filter(|a| a.balance_sats > 0).collect()
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

    fn available_sats(&self) -> u64 {
        self.source().map_or(0, |a| a.balance_sats)
    }

    fn available(&self) -> String {
        self.settings
            .denomination
            .format(self.available_sats(), &self.network)
    }

    /// The same number without its unit, for a field that will be read back.
    fn available_amount(&self) -> String {
        let shown = self.available();
        shown.rsplit_once(' ').map_or(shown.clone(), |(amount, _)| amount.to_string())
    }

    fn unit(&self) -> &'static str {
        self.settings.denomination.label(&self.network)
    }

    fn has_funds(&self) -> bool {
        self.available_sats() > 0
    }

    /// Shown only when there is a choice to make.
    fn many_sources(&self) -> bool {
        self.fundable().len() > 1
    }

    fn fee_floor(&self) -> f64 {
        self.min_fee.unwrap_or(1.0).max(1.0)
    }

    fn fee_hint(&self) -> String {
        match self.min_fee {
            Some(rate) => format!(
                "Satoshis per virtual byte. Connected peers relay from {rate:.1}."
            ),
            None => "Satoshis per virtual byte".into(),
        }
    }

    /// A dollar figure beside an amount, when the price is known and wanted.
    fn fiat(&self, sats: u64) -> Option<String> {
        let price = self.price.as_ref()?;
        self.settings
            .show_fiat
            .then(|| format!("≈ ${:.2}", price.value_of(sats)))
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
                    self.settings.denomination.format(a.balance_sats, &self.network)
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
                        set_visible: !model.has_funds() && model.sent.is_none(),
                    },

                    // Sent.
                    adw::StatusPage {
                        set_icon_name: Some("channel-secure-symbolic"),
                        set_title: "Payment sent",
                        #[watch]
                        set_visible: model.sent.is_some(),
                        #[watch]
                        set_description: model.sent.as_deref(),

                        #[wrap(Some)]
                        set_child = &gtk::Button {
                            set_label: "Done",
                            set_halign: gtk::Align::Center,
                            add_css_class: "pill",
                            add_css_class: "suggested-action",
                            connect_clicked => SendMsg::Reset,
                        },
                    },

                    // The form.
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 18,
                        #[watch]
                        set_visible: model.has_funds() && model.sent.is_none(),

                        adw::PreferencesGroup {
                            #[watch]
                            set_description: Some(&format!("Available: {}", model.available())),

                            #[name(to_row)]
                            adw::EntryRow {
                                set_title: "Pay to",
                                #[watch]
                                set_sensitive: !model.busy,
                            },

                            #[name(amount_row)]
                            adw::EntryRow {
                                #[watch]
                                set_title: &format!("Amount in {}", model.unit()),
                                #[watch]
                                set_sensitive: !model.busy,

                                // Typing an amount is a way of saying "not
                                // everything", so the field stays editable and
                                // an edit releases Max rather than being
                                // refused by a greyed-out row.
                                connect_changed[sender] => move |_| {
                                    sender.input(SendMsg::AmountEdited);
                                },

                                #[name(max_button)]
                                add_suffix = &gtk::ToggleButton {
                                    set_label: "Max",
                                    set_valign: gtk::Align::Center,
                                    set_tooltip_text: Some(
                                        "Send everything on this path, fee included"
                                    ),
                                    connect_toggled[sender] => move |button| {
                                        sender.input(SendMsg::ToggleMax(button.is_active()));
                                    },
                                },
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
                            #[watch]
                            set_sensitive: !model.busy,
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
            settings: Settings::load(),
            network: "bitcoin".into(),
            accounts: Vec::new(),
            price: None,
            from: None,
            max: false,
            min_fee: None,
            error: None,
            busy: false,
            plan: None,
            sent: None,
            from_model: gtk::StringList::new(&[]),
            from_labels: Vec::new(),
            to_row: None,
            amount_row: None,
        };

        let widgets = view_output!();

        // Held so a finished payment can leave an empty form behind rather
        // than the last one, which is how a payment gets sent twice.
        model.to_row = Some(widgets.to_row.clone());
        model.amount_row = Some(widgets.amount_row.clone());

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
                self.sync_sources();
            }

            SendMsg::SetDenomination(denomination) => {
                self.settings.denomination = denomination;
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

            SendMsg::SelectFrom(index) => {
                self.from = self
                    .fundable()
                    .get(index as usize)
                    .map(|a| a.script_type);
            }

            SendMsg::ToggleMax(max) => {
                self.max = max;
                // Filled in on the way up, and left alone on the way down: the
                // number is a reasonable starting point for editing.
                if max {
                    widgets.amount_row.set_text(&self.available_amount());
                }
            }

            SendMsg::AmountEdited => {
                // Still the whole balance? Still a max send. Anything else and
                // the toggle no longer describes what is in the field.
                if self.max
                    && self.settings.denomination.parse(&widgets.amount_row.text())
                        != Ok(self.available_sats())
                {
                    self.max = false;
                    widgets.max_button.set_active(false);
                }
            }

            SendMsg::Review => {
                self.error = None;
                let network = self.network();

                let to = match crate::wallet::send::parse_address(
                    &widgets.to_row.text(),
                    network,
                ) {
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
                    match self.settings.denomination.parse(&widgets.amount_row.text()) {
                        Ok(sats) => Sending::Exact(
                            bdk_wallet::bitcoin::Amount::from_sat(sats),
                        ),
                        Err(message) => {
                            self.error = Some(message);
                            self.update_view(widgets, sender);
                            return;
                        }
                    }
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
                let _ = sender.output(SendOutput::Plan(Box::new(Draft {
                    from: source,
                    to,
                    amount,
                    fee_rate,
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

            SendMsg::Confirm(password) => {
                let Some(plan) = self.plan.take() else { return };
                self.busy = true;
                let _ = sender.output(SendOutput::Send {
                    plan: Box::new(plan),
                    password,
                });
            }

            SendMsg::Sent(result) => {
                self.busy = false;
                match *result {
                    Ok(txid) => {
                        self.sent = Some(txid);
                        self.error = None;
                    }
                    Err(message) => self.error = Some(capitalise(&message)),
                }
            }

            SendMsg::Reset => {
                self.sent = None;
                self.error = None;
                self.plan = None;
                self.max = false;
                widgets.max_button.set_active(false);
                widgets.to_row.set_text("");
                widgets.amount_row.set_text("");
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
        let amount = unit.format(plan.spend.to_sat(), network);
        let fee = unit.format(plan.fee.to_sat(), network);
        let total = unit.format(plan.total().to_sat(), network);

        let mut body = format!("Send {amount} to\n{}", plan.to);
        if let Some(fiat) = self.fiat(plan.spend.to_sat()) {
            body = format!("Send {amount} ({fiat}) to\n{}", plan.to);
        }
        body.push_str(&format!("\n\nFee {fee}\nLeaving this wallet {total}"));
        if self.many_sources() {
            body.push_str(&format!("\nFrom {}", plan.from.label()));
        }

        let dialog = adw::AlertDialog::new(Some("Send this payment?"), Some(&body));
        dialog.set_body_use_markup(false);
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("send", "Send");
        dialog.set_response_appearance("send", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("send"));
        dialog.set_close_response("cancel");

        // The password buys one signature. It is asked for here rather than at
        // unlock because nothing before this point needs a key.
        let password = gtk::PasswordEntry::new();
        password.set_show_peek_icon(true);
        password.set_placeholder_text(Some("Wallet password"));
        password.set_margin_top(6);
        dialog.set_extra_child(Some(&password));

        {
            let sender = sender.clone();
            let password = password.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "send" {
                    sender.input(SendMsg::Confirm(Password(Zeroizing::new(
                        password.text().to_string(),
                    ))));
                }
            });
        }

        dialog.present(Some(root));
    }
}

/// Errors read better as sentences when they start like one.
fn capitalise(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::capitalise;

    #[test]
    fn messages_start_like_sentences() {
        assert_eq!(capitalise("that is not a number"), "That is not a number");
        assert_eq!(capitalise(""), "");
    }
}
