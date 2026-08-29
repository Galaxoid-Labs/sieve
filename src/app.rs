//! Root component: the application window and top-level screen switching.
//!
//! The window content is a bare `gtk::Stack`; each screen supplies its own
//! `adw::ToolbarView` and header bar, because onboarding needs a Back button
//! that the other screens must not show.

use std::sync::Arc;

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::ui::chooser::{Chooser, ChooserMsg, ChooserOutput};
use crate::ui::onboarding::{Onboarding, OnboardingOutput};
use crate::ui::restore::{Restore, RestoreOutput};
use crate::ui::unlock::{Unlock, UnlockMsg, UnlockOutput};
use crate::ui::wallet_page::{WalletPage, WalletPageMsg};
use crate::wallet::node::{Notice, Progress, Session};
use crate::wallet::{self, Paths, Summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Chooser,
    Onboarding,
    Restore,
    Locked,
    Unlocked,
}

impl Screen {
    fn page(self) -> &'static str {
        match self {
            Screen::Chooser => "chooser",
            Screen::Onboarding => "onboarding",
            Screen::Restore => "restore",
            Screen::Locked => "unlock",
            Screen::Unlocked => "wallet",
        }
    }
}

pub struct App {
    screen: Screen,
    /// The wallet currently open, if any.
    active: Option<Paths>,
    session: Option<Arc<Session>>,
    chooser: Controller<Chooser>,
    onboarding: Controller<Onboarding>,
    restore: Controller<Restore>,
    unlock: Controller<Unlock>,
    wallet: Controller<WalletPage>,
}

#[derive(Debug)]
pub enum AppMsg {
    ShowRestore,
    ShowWelcome,
    ShowChooser,
    /// Reveal a fresh receive address on one derivation path.
    RevealAddress(crate::wallet::accounts::ScriptType),
    /// Open a specific wallet from the chooser.
    OpenWallet(String),
    /// A wallet now exists on disk, or an existing one was unlocked. Both
    /// arrive with a watch-only summary and nothing secret. The paths say
    /// *which* wallet, which is what decides whether the running light client
    /// still belongs to what is on screen.
    Ready { paths: Paths, summary: Summary },
    /// The desktop switched between light and dark.
    ///
    /// Stock Adwaita widgets and style classes recolour themselves, so nothing
    /// here has to react yet. Anything we draw ourselves does — the receive-page
    /// QR code lands in a `gtk::DrawingArea`, and a QR drawn with hardcoded
    /// black-on-white becomes invisible in dark mode.
    ColorSchemeChanged(bool),
}

#[derive(Debug)]
pub enum AppCmd {
    Started(Result<Arc<Session>, String>),
    Update(Result<Summary, String>),
    /// `None` means the node stopped.
    Progress(Option<Progress>),
    Warning(Option<Notice>),
    Revealed(Result<(String, Summary), String>),
}

#[relm4::component(pub)]
impl Component for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = AppCmd;

    view! {
        adw::ApplicationWindow {
            set_title: Some("Sieve"),
            set_default_size: (460, 680),

            #[wrap(Some)]
            #[name(stack)]
            set_content = &gtk::Stack {
                // named so `init` can select the right screen after the children exist
                set_transition_type: gtk::StackTransitionType::Crossfade,
                // skip_init: the stack has no children yet when init-time
                // property assignment runs, and it defaults to showing the
                // first one added.
                #[watch(skip_init)]
                set_visible_child_name: model.screen.page(),

                add_named: (model.chooser.widget(), Some("chooser")),
                add_named: (model.onboarding.widget(), Some("onboarding")),
                add_named: (model.restore.widget(), Some("restore")),
                add_named: (model.unlock.widget(), Some("unlock")),
                add_named: (model.wallet.widget(), Some("wallet")),
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Sieve used to keep a single wallet in the data root. Move it before
        // listing, or an existing wallet would silently disappear.
        wallet::migrate_legacy_layout();

        let wallets = wallet::list_wallets();
        let screen = match wallets.len() {
            0 => Screen::Onboarding,
            // One wallet needs no chooser; go straight to its unlock screen.
            1 => Screen::Locked,
            _ => Screen::Chooser,
        };
        tracing::debug!(?screen, wallets = wallets.len(), "starting");

        let chooser = Chooser::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                ChooserOutput::Open(id) => AppMsg::OpenWallet(id),
                ChooserOutput::New => AppMsg::ShowWelcome,
                ChooserOutput::Import => AppMsg::ShowRestore,
            },
        );
        let onboarding = Onboarding::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                OnboardingOutput::Created { paths, summary } => AppMsg::Ready { paths, summary },
                OnboardingOutput::WantsRestore => AppMsg::ShowRestore,
            },
        );
        let restore = Restore::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                RestoreOutput::Imported { paths, summary } => AppMsg::Ready { paths, summary },
                RestoreOutput::Cancelled => AppMsg::ShowWelcome,
            },
        );
        let unlock = Unlock::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                UnlockOutput::Unlocked { paths, summary } => AppMsg::Ready { paths, summary },
                UnlockOutput::SwitchWallet => AppMsg::ShowChooser,
            });
        let wallet = WalletPage::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                crate::ui::wallet_page::WalletPageOutput::SwitchWallet => AppMsg::ShowChooser,
                crate::ui::wallet_page::WalletPageOutput::NewAddress(script_type) => {
                    AppMsg::RevealAddress(script_type)
                }
            },
        );

        // `ColorScheme::Default` follows the desktop setting, which is what we
        // want — GNOME owns this preference, not the app. It is never overridden;
        // we only listen so custom-drawn content can repaint.
        let style = adw::StyleManager::default();
        tracing::debug!(dark = style.is_dark(), "following the system color scheme");
        style.connect_dark_notify({
            let sender = sender.clone();
            move |manager| sender.input(AppMsg::ColorSchemeChanged(manager.is_dark()))
        });

        let active = wallets.first().map(|w| Paths::for_wallet(&w.id));
        if let (Screen::Locked, Some(entry)) = (screen, wallets.first()) {
            unlock.emit(UnlockMsg::Open {
                paths: Paths::for_wallet(&entry.id),
                name: entry.name.clone(),
            });
        }

        let model = App {
            screen, active, session: None, chooser, onboarding, restore, unlock, wallet,
        };
        let widgets = view_output!();

        // The stack shows its first child until told otherwise, and that child
        // is the onboarding page.
        widgets
            .stack
            .set_visible_child_name(model.screen.page());

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            AppMsg::Ready { paths, summary } => {
                // Opening a different wallet must retire the running client.
                // Otherwise the previous wallet's node keeps feeding this
                // screen, and a freshly imported wallet shows the old one's
                // sync state — including a reassuring "Up to date" it has not
                // earned.
                let switched = self.active.as_ref().map(|p| &p.dir) != Some(&paths.dir);
                if switched && let Some(session) = self.session.take() {
                    tracing::info!("switching wallets; stopping the previous light client");
                    session.shutdown();
                    self.wallet.emit(WalletPageMsg::Reset);
                }

                self.active = Some(paths.clone());
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.screen = Screen::Unlocked;

                if self.session.is_none() {
                    sender.oneshot_command(async move {
                        AppCmd::Started(
                            Session::start(&paths).await.map(Arc::new).map_err(|e| e.to_string()),
                        )
                    });
                }
            }
            AppMsg::ShowRestore => self.screen = Screen::Restore,
            AppMsg::ShowWelcome => self.screen = Screen::Onboarding,
            AppMsg::RevealAddress(script_type) => {
                let Some(session) = self.session.clone() else {
                    // Nothing to reveal from until the client is up.
                    return;
                };
                sender.oneshot_command(async move {
                    AppCmd::Revealed(
                        session.reveal_next(script_type).await.map_err(|e| e.to_string()),
                    )
                });
            }
            AppMsg::ShowChooser => {
                // Re-read from disk: a wallet may have been created since this
                // screen was last shown.
                self.chooser.emit(ChooserMsg::Refresh);
                self.screen = Screen::Chooser;
            }
            AppMsg::OpenWallet(id) => {
                let paths = Paths::for_wallet(&id);
                let name = wallet::Meta::load(&paths)
                    .map(|m| m.display_name(&id))
                    .unwrap_or_else(|| id.clone());
                self.unlock.emit(UnlockMsg::Open { paths: paths.clone(), name });
                self.active = Some(paths);
                self.screen = Screen::Locked;
            }
            AppMsg::ColorSchemeChanged(dark) => {
                tracing::debug!(dark, "system color scheme changed");
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: Self::CommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            AppCmd::Started(Ok(session)) => {
                tracing::info!("light client started");
                self.session = Some(session);
                // Two independent loops: one awaits wallet updates, the other
                // progress events. Each re-arms itself, which is how relm4
                // models a stream.
                self.await_update(&sender);
                self.await_progress(&sender);
                self.await_warning(&sender);
            }
            AppCmd::Started(Err(message)) => {
                tracing::error!(%message, "could not start the light client");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }
            AppCmd::Update(Ok(summary)) => {
                tracing::debug!(balance = summary.balance_sats, "wallet updated");
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.wallet.emit(WalletPageMsg::SetProgress(Progress::Synced));
                self.await_update(&sender);
            }
            AppCmd::Update(Err(message)) => {
                // Do not re-arm: the loop would spin on a persistent failure.
                tracing::error!(%message, "sync failed");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }
            AppCmd::Progress(Some(progress)) => {
                self.wallet.emit(WalletPageMsg::SetProgress(progress));
                self.await_progress(&sender);
            }
            AppCmd::Progress(None) => tracing::warn!("the node stopped emitting progress"),
            AppCmd::Warning(Some(notice)) => {
                match notice {
                    Notice::Peers { connected, required } => {
                        self.wallet.emit(WalletPageMsg::Peers { connected, required });
                    }
                    Notice::Problem(message) => self.wallet.emit(WalletPageMsg::Note(message)),
                    Notice::Ignorable => {}
                }
                self.await_warning(&sender);
            }
            AppCmd::Warning(None) => tracing::warn!("the node stopped emitting warnings"),
            AppCmd::Revealed(Ok((address, summary))) => {
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.wallet.emit(WalletPageMsg::ShowFreshAddress(address));
            }
            AppCmd::Revealed(Err(message)) => {
                tracing::error!(%message, "could not reveal an address");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }
        }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        if let Some(session) = &self.session {
            session.shutdown();
        }
    }
}

impl App {
    fn await_update(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else { return };
        sender.oneshot_command(async move {
            AppCmd::Update(session.next_update().await.map_err(|e| e.to_string()))
        });
    }

    fn await_progress(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else { return };
        sender.oneshot_command(async move { AppCmd::Progress(session.next_progress().await) });
    }

    fn await_warning(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else { return };
        sender.oneshot_command(async move { AppCmd::Warning(session.next_warning().await) });
    }
}
