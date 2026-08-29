//! Root component: the application window and top-level screen switching.
//!
//! The window content is a bare `gtk::Stack`; each screen supplies its own
//! `adw::ToolbarView` and header bar, because onboarding needs a Back button
//! that the other screens must not show.

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::ui::onboarding::{Onboarding, OnboardingOutput};
use crate::ui::unlock::{Unlock, UnlockOutput};
use crate::ui::wallet_page::{WalletPage, WalletPageMsg};
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

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

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
            .launch(paths)
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

        let model = App { screen, onboarding, unlock, wallet };
        let widgets = view_output!();

        // The stack shows its first child until told otherwise, and that child
        // is the onboarding page.
        widgets
            .stack
            .set_visible_child_name(model.screen.page());

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Ready(summary) => {
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.screen = Screen::Unlocked;
            }
            AppMsg::ColorSchemeChanged(dark) => {
                tracing::debug!(dark, "system color scheme changed");
            }
        }
    }
}
