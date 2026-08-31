//! Pick a wallet, or make another one.
//!
//! Shown when more than one wallet exists, and reachable from the wallet page
//! so that creating a second wallet never means deleting the first.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::{self, WalletEntry};

#[derive(Debug)]
pub struct WalletRow {
    id: String,
    name: String,
    network: String,
}

#[derive(Debug)]
pub enum WalletRowOutput {
    Chosen(String),
}

#[relm4::factory(pub)]
impl FactoryComponent for WalletRow {
    type Init = WalletEntry;
    type Input = ();
    type Output = WalletRowOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            set_title: &self.name,
            set_subtitle: &self.network,
            set_activatable: true,

            add_suffix = &gtk::Image {
                set_icon_name: Some("go-next-symbolic"),
            },

            connect_activated[sender, id = self.id.clone()] => move |_| {
                let _ = sender.output(WalletRowOutput::Chosen(id.clone()));
            },
        }
    }

    fn init_model(entry: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        WalletRow {
            id: entry.id,
            name: entry.name,
            network: entry.network,
        }
    }
}

pub struct Chooser {
    wallets: FactoryVecDeque<WalletRow>,
    count: usize,
}

#[derive(Debug)]
pub enum ChooserMsg {
    /// Re-read the wallet list from disk.
    Refresh,
    /// Whether a wallet is already open behind this screen.
    Chosen(String),
    New,
    Import,
}

#[derive(Debug)]
pub enum ChooserOutput {
    Open(String),
    New,
    Import,
}

#[relm4::component(pub)]
impl SimpleComponent for Chooser {
    type Init = ();
    type Input = ChooserMsg;
    type Output = ChooserOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                #[wrap(Some)]
                set_title_widget = &adw::WindowTitle {
                    set_title: "Sieve",
                    set_subtitle: "Choose a wallet",
                },
            },

            #[wrap(Some)]
            set_content = &adw::PreferencesPage {
                adw::PreferencesGroup {
                    set_title: "Wallets",
                    #[watch]
                    set_description: Some(&format!("{} on this computer", model.count)),

                    #[local_ref]
                    wallet_list -> gtk::ListBox {
                        add_css_class: "boxed-list",
                        set_selection_mode: gtk::SelectionMode::None,
                    },
                },

                adw::PreferencesGroup {
                    adw::ActionRow {
                        set_title: "Create a new wallet",
                        set_subtitle: "Generates a new recovery phrase",
                        set_activatable: true,
                        add_prefix = &gtk::Image {
                            set_icon_name: Some("list-add-symbolic"),
                        },
                        connect_activated => ChooserMsg::New,
                    },

                    adw::ActionRow {
                        set_title: "Import an existing wallet",
                        set_subtitle: "Recovery phrase, private key, or descriptor",
                        set_activatable: true,
                        add_prefix = &gtk::Image {
                            set_icon_name: Some("document-save-symbolic"),
                        },
                        connect_activated => ChooserMsg::Import,
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
        let wallets =
            FactoryVecDeque::builder()
                .launch_default()
                .forward(sender.input_sender(), |out| match out {
                    WalletRowOutput::Chosen(id) => ChooserMsg::Chosen(id),
                });

        let mut model = Chooser { wallets, count: 0 };
        model.reload();

        let wallet_list = model.wallets.widget();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ChooserMsg::Refresh => self.reload(),
            ChooserMsg::Chosen(id) => {
                let _ = sender.output(ChooserOutput::Open(id));
            }
            ChooserMsg::New => {
                let _ = sender.output(ChooserOutput::New);
            }
            ChooserMsg::Import => {
                let _ = sender.output(ChooserOutput::Import);
            }
        }
    }
}

impl Chooser {
    fn reload(&mut self) {
        let entries = wallet::list_wallets();
        self.count = entries.len();
        let mut guard = self.wallets.guard();
        guard.clear();
        for entry in entries {
            guard.push_back(entry);
        }
    }
}
