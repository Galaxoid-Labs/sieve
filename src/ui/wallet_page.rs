//! Placeholder for the unlocked wallet view.
//!
//! Once the BDK wallet is wired up this becomes the balance, transaction list,
//! and send/receive flows — built from `adw::PreferencesGroup` and
//! `adw::ActionRow` rather than hand-laid boxes, per the HIG.

use relm4::adw;
use relm4::prelude::*;

pub struct WalletPage;

#[derive(Debug)]
pub enum WalletPageMsg {}

#[relm4::component(pub)]
impl SimpleComponent for WalletPage {
    type Init = ();
    type Input = WalletPageMsg;
    type Output = ();

    view! {
        adw::StatusPage {
            set_icon_name: Some("wallet-symbolic"),
            set_title: "Wallet unlocked",
            set_description: Some("Chain sync and balances are not implemented yet."),
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = WalletPage;
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }
}
