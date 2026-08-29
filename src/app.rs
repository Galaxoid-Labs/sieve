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

pub struct App {
    /// The navigation history. Held here so every screen's back button routes
    /// through one place instead of each inventing its own.
    nav: adw::NavigationView,
    /// Unlocking is a dialog over the wallet, not a page you walk through.
    /// Landing straight on the wallet is the whole point: the thing you opened
    /// the app for is on screen from the first frame, just not filled in yet.
    unlock_dialog: adw::Dialog,
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
    /// Go back one page. Every screen's back button routes here so the history
    /// lives in one place.
    Back,
    ShowOnboarding,
    ShowRestore,
    /// Open the wallet list — a detour, not part of the main path.
    ShowWallets,
    /// Re-present the password dialog for the wallet already on screen.
    PromptUnlock,
    /// Open a specific wallet from the list.
    OpenWallet(String),
    /// A wallet now exists on disk, or an existing one was unlocked. Both
    /// arrive with a watch-only summary and nothing secret. The paths say
    /// *which* wallet, which is what decides whether the running light client
    /// still belongs to what is on screen.
    Ready { paths: Paths, summary: Summary },
    /// Reveal a fresh receive address on one derivation path.
    RevealAddress(crate::wallet::accounts::ScriptType),
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
            // Wide enough to start on the header switcher and give the
            // activity list room, tall enough for a preferences page.
            set_default_size: (820, 760),

            set_content: Some(&nav),
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
        tracing::debug!(wallets = wallets.len(), "starting");

        let nav = adw::NavigationView::new();
        let unlock_dialog = adw::Dialog::new();
        unlock_dialog.set_title("Unlock");
        unlock_dialog.set_content_width(400);
        // No fixed height: the dialog sizes to its content, so the button can
        // never end up below the fold.

        let chooser = Chooser::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                ChooserOutput::Open(id) => AppMsg::OpenWallet(id),
                ChooserOutput::New => AppMsg::ShowOnboarding,
                ChooserOutput::Import => AppMsg::ShowRestore,
                ChooserOutput::Back => AppMsg::Back,
            },
        );
        let onboarding = Onboarding::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                OnboardingOutput::Created { paths, summary } => AppMsg::Ready { paths, summary },
                OnboardingOutput::WantsRestore => AppMsg::ShowRestore,
                OnboardingOutput::Cancelled => AppMsg::Back,
            },
        );
        let restore = Restore::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                RestoreOutput::Imported { paths, summary } => AppMsg::Ready { paths, summary },
                RestoreOutput::Cancelled => AppMsg::Back,
            },
        );
        let unlock = Unlock::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                UnlockOutput::Unlocked { paths, summary } => AppMsg::Ready { paths, summary },
                UnlockOutput::SwitchWallet => AppMsg::Back,
            });
        let wallet = WalletPage::builder().launch(()).forward(
            sender.input_sender(),
            |out| match out {
                crate::ui::wallet_page::WalletPageOutput::SwitchWallet => AppMsg::ShowWallets,
                crate::ui::wallet_page::WalletPageOutput::Unlock => AppMsg::PromptUnlock,
                crate::ui::wallet_page::WalletPageOutput::NewAddress(script_type) => {
                    AppMsg::RevealAddress(script_type)
                }
            },
        );

        // `ColorScheme::Default` follows the desktop setting, which is what we
        // want — GNOME owns this preference, not the app. It is never
        // overridden; we only listen so custom-drawn content can repaint.
        let style = adw::StyleManager::default();
        tracing::debug!(dark = style.is_dark(), "following the system color scheme");
        style.connect_dark_notify({
            let sender = sender.clone();
            move |manager| sender.input(AppMsg::ColorSchemeChanged(manager.is_dark()))
        });

        // Pages are registered up front and navigated by tag. The view owns
        // the history, which is what lets back work everywhere without each
        // screen inventing its own.
        unlock_dialog.set_child(Some(unlock.widget()));

        for (tag, title, child, can_pop) in [
            ("wallet", "Wallet", wallet.widget().clone().upcast::<gtk::Widget>(), true),
            ("chooser", "Wallets", chooser.widget().clone().upcast(), true),
            // Setup and import drive their own back button: theirs steps
            // backwards through a flow before leaving it, and two back buttons
            // in one header is worse than one that does both jobs.
            ("onboarding", "New wallet", onboarding.widget().clone().upcast(), false),
            ("restore", "Import", restore.widget().clone().upcast(), false),
        ] {
            let page = adw::NavigationPage::new(&child, title);
            page.set_tag(Some(tag));
            page.set_can_pop(can_pop);
            nav.add(&page);
        }

        let model = App {
            nav: nav.clone(),
            unlock_dialog: unlock_dialog.clone(),
            active: None,
            session: None,
            chooser,
            onboarding,
            restore,
            unlock,
            wallet,
        };
        let widgets = view_output!();

        match wallets.first() {
            // Nothing to open, so setup is the whole app.
            None => model.nav.replace_with_tags(&["onboarding"]),
            // The wallet is the root. Opening the app puts you on it
            // immediately, locked, with the password asked for on top.
            Some(entry) => {
                model.nav.replace_with_tags(&["wallet"]);
                sender.input(AppMsg::OpenWallet(entry.id.clone()));
            }
        }

        ComponentParts { model, widgets }
    }

    fn update(
        &mut self,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            AppMsg::Back => {
                self.nav.pop();
            }
            AppMsg::ShowOnboarding => self.nav.push_by_tag("onboarding"),
            AppMsg::ShowRestore => self.nav.push_by_tag("restore"),

            AppMsg::ShowWallets => {
                self.chooser.emit(ChooserMsg::Refresh);
                self.nav.push_by_tag("chooser");
            }

            AppMsg::PromptUnlock => {
                if let Some(root) = self.nav.root().and_then(|r| r.root()) {
                    self.unlock_dialog.present(Some(&root));
                }
            }

            AppMsg::OpenWallet(id) => {
                let paths = Paths::for_wallet(&id);
                let name = wallet::Meta::load(&paths)
                    .map(|m| m.display_name(&id))
                    .unwrap_or_else(|| id.clone());
                self.unlock.emit(UnlockMsg::Open { paths, name: name.clone() });
                self.wallet.emit(WalletPageMsg::SetLocked(true));
                self.wallet.emit(WalletPageMsg::SetName(name));

                // Chosen from the list, so leave the detour before asking.
                self.nav.pop_to_tag("wallet");
                sender.input(AppMsg::PromptUnlock);
            }

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
                self.wallet.emit(WalletPageMsg::SetLocked(false));
                self.chooser.emit(ChooserMsg::Refresh);

                self.unlock_dialog.close();
                // Whether this came from a password, a new wallet or an
                // import, the wallet is where you end up — and it is the root,
                // so nothing is left behind to walk back through.
                self.nav.replace_with_tags(&["wallet"]);

                if self.session.is_none() {
                    sender.oneshot_command(async move {
                        AppCmd::Started(
                            Session::start(&paths).await.map(Arc::new).map_err(|e| e.to_string()),
                        )
                    });
                }
            }

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
