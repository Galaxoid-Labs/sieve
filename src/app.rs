//! Root component: the application window and top-level screen switching.

use std::path::PathBuf;

use adw::prelude::*;
use relm4::prelude::*;
use relm4::{adw, gtk};

use crate::ui::unlock::{Unlock, UnlockOutput};
use crate::ui::wallet_page::WalletPage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Locked,
    Unlocked,
}

impl Screen {
    fn page(self) -> &'static str {
        match self {
            Screen::Locked => "unlock",
            Screen::Unlocked => "wallet",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Screen::Locked => "Locked",
            Screen::Unlocked => "Signet",
        }
    }
}

pub struct App {
    screen: Screen,
    unlock: Controller<Unlock>,
    wallet: Controller<WalletPage>,
}

#[derive(Debug)]
pub enum AppMsg {
    Unlocked,
    /// The desktop switched between light and dark.
    ///
    /// Stock Adwaita widgets and style classes recolour themselves, so nothing
    /// here has to react yet. Anything we draw ourselves does — the receive-page
    /// QR code lands in a `gtk::DrawingArea`, and a QR drawn with hardcoded
    /// black-on-white becomes invisible in dark mode.
    ColorSchemeChanged(bool),
}

/// `$XDG_DATA_HOME/sieve/vault.sieve`.
///
/// Kept out of any cloud-synced directory on purpose; export is an explicit,
/// user-initiated action, never a side effect of where the file lives.
fn vault_path() -> PathBuf {
    directories::ProjectDirs::from("com", "jdavis", "Sieve")
        .map(|dirs| dirs.data_dir().join("vault.sieve"))
        .unwrap_or_else(|| PathBuf::from("vault.sieve"))
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("Sieve"),
            set_default_size: (420, 640),

            #[wrap(Some)]
            set_content = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "Sieve",
                        #[watch]
                        set_subtitle: model.screen.subtitle(),
                    },
                    // TODO: primary menu (About, Preferences, Quit) once the
                    // corresponding actions exist. An empty MenuButton is worse
                    // than none.
                },

                #[wrap(Some)]
                set_content = &gtk::Stack {
                    set_transition_type: gtk::StackTransitionType::Crossfade,
                    // skip_init: the stack has no children yet when init-time
                    // property assignment runs, and it defaults to showing the
                    // first one added, which is already the correct state.
                    #[watch(skip_init)]
                    set_visible_child_name: model.screen.page(),

                    add_named: (model.unlock.widget(), Some("unlock")),
                    add_named: (model.wallet.widget(), Some("wallet")),
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let unlock = Unlock::builder()
            .launch(vault_path())
            .forward(sender.input_sender(), |out| match out {
                UnlockOutput::Unlocked => AppMsg::Unlocked,
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

        let model = App { screen: Screen::Locked, unlock, wallet };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Unlocked => self.screen = Screen::Unlocked,
            AppMsg::ColorSchemeChanged(dark) => {
                tracing::debug!(dark, "system color scheme changed");
            }
        }
    }
}
