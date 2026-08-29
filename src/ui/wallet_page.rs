//! The unlocked wallet: balance, receive address, and sync status.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::wallet::node::Progress;
use crate::wallet::{AccountSummary, Summary};

/// One derivation path's row. A factory rather than a static list, because the
/// number of paths depends on how the wallet was created.
#[derive(Debug)]
pub struct PathRow {
    label: String,
    address: String,
    balance_sats: u64,
}

#[relm4::factory(pub)]
impl FactoryComponent for PathRow {
    type Init = AccountSummary;
    type Input = ();
    type Output = ();
    type CommandOutput = ();
    type ParentWidget = gtk::ListBox;

    view! {
        adw::ActionRow {
            set_title: &self.label,
            set_subtitle: &self.address,
            set_subtitle_lines: 1,

            add_suffix = &gtk::Label {
                add_css_class: "numeric",
                // Dim a path that holds nothing: it was searched and came back
                // empty, which is information, not an error.
                set_css_classes: if self.balance_sats == 0 {
                    &["numeric", "dim-label"]
                } else {
                    &["numeric"]
                },
                set_label: &format!("{} sats", self.balance_sats),
            },
        }
    }

    fn init_model(
        summary: Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        PathRow {
            label: summary.script_type.to_string(),
            address: summary.next_address,
            balance_sats: summary.balance_sats,
        }
    }
}

pub struct WalletPage {
    summary: Option<Summary>,
    progress: Progress,
    peers: Option<(usize, usize)>,
    paths: FactoryVecDeque<PathRow>,
    note: Option<String>,
    error: Option<String>,
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

    fn peers(&self) -> String {
        match self.peers {
            Some((connected, required)) => format!("{connected} of {required} connected"),
            None => "Connecting…".into(),
        }
    }

    fn pending(&self) -> String {
        match &self.summary {
            Some(s) if s.pending_sats > 0 => format!("{} sats", s.pending_sats),
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
                    #[watch]
                    set_subtitle: model.summary.as_ref().map_or("", |s| s.network.as_str()),
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
        let paths = FactoryVecDeque::builder().launch_default().detach();
        let model = WalletPage {
            paths,
            summary: None,
            progress: Progress::Connecting,
            peers: None,
            note: None,
            error: None,
        };
        let paths_box = model.paths.widget();
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            WalletPageMsg::Show(summary) => {
                // Rebuild rather than diff: four rows, and the set only changes
                // when a sync lands.
                let mut guard = self.paths.guard();
                guard.clear();
                if summary.accounts.len() > 1 {
                    for account in &summary.accounts {
                        guard.push_back(account.clone());
                    }
                }
                drop(guard);
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
