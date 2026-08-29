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
use crate::ui::onboarding::{Onboarding, OnboardingMsg, OnboardingOutput};
use crate::ui::restore::{Restore, RestoreOutput};
use crate::ui::unlock::{Unlock, UnlockMsg, UnlockOutput};
use crate::ui::wallet_page::{WalletPage, WalletPageMsg};
use crate::wallet::node::{Notice, Progress, Session};
use crate::wallet::{self, Paths, Summary};

pub struct App {
    /// The navigation history. Held here so every screen's back button routes
    /// through one place instead of each inventing its own.
    nav: adw::NavigationView,
    /// Preferences, built once and kept: it owns the wallet list as a subpage,
    /// and a widget cannot be re-parented between short-lived dialogs.
    prefs: adw::PreferencesDialog,
    /// The wallet list, wrapped so preferences can slide it in.
    chooser_page: adw::NavigationPage,
    /// The page currently inside the dialog, so reopening replaces it rather
    /// than stacking another copy.
    prefs_page: Option<adw::PreferencesPage>,
    /// Which dialogs are actually on screen. Closing one that was never
    /// presented is a critical, and both get closed on paths that do not know
    /// whether they were opened — unlocking from a fresh import, for one.
    prefs_open: bool,
    unlock_open: bool,
    /// Owned here because preferences is where it is changed.
    settings: crate::settings::Settings,
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

/// Every icon name the interface uses.
///
/// Checked at startup because GTK draws a placeholder for a name it cannot
/// resolve and says nothing about it, so a typo or an icon that is simply not
/// in the theme ships silently. Add to this list when adding an icon.
const ICONS: &[&str] = &[
    "channel-secure-symbolic",
    "document-open-recent-symbolic",
    "document-save-symbolic",
    "edit-copy-symbolic",
    "go-next-symbolic",
    "go-previous-symbolic",
    "list-add-symbolic",
    "network-idle-symbolic",
    "network-offline-symbolic",
    "network-wireless-symbolic",
    "open-menu-symbolic",
    "preferences-system-symbolic",
    "sieve-receive-symbolic",
    "sieve-send-symbolic",
    "view-refresh-symbolic",
    "web-browser-symbolic",
];

#[derive(Debug)]
pub enum AppMsg {
    /// Go back one page. Every screen's back button routes here so the history
    /// lives in one place.
    Back,
    ShowOnboarding,
    ShowRestore,
    ShowPreferences,
    /// Re-read what the header chain can tell us.
    RefreshChain,
    /// A dialog was dismissed, by us or by the person using it.
    PrefsClosed,
    UnlockClosed,
    ToggleDenomination,
    ForgetPeers,
    SetAppearance(crate::settings::Appearance),
    RenameWallet { paths: Paths, name: String },
    SetShowFiat(bool),
    /// Slide the wallet list in over preferences.
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
    Chain(Result<crate::wallet::node::ChainInfo, String>),
    Tick,
    Priced(Result<crate::price::Price, String>),
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
                crate::ui::wallet_page::WalletPageOutput::ShowPreferences => {
                    AppMsg::ShowPreferences
                }
                crate::ui::wallet_page::WalletPageOutput::RefreshChain => AppMsg::RefreshChain,
                crate::ui::wallet_page::WalletPageOutput::Unlock => AppMsg::PromptUnlock,
                crate::ui::wallet_page::WalletPageOutput::NewAddress(script_type) => {
                    AppMsg::RevealAddress(script_type)
                }
            },
        );

        // The desktop owns this preference, not the app. Normally
        // `ColorScheme::Default` follows it on its own — but libadwaita's
        // settings backend does not find the source in every session, and on
        // one where the portal and gsettings both plainly say prefer-dark the
        // app still came up light. So the setting is read directly and
        // mirrored, which is still following the desktop, just by a route that
        // works here. Nothing is chosen by Sieve.
        // Registering a resource is not the same as GTK finding icons in it,
        // and a missing icon draws a placeholder rather than logging anything.
        // Checked here rather than in main: there is no display that early, so
        // anything touching the icon theme there silently does nothing.
        if let Some(display) = gtk::gdk::Display::default() {
            let theme = gtk::IconTheme::for_display(&display);
            // Said explicitly rather than relying on GtkApplication to derive
            // it from the application id. It is one line, and the failure mode
            // of assuming otherwise is a broken picture with nothing in the
            // log.
            theme.add_resource_path("/com/jdavis/Sieve/icons");

            // Every icon the app names, not just its own. A name that is not
            // in the theme draws a placeholder and logs nothing, so the only
            // way to catch a wrong name is to ask.
            for name in ICONS {
                if !theme.has_icon(name) {
                    tracing::error!(name, "icon missing — this will draw a broken picture");
                }
            }
        }

        let style = adw::StyleManager::default();
        let desktop = desktop_interface_settings();
        let settings = crate::settings::Settings::load();

        apply_appearance(&style, settings.appearance);
        if let Some(gio_settings) = &desktop {
            // Keep following it while the choice is to follow.
            gio_settings.connect_changed(Some("color-scheme"), move |_, _| {
                let current = crate::settings::Settings::load();
                apply_appearance(&adw::StyleManager::default(), current.appearance);
            });
        }

        tracing::debug!(
            dark = style.is_dark(),
            desktop = desktop.as_ref().map(|s| s.string("color-scheme").to_string()),
            "following the system color scheme"
        );
        style.connect_dark_notify({
            let sender = sender.clone();
            move |manager| sender.input(AppMsg::ColorSchemeChanged(manager.is_dark()))
        });

        // Pages are registered up front and navigated by tag. The view owns
        // the history, which is what lets back work everywhere without each
        // screen inventing its own.
        unlock_dialog.set_child(Some(unlock.widget()));

        // The wallet list is not part of the main path any more: it lives
        // inside preferences, which is where you go to change wallets.
        let chooser_page = adw::NavigationPage::new(chooser.widget(), "Wallets");
        chooser_page.set_tag(Some("wallets"));

        for (tag, title, child, can_pop) in [
            ("wallet", "Wallet", wallet.widget().clone().upcast::<gtk::Widget>(), true),
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

        wallet.emit(WalletPageMsg::SetDenomination(settings.denomination));

        unlock_dialog.connect_closed({
            let sender = sender.clone();
            move |_| sender.input(AppMsg::UnlockClosed)
        });

        let model = App {
            nav: nav.clone(),
            prefs: {
                let dialog = adw::PreferencesDialog::new();
                // Sized rather than left to its content: a dialog that hugs two
                // rows looks like a mistake, and it has to stay steady when the
                // wallet list slides in over it.
                dialog.set_content_width(480);
                dialog.set_content_height(560);
                dialog
            },
            chooser_page,
            prefs_page: None,
            prefs_open: false,
            unlock_open: false,
            settings,
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

        // The one opened last, if it is still there. Falling back to the
        // first by name is a sort order, not a choice anybody made.
        let opening = model
            .settings
            .last_wallet
            .as_ref()
            .and_then(|id| wallets.iter().find(|w| &w.id == id))
            .or_else(|| wallets.first());

        match opening {
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
                // Setup and import set can_pop false so they can own their
                // back button, which also means pop does not move them. The
                // destination is known anyway: the wallet, when there is one.
                if self.active.is_some() {
                    self.nav.replace_with_tags(&["wallet"]);
                } else {
                    self.nav.pop();
                }
            }
            // Both are reached from the wallet list inside preferences, and
            // both take over the window. Leave the dialog first, or the window
            // visibly changes underneath a dialog that is still up.
            AppMsg::ShowOnboarding => {
                self.close_prefs();
                // Reached from the wallet list, where creating a wallet was
                // already chosen — so start at the first question that has not
                // been answered, and leave a way back out.
                if self.active.is_some() {
                    self.onboarding.emit(OnboardingMsg::EnteredByChoice);
                }
                self.nav.push_by_tag("onboarding");
            }
            AppMsg::ShowRestore => {
                self.close_prefs();
                self.nav.push_by_tag("restore");
            }

            AppMsg::RefreshChain => {
                let Some(session) = self.session.clone() else { return };
                sender.oneshot_command(async move {
                    AppCmd::Chain(session.chain_info().await.map_err(|e| e.to_string()))
                });
            }

            AppMsg::ShowPreferences => {
                self.rebuild_preferences(&sender);
                if let Some(window) = self.nav.root() {
                    if !self.prefs_open {
                        self.prefs.connect_closed({
                            let sender = sender.clone();
                            move |_| sender.input(AppMsg::PrefsClosed)
                        });
                    }
                    self.prefs.present(Some(&window));
                    self.prefs_open = true;
                }
            }

            AppMsg::PrefsClosed => self.prefs_open = false,
            AppMsg::UnlockClosed => self.unlock_open = false,

            AppMsg::ToggleDenomination => {
                self.settings.denomination = self.settings.denomination.toggled();
                self.settings.save();
                self.wallet.emit(WalletPageMsg::SetDenomination(self.settings.denomination));
                self.rebuild_preferences(&sender);
            }

            AppMsg::SetShowFiat(show) => {
                self.settings.show_fiat = show;
                self.settings.save();
                if show {
                    self.fetch_price(&sender);
                } else {
                    self.wallet.emit(WalletPageMsg::SetPrice(None));
                }
            }

            AppMsg::SetAppearance(appearance) => {
                self.settings.appearance = appearance;
                self.settings.save();
                apply_appearance(&adw::StyleManager::default(), appearance);
            }

            AppMsg::RenameWallet { paths, name } => {
                match wallet::Meta::rename(&paths, &name) {
                    Ok(()) => {
                        let id = paths
                            .dir
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let shown = wallet::Meta::load(&paths)
                            .map(|m| m.display_name(&id))
                            .unwrap_or(id);
                        // The header carries the name, and so does the list.
                        self.wallet.emit(WalletPageMsg::SetName(shown));
                        self.chooser.emit(ChooserMsg::Refresh);
                    }
                    Err(e) => tracing::warn!(%e, "could not rename the wallet"),
                }
            }

            AppMsg::ForgetPeers => {
                let network = self
                    .active
                    .as_ref()
                    .and_then(wallet::Meta::load)
                    .map(|m| m.network())
                    .unwrap_or(wallet::DEFAULT_NETWORK);
                crate::peers::clear(network);
                self.rebuild_preferences(&sender);
            }

            AppMsg::ShowWallets => {
                self.chooser.emit(ChooserMsg::Refresh);
                // Slides in over preferences with its own back button, rather
                // than throwing the dialog away to navigate the window behind.
                self.prefs.push_subpage(&self.chooser_page);
            }

            AppMsg::PromptUnlock => {
                if let Some(root) = self.nav.root().and_then(|r| r.root()) {
                    self.unlock_dialog.present(Some(&root));
                    self.unlock_open = true;
                }
            }

            AppMsg::OpenWallet(id) => {
                let paths = Paths::for_wallet(&id);
                let name = wallet::Meta::load(&paths)
                    .map(|m| m.display_name(&id))
                    .unwrap_or_else(|| id.clone());
                // Remembered before the password, so closing at the unlock
                // prompt still returns here next time — that was a choice too.
                self.settings.last_wallet = Some(id.clone());
                self.settings.save();

                self.unlock.emit(UnlockMsg::Open { paths, name: name.clone() });
                self.wallet.emit(WalletPageMsg::SetLocked(true));
                self.wallet.emit(WalletPageMsg::SetName(name));

                // Chosen from the list, so close preferences before asking.
                self.close_prefs();
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

                self.close_unlock();
                self.close_prefs();
                // Whether this came from a password, a new wallet or an
                // import, the wallet is where you end up — and it is the root,
                // so nothing is left behind to walk back through.
                self.nav.replace_with_tags(&["wallet"]);

                self.fetch_price(&sender);

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
                sender.input(AppMsg::RefreshChain);
                self.schedule_tick(&sender);
            }
            AppCmd::Started(Err(message)) => {
                tracing::error!(%message, "could not start the light client");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }
            AppCmd::Chain(Ok(info)) => self.wallet.emit(WalletPageMsg::SetChain(Some(info))),
            AppCmd::Tick => {
                sender.input(AppMsg::RefreshChain);
                self.schedule_tick(&sender);
            }
            AppCmd::Chain(Err(message)) => {
                tracing::warn!(%message, "could not read the chain");
            }
            AppCmd::Update(Ok(summary)) => {
                tracing::debug!(balance = summary.balance_sats, "wallet updated");
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.wallet.emit(WalletPageMsg::SetProgress(Progress::Synced));
                sender.input(AppMsg::RefreshChain);
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
                        // The peer list is a snapshot; without this it shows
                        // whoever was connected when the last sync finished.
                        sender.input(AppMsg::RefreshChain);
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
            AppCmd::Priced(Ok(price)) => {
                self.wallet.emit(WalletPageMsg::SetPrice(Some(price)));
            }
            AppCmd::Priced(Err(message)) => {
                // A missing price is not a wallet problem: the balance in
                // bitcoin is the real number and is already on screen.
                tracing::warn!(%message, "could not fetch a price");
                self.wallet.emit(WalletPageMsg::SetPrice(None));
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

/// The desktop's interface settings, if the schema is installed.
///
/// Returns `None` rather than panicking: `gio::Settings::new` aborts on a
/// missing schema, and a desktop without it is a reason to fall back to
/// libadwaita's own detection, not to refuse to start.
fn desktop_interface_settings() -> Option<gtk::gio::Settings> {
    const SCHEMA: &str = "org.gnome.desktop.interface";
    gtk::gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(SCHEMA, true))
        .map(|_| gtk::gio::Settings::new(SCHEMA))
}

/// Apply the chosen appearance.
///
/// Following the system means reading the desktop's setting directly rather
/// than leaving it to `ColorScheme::Default`, which does not find the source in
/// every session.
fn apply_appearance(style: &adw::StyleManager, appearance: crate::settings::Appearance) {
    use crate::settings::Appearance;

    let scheme = match appearance {
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
        Appearance::System => match desktop_interface_settings()
            .map(|s| s.string("color-scheme").to_string())
            .as_deref()
        {
            Some("prefer-dark") => adw::ColorScheme::PreferDark,
            Some("prefer-light") => adw::ColorScheme::PreferLight,
            // No opinion from the desktop, so none from us either.
            _ => adw::ColorScheme::Default,
        },
    };
    style.set_color_scheme(scheme);
}

impl App {
    /// Ask again shortly.
    ///
    /// Both the things that refresh the chain view go quiet once a wallet is
    /// caught up: next_update parks until a new block, and the node only warns
    /// about connections while it is below its target. Without a tick the
    /// Network tab freezes at whatever was true when the sync finished.
    fn schedule_tick(&self, sender: &ComponentSender<Self>) {
        if self.session.is_none() {
            return;
        }
        sender.oneshot_command(async move {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            AppCmd::Tick
        });
    }

    /// Fetch a price, if the person asked for one and it would mean anything.
    ///
    /// Never on a test network: signet coins have no price, so a number there
    /// would be fiction, and the request would be a disclosure bought for
    /// nothing.
    fn fetch_price(&self, sender: &ComponentSender<Self>) {
        if !self.settings.show_fiat {
            return;
        }
        let is_mainnet = self
            .active
            .as_ref()
            .and_then(wallet::Meta::load)
            .is_some_and(|m| m.network() == bdk_wallet::bitcoin::Network::Bitcoin);
        if !is_mainnet {
            self.wallet.emit(WalletPageMsg::SetPrice(None));
            return;
        }

        sender.spawn_oneshot_command(|| {
            AppCmd::Priced(crate::price::fetch().map_err(|e| e.to_string()))
        });
    }

    /// Close a dialog only if it is on screen. Closing one that was never
    /// presented is an Adwaita critical, and several paths here run whether or
    /// not the dialog was ever opened.
    fn close_prefs(&mut self) {
        if self.prefs_open {
            self.prefs.close();
            self.prefs_open = false;
        }
    }

    fn close_unlock(&mut self) {
        if self.unlock_open {
            self.unlock_dialog.close();
            self.unlock_open = false;
        }
    }

    /// Fill the preferences dialog with the current state.
    ///
    /// The dialog itself is long-lived — it owns the wallet-list subpage, and a
    /// widget cannot be re-parented between short-lived dialogs — so its page
    /// is replaced rather than the dialog rebuilt.
    fn rebuild_preferences(&mut self, sender: &ComponentSender<Self>) {
        let page = adw::PreferencesPage::new();
        page.set_title("Preferences");

        // First, because it is the one thing here you might have opened
        // preferences to do, and everything else describes a wallet you would
        // be leaving anyway.
        let switch = adw::ActionRow::new();
        switch.set_title("Switch wallet");
        switch.set_subtitle("Open a different wallet, or make another");
        switch.set_activatable(true);
        switch.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let sender = sender.clone();
            switch.connect_activated(move |_| sender.input(AppMsg::ShowWallets));
        }

        let leaving = adw::PreferencesGroup::new();
        leaving.add(&switch);
        page.add(&leaving);

        let display = adw::PreferencesGroup::new();
        display.set_title("Display");

        let amounts = adw::ActionRow::new();
        amounts.set_title("Amounts");
        amounts.set_activatable(true);
        amounts.set_subtitle(match self.settings.denomination {
            crate::settings::Denomination::Sats => "Satoshis",
            crate::settings::Denomination::Btc => "Decimal BTC",
        });
        // Preferences has no summary to hand, so the unit is shown for the
        // network the open wallet is on.
        let network = self
            .active
            .as_ref()
            .and_then(wallet::Meta::load)
            .map(|m| m.network)
            .unwrap_or_else(|| "bitcoin".into());
        let unit = gtk::Label::new(Some(self.settings.denomination.label(&network)));
        unit.add_css_class("dim-label");
        amounts.add_suffix(&unit);
        {
            let sender = sender.clone();
            amounts.connect_activated(move |_| sender.input(AppMsg::ToggleDenomination));
        }
        display.add(&amounts);

        let fiat = adw::SwitchRow::new();
        fiat.set_title("Show value in dollars");
        fiat.set_subtitle(
            "Fetches a price from Bitfinex. This is the only connection Sieve makes that is \
             not to the Bitcoin network. It sends no wallet data, but it does reveal your IP \
             address and when you opened the wallet.",
        );
        fiat.set_active(self.settings.show_fiat);
        {
            let sender = sender.clone();
            fiat.connect_active_notify(move |row| {
                sender.input(AppMsg::SetShowFiat(row.is_active()));
            });
        }
        display.add(&fiat);

        // Sieve never sets the colour scheme — it follows the desktop. This
        // row says what it is currently reading, which is the only way to tell
        // "the app ignores my theme" apart from "the desktop is not telling
        // it", and those have very different fixes.
        let appearance = adw::ComboRow::new();
        appearance.set_title("Appearance");
        appearance.set_model(Some(&gtk::StringList::new(
            &crate::settings::Appearance::ALL.map(|a| a.label()),
        )));
        appearance.set_selected(
            crate::settings::Appearance::ALL
                .iter()
                .position(|a| *a == self.settings.appearance)
                .unwrap_or(0) as u32,
        );
        // Connected after the initial selection, so setting it does not fire.
        {
            let sender = sender.clone();
            appearance.connect_selected_notify(move |row| {
                if let Some(choice) =
                    crate::settings::Appearance::ALL.get(row.selected() as usize)
                {
                    sender.input(AppMsg::SetAppearance(*choice));
                }
            });
        }
        display.add(&appearance);

        page.add(&display);

        let this = adw::PreferencesGroup::new();
        this.set_title("This wallet");

        if let Some(paths) = self.active.clone() {
            let id = paths
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let current = wallet::Meta::load(&paths)
                .map(|m| m.display_name(&id))
                .unwrap_or_default();

            let name = adw::EntryRow::new();
            name.set_title("Name");
            name.set_text(&current);
            // Applied when the row is done being edited rather than on every
            // keystroke, so a half-typed name is never what gets saved.
            {
                let sender = sender.clone();
                let paths = paths.clone();
                name.connect_apply(move |row| {
                    sender.input(AppMsg::RenameWallet {
                        paths: paths.clone(),
                        name: row.text().to_string(),
                    });
                });
            }
            name.set_show_apply_button(true);
            this.add(&name);
        }

        if let Some(paths) = &self.active
            && let Some(meta) = wallet::Meta::load(paths)
        {
            let chain = adw::ActionRow::new();
            chain.set_title("Chain");
            chain.set_subtitle(&meta.network);
            this.add(&chain);

            let watched = adw::ActionRow::new();
            watched.set_title("Derivation paths watched");
            watched.set_subtitle(
                &meta
                    .script_types
                    .iter()
                    .map(|s| s.label())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            watched.set_subtitle_lines(2);
            this.add(&watched);
        }

        // Remembering peers is what makes a restart quick; forgetting them is
        // how you start over if the set has gone stale or you would rather not
        // reconnect to the same machines.
        if let Some(paths) = &self.active
            && let Some(meta) = wallet::Meta::load(paths)
        {
            let network = meta.network();
            let known = crate::peers::count(network);

            let peers = adw::ActionRow::new();
            peers.set_title("Remembered peers");
            peers.set_subtitle(&match known {
                0 => "None yet — the next sync will remember some".to_string(),
                1 => "1 peer, tried first on the next start".to_string(),
                n => format!("{n} peers, tried first on the next start"),
            });
            peers.set_subtitle_lines(2);

            let forget = gtk::Button::with_label("Forget");
            forget.set_valign(gtk::Align::Center);
            forget.add_css_class("flat");
            forget.set_sensitive(known > 0);
            {
                let sender = sender.clone();
                forget.connect_clicked(move |_| sender.input(AppMsg::ForgetPeers));
            }
            peers.add_suffix(&forget);
            this.add(&peers);
        }

        page.add(&this);

        // Replace whatever was there, so reopening never stacks pages.
        if let Some(existing) = self.prefs_page.take() {
            self.prefs.remove(&existing);
        }
        self.prefs.add(&page);
        self.prefs_page = Some(page);
    }

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
