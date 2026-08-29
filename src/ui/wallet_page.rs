//! The unlocked wallet.
//!
//! Balance and addresses only, for now — sync, sending and history arrive with
//! M2 onward. Rows are stock `adw::ActionRow`s inside a `PreferencesGroup`.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::{NETWORK, Summary};

pub struct WalletPage {
    summary: Option<Summary>,
}

#[derive(Debug)]
pub enum WalletPageMsg {
    Show(Summary),
    CopyAddress,
}

impl WalletPage {
    fn balance(&self) -> String {
        match &self.summary {
            Some(s) => format!("{} sats", s.balance_sats),
            None => "—".into(),
        }
    }

    fn address(&self) -> String {
        match &self.summary {
            Some(s) => s.next_address.clone(),
            None => "—".into(),
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for WalletPage {
    type Init = ();
    type Input = WalletPageMsg;
    type Output = ();

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Sieve",
                    set_subtitle: &NETWORK.to_string(),
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
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Receive",
                    set_description: Some("Chain sync is not implemented yet, so this balance will not change."),

                    adw::ActionRow {
                        set_title: "Next address",
                        #[watch]
                        set_subtitle: &model.address(),
                        set_subtitle_lines: 2,

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
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = WalletPage { summary: None };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            WalletPageMsg::Show(summary) => self.summary = Some(summary),
            WalletPageMsg::CopyAddress => {
                if let Some(summary) = &self.summary {
                    gtk::gdk::Display::default()
                        .map(|display| display.clipboard().set_text(&summary.next_address));
                }
            }
        }
    }
}
