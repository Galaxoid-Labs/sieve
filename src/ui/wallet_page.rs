//! The unlocked wallet: balance, receive address, and sync status.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::node::Progress;
use crate::wallet::{NETWORK, Summary};

pub struct WalletPage {
    summary: Option<Summary>,
    progress: Progress,
    note: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
pub enum WalletPageMsg {
    Show(Summary),
    SetProgress(Progress),
    /// A non-fatal report from the node — a slow peer, a peer without filters.
    Note(String),
    Failed(String),
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

    fn syncing(&self) -> bool {
        !matches!(self.progress, Progress::Synced)
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
                    set_title: "Network",
                    set_description: Some(
                        "Sieve downloads compact block filters and matches them on this \
                         machine. No server learns which addresses are yours."
                    ),

                    adw::ActionRow {
                        set_title: "Status",
                        #[watch]
                        set_subtitle: &model.progress.label(),

                        add_suffix = &gtk::Spinner {
                            set_valign: gtk::Align::Center,
                            #[watch]
                            set_visible: model.syncing(),
                            #[watch]
                            set_spinning: model.syncing(),
                        },
                    },

                    adw::ActionRow {
                        add_css_class: "dim-label",
                        set_title: "Last report",
                        #[watch]
                        set_visible: model.note.is_some(),
                        #[watch]
                        set_subtitle: model.note.as_deref().unwrap_or_default(),
                        set_subtitle_lines: 2,
                    },

                    // Only meaningful once the node reports a real fraction;
                    // before that the work is unbounded and a bar would lie.
                    gtk::ProgressBar {
                        set_margin_top: 6,
                        set_margin_start: 12,
                        set_margin_end: 12,
                        set_margin_bottom: 6,
                        #[watch]
                        set_visible: model.progress.fraction().is_some() && model.syncing(),
                        #[watch]
                        set_fraction: model.progress.fraction().unwrap_or(0.0),
                    },
                },

                adw::PreferencesGroup {
                    set_title: "Receive",

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
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = WalletPage {
            summary: None,
            progress: Progress::Connecting,
            note: None,
            error: None,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            WalletPageMsg::Show(summary) => self.summary = Some(summary),
            WalletPageMsg::SetProgress(progress) => {
                self.progress = progress;
                self.error = None;
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
