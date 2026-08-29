//! Root component: the application window and top-level screen switching.
//!
//! The window content is a bare `gtk::Stack`; each screen supplies its own
//! `adw::ToolbarView` and header bar, because onboarding needs a Back button
//! that the other screens must not show.

use std::sync::Arc;

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::ui::onboarding::{Onboarding, OnboardingOutput};
use crate::ui::unlock::{Unlock, UnlockOutput};
use crate::ui::wallet_page::{WalletPage, WalletPageMsg};
use crate::wallet::node::{Progress, Session};
use crate::wallet::{Paths, Summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Onboarding,
    Locked,
    Unlocked,
}

impl Screen {
    fn page(self) -> &'static str {
        match self {
            Screen::Onboarding => "onboarding",
            Screen::Locked => "unlock",
            Screen::Unlocked => "wallet",
        }
    }
}

pub struct App {
    screen: Screen,
    paths: Paths,
    session: Option<Arc<Session>>,
    onboarding: Controller<Onboarding>,
    unlock: Controller<Unlock>,
    wallet: Controller<WalletPage>,
}

#[derive(Debug)]
pub enum AppMsg {
    /// A wallet now exists on disk, or an existing one was unlocked. Both
    /// arrive with a watch-only summary and nothing secret.
    Ready(Summary),
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

                add_named: (model.onboarding.widget(), Some("onboarding")),
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
        let paths = Paths::discover();

        // First run is decided by the vault, not the database: the database can
        // be rebuilt from the seed, so only a missing vault means "no wallet".
        let screen = if paths.is_initialised() { Screen::Locked } else { Screen::Onboarding };
        tracing::debug!(?screen, vault = %paths.vault.display(), "starting");

        let onboarding = Onboarding::builder().launch(paths.clone()).forward(
            sender.input_sender(),
            |out| match out {
                OnboardingOutput::Created(summary) => AppMsg::Ready(summary),
            },
        );
        let unlock = Unlock::builder()
            .launch(paths.clone())
            .forward(sender.input_sender(), |out| match out {
                UnlockOutput::Unlocked(summary) => AppMsg::Ready(summary),
            });
        let wallet = WalletPage::builder().launch(()).detach();

        // `ColorScheme::Default` follows the desktop setting, which is what we
        // want — GNOME owns this preference, not the app. It is never overridden;
        // we only listen so custom-drawn content can repaint.
        let style = adw::StyleManager::default();
        tracing::debug!(dark = style.is_dark(), "following the system color scheme");
        style.connect_dark_notify({
            let sender = sender.clone();
            move |manager| sender.input(AppMsg::ColorSchemeChanged(manager.is_dark()))
        });

        let model = App { screen, paths, session: None, onboarding, unlock, wallet };
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
            AppMsg::Ready(summary) => {
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.screen = Screen::Unlocked;

                // Start the light client once, on first entry to the wallet.
                if self.session.is_none() {
                    let paths = self.paths.clone();
                    sender.oneshot_command(async move {
                        AppCmd::Started(
                            Session::start(&paths).await.map(Arc::new).map_err(|e| e.to_string()),
                        )
                    });
                }
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
}
