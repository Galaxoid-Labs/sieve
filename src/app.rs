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
use crate::ui::reveal::{Reveal, RevealMsg};
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
    /// Showing the recovery phrase again, likewise a subpage of preferences.
    reveal_page: adw::NavigationPage,
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
    /// What the open wallet holds, so the warning before deleting it can say
    /// so. Cleared with everything else on a switch.
    balance_sats: Option<u64>,
    /// Which session the results arriving now belong to.
    ///
    /// A command started against one wallet can land after another has been
    /// opened — trivially, over Tor, where reading the chain takes a dozen
    /// round trips through circuits. Without this, a signet chain, a signet
    /// peer list, or worse a signet *balance* lands on a mainnet wallet.
    generation: u64,
    /// When scan progress was last written down.
    scan_recorded: Option<std::time::Instant>,
    /// The last locally computed fee estimate: height, sat/vB, and where it
    /// came from. Kept so switching to Send twice does not download the same
    /// block twice.
    fee_estimate: Option<(u32, f64, String)>,
    /// The height the chain view last reported, which is what makes the
    /// estimate stale.
    chain_tip: Option<u32>,
    /// When the peer list was last read, so a stream of connection warnings
    /// cannot turn into a stream of widget rebuilds.
    peers_read: Option<std::time::Instant>,
    /// The Amounts row and its unit label, kept so toggling the unit does not
    /// rebuild the page.
    amounts_row: Option<(adw::ActionRow, gtk::Label)>,
    /// What the last proxy check found, shown under the Tor switch.
    tor_status: Option<String>,
    /// Whether that status is a failure, so it can be coloured like one.
    tor_failed: bool,
    /// Set when someone has just asked for Tor, so a failure can put their
    /// switch back rather than leaving a wallet that will not connect.
    tor_asked_for: bool,
    /// The status row and the switch, kept so bootstrap progress can be
    /// written into them directly. Rebuilding the whole preferences page for
    /// each percentage threw the reader back to the top of it.
    tor_row: Option<adw::ActionRow>,
    tor_switch: Option<(adw::SwitchRow, gtk::glib::SignalHandlerId)>,
    /// The proxy currently in use — the one Sieve found running, or the one it
    /// started. Nothing connects through Tor until this is set.
    tor_active: Option<crate::tor::Proxy>,
    session: Option<Arc<Session>>,
    chooser: Controller<Chooser>,
    onboarding: Controller<Onboarding>,
    // Never read, and must not be dropped: a Relm4 controller shuts its
    // component down when it goes, so this field is what keeps the restore
    // screen alive between visits.
    #[allow(dead_code)]
    restore: Controller<Restore>,
    /// Whether this session's block count has been written down yet. Once per
    /// session: later updates are new tips arriving, not this scan.
    blocks_recorded: bool,
    /// Dropping this unsubscribes from logind, so it is held for the life of
    /// the app rather than let go at the end of `init`.
    sleep_watch: Option<gtk::gio::SignalSubscription>,
    /// The same for the desktop theme: dropping the monitor stops the watch,
    /// and the accent would then be whatever it was at startup. Never read for
    /// that reason — being held *is* what it does.
    #[allow(dead_code)]
    theme_watch: Option<gtk::gio::FileMonitor>,
    /// Where the desktop's palette is written, so it can be reapplied when the
    /// colour scheme changes under it.
    accent_provider: Option<gtk::CssProvider>,
    /// The payment a replacement is replacing, taken from the plan at the
    /// moment of signing and cleared when the broadcast lands.
    ///
    /// Set from `Plan::replaces` rather than when the bump was asked for:
    /// asking is not doing. A dialog opened and cancelled used to leave this
    /// set, and the next ordinary payment inherited the old one's label, its
    /// "fee raised" toast, and its navigation.
    bumping: Option<String>,
    /// When somebody last touched this window. What the idle lock counts from.
    ///
    /// A plain `Instant` rather than a timer that gets cancelled and rebuilt on
    /// every keystroke: input is continuous and timers are not free.
    last_seen: std::time::Instant,
    /// How many events have reached the window. Only for working out whether
    /// an idle wallet is genuinely idle.
    stirs: u64,
    unlock: Controller<Unlock>,
    wallet: Controller<WalletPage>,
    reveal: Controller<Reveal>,
    /// Whether the open wallet's password has been given this session. Not the
    /// same as "a wallet is open": the wallet screen is on display, with its
    /// balance, before anyone has typed anything.
    unlocked: bool,
}

/// Every icon name the interface uses.
///
/// Checked at startup because GTK draws a placeholder for a name it cannot
/// resolve and says nothing about it, so a typo or an icon that is simply not
/// in the theme ships silently. Add to this list when adding an icon.
/// How often the peer list may be re-read while connections are churning.
const PEER_REFRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// How often to ask whether the wallet has been left alone. Coarse on purpose:
/// see `watch_for_idle`.
const IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(15);

/// Hand the desktop's accent to libadwaita, or take ours back out of the way.
///
/// Everything else — `accent_color`, `theme_selected_bg_color`, focus rings —
/// libadwaita defines in terms of `@accent_bg_color`, so one colour is the
/// whole change.
fn apply_palette(provider: &gtk::CssProvider) {
    // Replacing a theme moves several files, and the monitor reports each of
    // them: one theme change arrived as five. Applying the same colour five
    // times is harmless and reading it five times in a log is not, so the last
    // one applied is remembered and an unchanged palette does nothing.
    #[allow(clippy::type_complexity)]
    thread_local! {
        static APPLIED: std::cell::RefCell<Option<(Option<crate::palette::Palette>, bool)>> =
            const { std::cell::RefCell::new(None) };
    }

    // The scheme actually in force, which is the Appearance preference rather
    // than the desktop theme's own mode. Part of the key, so choosing Light
    // under a dark desktop theme re-applies rather than leaving that theme's
    // surfaces in place.
    let dark = adw::StyleManager::default().is_dark();
    let found = crate::palette::desktop();
    let changed = APPLIED.with(|applied| {
        let mut applied = applied.borrow_mut();
        if *applied == Some((found.clone(), dark)) {
            return false;
        }
        *applied = Some((found.clone(), dark));
        true
    });
    if !changed {
        return;
    }

    match found {
        Some(palette) => {
            tracing::debug!(
                accent = %palette.accent,
                surfaces = palette.surfaces.as_ref().is_some_and(|s| s.dark == dark),
                "following the desktop's palette"
            );
            provider.load_from_string(&palette.css(dark));
        }
        // Nothing published, or it stopped being readable: back to whatever
        // libadwaita would have done on its own.
        None => {
            tracing::debug!("no desktop palette; using libadwaita's own accent");
            provider.load_from_string("");
        }
    }
}

/// Lock when the computer goes to sleep.
///
/// A closed laptop is the most ordinary way a wallet is left unattended, and it
/// beats every idle timeout to it: the screen is shut, the machine may travel,
/// and the countdown would still be running when it opens.
///
/// Over the system bus that GIO already provides rather than a D-Bus crate —
/// this is one signal subscription, and it costs nothing to have. A machine
/// with no logind simply never signals, which is why every step here fails
/// quietly: locking is a precaution, and a precaution that cannot be armed is
/// not an error worth putting in front of anybody.
fn watch_for_sleep(sender: &ComponentSender<App>) -> Option<gtk::gio::SignalSubscription> {
    let Ok(bus) = gtk::gio::bus_get_sync(gtk::gio::BusType::System, gtk::gio::Cancellable::NONE)
    else {
        tracing::debug!("no system bus; the wallet will not lock on sleep");
        return None;
    };

    let sender = sender.clone();
    // The returned subscription unsubscribes when it is dropped, so the caller
    // holds it for as long as the app is running.
    let watch = bus.subscribe_to_signal(
        Some("org.freedesktop.login1"),
        Some("org.freedesktop.login1.Manager"),
        Some("PrepareForSleep"),
        Some("/org/freedesktop/login1"),
        None,
        gtk::gio::DBusSignalFlags::NONE,
        move |signal| {
            // The signal fires twice: `true` on the way down, `false` on the
            // way back. Locking on the way down is the point — afterwards the
            // machine has already been asleep with the wallet open.
            let going_to_sleep = signal
                .parameters
                .child_value(0)
                .get::<bool>()
                .unwrap_or(false);
            if going_to_sleep {
                sender.input(AppMsg::Lock(LockReason::Suspend));
            }
        },
    );
    tracing::debug!("will lock when this computer goes to sleep");
    Some(watch)
}

/// Whether an untouched wallet has been untouched for long enough.
///
/// Its own function so the rule can be tested: the branch that matters only
/// runs on a wallet somebody has unlocked, which is exactly the state a test
/// cannot reach through the interface.
fn should_lock(
    setting: crate::settings::IdleLock,
    unlocked: bool,
    untouched: std::time::Duration,
) -> bool {
    // A locked wallet is already locked, and a watch-only one that was never
    // unlocked has nothing to shut.
    if !unlocked {
        return false;
    }
    setting.duration().is_some_and(|after| untouched >= after)
}

/// Why a wallet was shut. Only the wording differs, but a lock that does not
/// say why reads as a fault.
#[derive(Debug, Clone, Copy)]
pub enum LockReason {
    Idle,
    Suspend,
    /// Somebody asked for it.
    Asked,
}
/// How far behind the estimated scan position to record a resume point.
///
/// Two thousand blocks — a difficulty period. The position is derived from a
/// fraction reported as a float, and resuming past where the scan truly
/// reached would skip blocks and lose transactions. Rescanning two thousand
/// blocks costs seconds.
const SCAN_MARGIN: u32 = 2_016;

/// How often to ask again when the node has gone quiet. Twenty seconds was
/// long enough that peers which had joined minutes ago were still missing from
/// the list.
const TICK: std::time::Duration = std::time::Duration::from_secs(8);

const ICONS: &[&str] = &[
    crate::APP_ID,
    "changes-prevent-symbolic",
    "channel-secure-symbolic",
    "document-open-recent-symbolic",
    "document-save-symbolic",
    "edit-copy-symbolic",
    "go-next-symbolic",
    "go-previous-symbolic",
    "list-add-symbolic",
    "network-idle-symbolic",
    "object-select-symbolic",
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
    /// TEMPORARY — see the welcome screen without starting over.
    PreviewWelcome,
    ShowRestore,
    ShowPreferences,
    /// Re-read what the header chain can tell us.
    RefreshChain,
    /// A dialog was dismissed, by us or by the person using it.
    PrefsClosed,
    UnlockClosed,
    ToggleDenomination,
    ForgetPeers,
    RenameWallet {
        paths: Paths,
        name: String,
    },
    SetShowFiat(bool),
    SetMempoolFees(bool),
    SetTor(bool),
    SetTorProxy(String),
    /// Ask the proxy whether it is there, and whether it is Tor.
    CheckTor,
    /// Try again after a failure, without treating it as a fresh request —
    /// a retry that fails must not quietly switch Tor off.
    RetryTor,
    /// Fill in a fee rate for a payment about to be made. Asked for when the
    /// send form comes into view, because both sources cost something: one a
    /// block download, the other a disclosure.
    EstimateFee,
    /// Slide the wallet list in over preferences.
    ShowWallets,
    /// Slide the recovery-phrase screen in over preferences.
    ShowRecoveryPhrase,
    /// Ask whether to delete the open wallet from this computer.
    AskRemoveWallet,
    /// Name a transaction or an address, or clear its name.
    SetLabel {
        kind: wallet::labels::Kind,
        reference: String,
        text: String,
    },
    /// Rebuild an unconfirmed payment at a higher fee, without signing it.
    PlanBump {
        txid: String,
        from: wallet::accounts::ScriptType,
        fee_rate: f64,
    },
    /// Write every label out as a BIP-329 file, or read one in.
    ExportLabels,
    ImportLabels,
    LabelFile {
        path: std::path::PathBuf,
        importing: bool,
    },
    /// Somebody touched the window. Not a state change — it only moves the
    /// clock the idle lock counts from.
    Stirred,
    SetIdleLock(crate::settings::IdleLock),
    /// Shut the wallet: the password is needed again to see it.
    Lock(LockReason),
    /// Set, change or remove the password on a watch-only wallet.
    AskWatchOnlyPassword,
    SetWatchOnlyPassword(crate::ui::send::Password),
    ClearWatchOnlyPassword,
    /// Confirm, then throw away this wallet's chain data and scan again.
    AskRescan,
    Rescan,
    /// Asked and answered.
    RemoveWallet(Paths),
    /// Re-present the password dialog for the wallet already on screen.
    PromptUnlock,
    /// Open a specific wallet from the list.
    OpenWallet(String),
    /// A wallet now exists on disk, or an existing one was unlocked. Both
    /// arrive with a watch-only summary and nothing secret. The paths say
    /// *which* wallet, which is what decides whether the running light client
    /// still belongs to what is on screen.
    Ready {
        paths: Paths,
        summary: Summary,
    },
    /// Reveal a fresh receive address on one derivation path.
    RevealAddress(crate::wallet::accounts::ScriptType),
    /// Build a transaction and hand back what it would cost. Watch-only: no
    /// password is involved until there is something to sign.
    PlanSend(Box<crate::wallet::send::Draft>),
    /// Sign the reviewed transaction and broadcast it.
    SendNow {
        plan: Box<crate::wallet::send::Plan>,
        password: crate::ui::send::Password,
    },
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
    Started {
        generation: u64,
        result: Result<Arc<Session>, String>,
    },
    Update {
        generation: u64,
        result: Result<Summary, String>,
    },
    /// `None` means the node stopped.
    Progress {
        generation: u64,
        progress: Option<Progress>,
    },
    Warning {
        generation: u64,
        notice: Option<Notice>,
    },
    Revealed {
        generation: u64,
        result: Result<(String, Summary), String>,
    },
    Chain {
        generation: u64,
        result: Result<crate::wallet::node::ChainInfo, String>,
    },
    Planned(Result<crate::wallet::send::Plan, String>),
    Sent(Result<(String, Summary), String>),
    Tick,
    Priced(Result<crate::price::Price, String>),
    /// The chain data has been cleared, or could not be.
    Rescanned(Result<(), String>),
    /// A replacement was built, or could not be.
    PlannedBump(Result<Box<crate::wallet::send::Plan>, String>),
    /// Time to ask whether the wallet has been left alone long enough.
    Idle,
    /// A watch-only wallet, opened without a password.
    Opened(Result<(Paths, Summary), String>),
    /// Who is connected, without waiting for the chain.
    Peers {
        generation: u64,
        peers: Vec<crate::wallet::node::PeerInfo>,
    },
    /// A fee rate in sat/vB, and where it came from.
    Estimated(Result<(f64, String), String>),
    /// Bootstrap news, while Tor is starting.
    TorProgress(String),
    /// Tor is up at this proxy, or could not be. `asked_for` distinguishes a
    /// switch someone just flipped — which goes back when it fails — from Tor
    /// already being on, where the honest answer is to stay on and not
    /// connect.
    TorReady {
        asked_for: bool,
        result: Result<crate::tor::Proxy, String>,
    },
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

        let chooser =
            Chooser::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    ChooserOutput::Open(id) => AppMsg::OpenWallet(id),
                    ChooserOutput::New => AppMsg::ShowOnboarding,
                    ChooserOutput::Import => AppMsg::ShowRestore,
                });
        let onboarding = Onboarding::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                OnboardingOutput::Created { paths, summary } => AppMsg::Ready { paths, summary },
                OnboardingOutput::WantsRestore => AppMsg::ShowRestore,
                OnboardingOutput::Cancelled => AppMsg::Back,
            });
        let restore =
            Restore::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    RestoreOutput::Imported { paths, summary } => AppMsg::Ready { paths, summary },
                    RestoreOutput::Cancelled => AppMsg::Back,
                });
        let unlock = Unlock::builder()
            .launch(())
            .forward(sender.input_sender(), |out| match out {
                UnlockOutput::Unlocked { paths, summary } => AppMsg::Ready { paths, summary },
            });
        let reveal = Reveal::builder().launch(()).detach();
        let wallet =
            WalletPage::builder()
                .launch(())
                .forward(sender.input_sender(), |out| match out {
                    crate::ui::wallet_page::WalletPageOutput::ShowPreferences => {
                        AppMsg::ShowPreferences
                    }
                    crate::ui::wallet_page::WalletPageOutput::Unlock => AppMsg::PromptUnlock,
                    crate::ui::wallet_page::WalletPageOutput::NewAddress(script_type) => {
                        AppMsg::RevealAddress(script_type)
                    }
                    crate::ui::wallet_page::WalletPageOutput::EstimateFee => AppMsg::EstimateFee,
                    crate::ui::wallet_page::WalletPageOutput::AskRescan => AppMsg::AskRescan,
                    // TEMPORARY — remove with the menu entry that sends it.
                    crate::ui::wallet_page::WalletPageOutput::ShowWelcome => AppMsg::PreviewWelcome,
                    crate::ui::wallet_page::WalletPageOutput::PlanBump {
                        txid,
                        from,
                        fee_rate,
                    } => AppMsg::PlanBump {
                        txid,
                        from,
                        fee_rate,
                    },
                    crate::ui::wallet_page::WalletPageOutput::SetLabel {
                        kind,
                        reference,
                        text,
                    } => AppMsg::SetLabel {
                        kind,
                        reference,
                        text,
                    },
                    crate::ui::wallet_page::WalletPageOutput::RetryTor => AppMsg::RetryTor,
                    crate::ui::wallet_page::WalletPageOutput::PlanSend(draft) => {
                        AppMsg::PlanSend(draft)
                    }
                    crate::ui::wallet_page::WalletPageOutput::Send { plan, password } => {
                        AppMsg::SendNow { plan, password }
                    }
                });

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
            theme.add_resource_path("/com/galaxoidlabs/Sieve/icons");

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

        follow_desktop_scheme(&style);
        if let Some(gio_settings) = &desktop {
            gio_settings.connect_changed(Some("color-scheme"), move |_, _| {
                follow_desktop_scheme(&adw::StyleManager::default());
            });
        }

        tracing::debug!(
            dark = style.is_dark(),
            desktop = desktop
                .as_ref()
                .map(|s| s.string("color-scheme").to_string()),
            "following the system color scheme"
        );
        style.connect_dark_notify({
            let sender = sender.clone();
            move |manager| sender.input(AppMsg::ColorSchemeChanged(manager.is_dark()))
        });

        // The desktop's own accent, where the desktop publishes one that
        // GNOME's settings do not carry. Its own provider, at a priority above
        // libadwaita's stylesheet and below a user's own, so it overrides the
        // default accent and nothing overrides the person sitting here.
        let accent = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &accent,
                gtk::STYLE_PROVIDER_PRIORITY_SETTINGS,
            );
        }
        apply_palette(&accent);
        let accent_provider = Some(accent.clone());

        // A theme change replaces the symlink this reads through, so the file
        // is watched rather than polled. Omarchy also sets `color-scheme` on
        // every theme change, but only *changes* it when the mode changes —
        // switching between two dark themes emits nothing.
        let watching = gtk::gio::File::for_path(crate::palette::watch_dir());
        let theme_watch = if let Ok(monitor) = watching.monitor_directory(
            gtk::gio::FileMonitorFlags::WATCH_MOVES,
            gtk::gio::Cancellable::NONE,
        ) {
            let accent = accent.clone();
            monitor.connect_changed(move |_, _, _, _| apply_palette(&accent));
            Some(monitor)
        } else {
            None
        };

        // Pages are registered up front and navigated by tag. The view owns
        // the history, which is what lets back work everywhere without each
        // screen inventing its own.
        unlock_dialog.set_child(Some(unlock.widget()));

        // The wallet list is not part of the main path any more: it lives
        // inside preferences, which is where you go to change wallets.
        let chooser_page = adw::NavigationPage::new(chooser.widget(), "Wallets");
        chooser_page.set_tag(Some("wallets"));

        let reveal_page = adw::NavigationPage::new(reveal.widget(), "Recovery phrase");
        reveal_page.set_tag(Some("phrase"));

        for (tag, title, child, can_pop) in [
            (
                "wallet",
                "Wallet",
                wallet.widget().clone().upcast::<gtk::Widget>(),
                true,
            ),
            // Setup and import drive their own back button: theirs steps
            // backwards through a flow before leaving it, and two back buttons
            // in one header is worse than one that does both jobs.
            (
                "onboarding",
                "New wallet",
                onboarding.widget().clone().upcast(),
                false,
            ),
            (
                "restore",
                "Import",
                restore.widget().clone().upcast(),
                false,
            ),
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

        let mut model = App {
            blocks_recorded: false,
            sleep_watch: None,
            theme_watch,
            accent_provider,
            bumping: None,
            last_seen: std::time::Instant::now(),
            stirs: 0,
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
            reveal_page,
            prefs_page: None,
            prefs_open: false,
            unlock_open: false,
            settings,
            unlock_dialog: unlock_dialog.clone(),
            active: None,
            balance_sats: None,
            generation: 0,
            scan_recorded: None,
            fee_estimate: None,
            chain_tip: None,
            peers_read: None,
            amounts_row: None,
            tor_status: None,
            tor_failed: false,
            tor_asked_for: false,
            tor_row: None,
            tor_switch: None,
            tor_active: None,
            session: None,
            chooser,
            onboarding,
            restore,
            unlock,
            wallet,
            reveal,
            unlocked: false,
        };
        let widgets = view_output!();

        // Everything that reaches this window, in the capture phase, before
        // any widget gets a say. A key, a click, a scroll, a moved pointer:
        // all of it is somebody being here, and it only ever moves a clock.
        //
        // One controller on the window rather than handlers on the widgets:
        // every future screen is covered by construction, which is the sort of
        // thing that is otherwise forgotten exactly once and quietly stops
        // working.
        {
            let watching = gtk::EventControllerLegacy::new();
            watching.set_propagation_phase(gtk::PropagationPhase::Capture);
            let sender = sender.clone();
            watching.connect_event(move |_, _| {
                sender.input(AppMsg::Stirred);
                // Seen, never swallowed.
                gtk::glib::Propagation::Proceed
            });
            root.add_controller(watching);
        }
        model.watch_for_idle(&sender);
        model.sleep_watch = watch_for_sleep(&sender);

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

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
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
            // TEMPORARY — the welcome step itself, which EnteredByChoice skips.
            AppMsg::PreviewWelcome => {
                self.close_prefs();
                self.onboarding.emit(OnboardingMsg::PreviewWelcome);
                self.nav.push_by_tag("onboarding");
            }

            AppMsg::ShowOnboarding => {
                self.close_prefs();
                // Always: this is only reachable from the wallet list, where
                // creating a wallet was already chosen. Gating it on an active
                // wallet was wrong — active is not set until a wallet unlocks,
                // so opening preferences from a locked wallet fell back to the
                // welcome screen.
                self.onboarding.emit(OnboardingMsg::EnteredByChoice);
                self.nav.push_by_tag("onboarding");
            }
            AppMsg::ShowRestore => {
                self.close_prefs();
                self.nav.push_by_tag("restore");
            }

            AppMsg::RefreshChain => {
                let Some(session) = self.session.clone() else {
                    return;
                };
                let generation = self.generation;
                sender.oneshot_command(async move {
                    AppCmd::Chain {
                        generation,
                        result: session.chain_info().await.map_err(|e| e.to_string()),
                    }
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

            AppMsg::PrefsClosed => {
                self.prefs_open = false;
                // The words are in memory only while they are on screen.
                self.reveal.emit(RevealMsg::Clear);
            }
            AppMsg::UnlockClosed => {
                self.unlock_open = false;
                // Dismissed without unlocking: the notice behind it is the
                // only thing left saying why the wallet is empty.
                self.wallet.emit(WalletPageMsg::SetAskingToUnlock(false));
            }

            AppMsg::ToggleDenomination => {
                self.settings.denomination = self.settings.denomination.toggled();
                self.settings.save();
                self.wallet
                    .emit(WalletPageMsg::SetDenomination(self.settings.denomination));
                self.refresh_amounts_row();
            }

            AppMsg::SetTor(on) => {
                self.settings.tor = on;
                self.settings.save();

                if !on {
                    // Ours to stop; a system daemon we merely borrowed is left
                    // alone by `stop`.
                    crate::tor::daemon::stop();
                    self.tor_active = None;
                    self.tor_status = None;
                    // Not just the preferences page: the network view carries
                    // the same claim, and leaving it saying "through Tor" is
                    // the most dangerous kind of stale — it describes the
                    // protection you no longer have.
                    self.refresh_tor_row();
                    self.wallet.emit(WalletPageMsg::TorProblem(None));
                    self.restart_session(&sender);
                    return;
                }

                // Brought up before it is believed. Turning Tor on and finding
                // out later that nothing was listening is the failure this
                // whole feature exists to avoid.
                self.ensure_tor_asked(&sender);
            }

            AppMsg::SetTorProxy(address) => {
                let address = address.trim().to_string();
                self.settings.tor_proxy = (!address.is_empty()).then_some(address);
                self.settings.save();
                sender.input(AppMsg::CheckTor);
            }

            AppMsg::CheckTor => {
                self.tor_active = None;
                self.ensure_tor_asked(&sender);
            }

            AppMsg::RetryTor => {
                self.tor_active = None;
                self.ensure_tor(&sender);
            }

            AppMsg::SetMempoolFees(on) => {
                self.settings.mempool_fees = on;
                self.settings.save();
                // The source changed, so the number on screen is from the
                // other one.
                self.fee_estimate = None;
                sender.input(AppMsg::EstimateFee);
            }

            AppMsg::EstimateFee => {
                let network = self
                    .active
                    .as_ref()
                    .and_then(wallet::Meta::load)
                    .map(|m| m.network)
                    .unwrap_or_else(|| "bitcoin".into());

                let proxy = self.tor_proxy();
                if self.settings.mempool_fees {
                    // Cached by nothing: the point of asking is a current
                    // number, and the request is cheap in bandwidth.
                    sender.oneshot_command(async move {
                        let fetched = tokio::task::spawn_blocking(move || {
                            crate::fees::fetch(&network, proxy).map_err(|e| e.to_string())
                        })
                        .await;
                        AppCmd::Estimated(match fetched {
                            Ok(Ok(rates)) => Ok((rates.suggested(), rates.summary())),
                            Ok(Err(message)) => Err(message),
                            Err(e) => Err(format!("could not ask mempool.space: {e}")),
                        })
                    });
                    return;
                }

                let Some(session) = self.session.clone() else {
                    return;
                };
                // One block download per tip, not one per visit.
                if let Some((height, rate, source)) = &self.fee_estimate
                    && self.chain_tip == Some(*height)
                {
                    let (rate, source) = (*rate, source.clone());
                    self.wallet.emit(WalletPageMsg::FeeSuggestion(rate, source));
                    return;
                }
                sender.oneshot_command(async move {
                    AppCmd::Estimated(
                        session
                            .average_fee_at_tip()
                            .await
                            .map(|(height, rate)| (rate, format!("Average of block {height}")))
                            .map_err(|e| e.to_string()),
                    )
                });
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

            AppMsg::SetLabel {
                kind,
                reference,
                text,
            } => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                let mut labels = wallet::labels::Labels::load(&paths.dir);
                labels.set(kind, &reference, &text);
                if let Err(e) = labels.save(&paths.dir) {
                    tracing::error!(%e, "could not save a label");
                    self.wallet
                        .emit(WalletPageMsg::Toast(crate::ui::send::capitalise(
                            &e.to_string(),
                        )));
                    return;
                }
                self.wallet.emit(WalletPageMsg::SetLabels(Box::new(labels)));
            }

            AppMsg::ExportLabels | AppMsg::ImportLabels => {
                let importing = matches!(msg, AppMsg::ImportLabels);
                let Some(window) = self.nav.root().and_downcast::<gtk::Window>() else {
                    return;
                };

                let filter = gtk::FileFilter::new();
                filter.set_name(Some("BIP-329 labels"));
                filter.add_pattern("*.jsonl");
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&filter);

                let dialog = gtk::FileDialog::builder()
                    .title(if importing {
                        "Import labels"
                    } else {
                        "Export labels"
                    })
                    .filters(&filters)
                    .modal(true)
                    .build();

                let sender = sender.clone();
                let chosen = move |file: Result<gtk::gio::File, _>| {
                    if let Ok(file) = file
                        && let Some(path) = file.path()
                    {
                        sender.input(AppMsg::LabelFile { path, importing });
                    }
                };
                if importing {
                    dialog.open(Some(&window), gtk::gio::Cancellable::NONE, move |file| {
                        chosen(file)
                    });
                } else {
                    dialog.set_initial_name(Some("labels.jsonl"));
                    dialog.save(Some(&window), gtk::gio::Cancellable::NONE, move |file| {
                        chosen(file)
                    });
                }
            }

            AppMsg::LabelFile { path, importing } => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                let result = if importing {
                    self.import_labels(&paths, &path)
                } else {
                    self.export_labels(&paths, &path)
                };
                let message = match result {
                    Ok(message) => message,
                    Err(e) => {
                        tracing::error!(%e, importing, "labels");
                        crate::ui::send::capitalise(&e.to_string())
                    }
                };
                self.prefs.add_toast(adw::Toast::new(&message));
            }

            AppMsg::Stirred => {
                self.stirs += 1;
                self.last_seen = std::time::Instant::now();
            }

            AppMsg::SetIdleLock(choice) => {
                let was_off = self.settings.idle_lock.duration().is_none();
                self.settings.idle_lock = choice;
                self.settings.save();
                // Turning it back on has to restart the poller, which stopped
                // itself when it was turned off.
                self.last_seen = std::time::Instant::now();
                if was_off {
                    self.watch_for_idle(&sender);
                }
            }

            AppMsg::Lock(reason) => {
                // Nothing to shut, or nothing to shut it against.
                if !self.unlocked || self.active.is_none() {
                    return;
                }
                tracing::info!(?reason, "locking the wallet");

                // Only the view is shut. The node keeps running: syncing is
                // watch-only work that needs no key, and stopping it would
                // mean re-downloading filters to see a balance that was on
                // screen a minute ago.
                self.unlocked = false;
                self.wallet.emit(WalletPageMsg::SetLocked(true));
                self.close_prefs();
                self.reveal.emit(RevealMsg::Clear);

                // Said out loud, because a wallet that shut itself while you
                // were reading it otherwise looks like a bug.
                self.wallet.emit(WalletPageMsg::Toast(
                    match reason {
                        LockReason::Idle => "Locked after a while untouched",
                        LockReason::Suspend => "Locked because this computer went to sleep",
                        LockReason::Asked => "Locked",
                    }
                    .into(),
                ));
            }

            AppMsg::PlanBump {
                txid,
                from,
                fee_rate,
            } => {
                let Some(session) = self.session.clone() else {
                    self.wallet.emit(WalletPageMsg::BumpPlanned(Box::new(Err(
                        "Not connected to the network yet — wait for peers".into(),
                    ))));
                    return;
                };
                // Rounded up: a replacement that pays fractionally less than
                // it promised is a replacement the network will not relay.
                let Some(rate) =
                    bdk_wallet::bitcoin::FeeRate::from_sat_per_vb(fee_rate.ceil() as u64)
                else {
                    self.wallet.emit(WalletPageMsg::BumpPlanned(Box::new(Err(
                        "That fee rate cannot be used".into(),
                    ))));
                    return;
                };
                sender.oneshot_command(async move {
                    AppCmd::PlannedBump(
                        session
                            .plan_bump(&txid, from, rate)
                            .await
                            .map(Box::new)
                            .map_err(|e| crate::ui::send::capitalise(&e.to_string())),
                    )
                });
            }

            AppMsg::AskWatchOnlyPassword => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                let Some(window) = self.nav.root() else {
                    return;
                };
                let locked = paths.lock.exists();

                let dialog = adw::AlertDialog::new(
                    Some(if locked {
                        "Change this password?"
                    } else {
                        "Set a password?"
                    }),
                    Some(
                        "This wallet holds no keys, so there is nothing here to decrypt. A \
                         password shuts what is on screen — the balance, the history, the \
                         addresses — against somebody who opens Sieve at this machine.\n\n\
                         It does not encrypt the files. Anybody holding them can still read \
                         this wallet's history, password or not.",
                    ),
                );
                dialog.add_response("cancel", "Cancel");
                if locked {
                    dialog.add_response("clear", "Remove it");
                    dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
                }
                dialog.add_response("set", if locked { "Change" } else { "Set" });
                dialog.set_response_appearance("set", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                // Typed twice, because this password is never checked against
                // anything the person already knows: a wallet with no keys has
                // no coins to fail to spend, so a typo would simply lock them
                // out of their own history with no way to notice until later.
                let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
                fields.set_margin_top(6);

                let entry = gtk::PasswordEntry::new();
                entry.set_show_peek_icon(true);
                entry.set_placeholder_text(Some("New password"));
                fields.append(&entry);

                let confirm = gtk::PasswordEntry::new();
                confirm.set_show_peek_icon(true);
                confirm.set_placeholder_text(Some("Type it again"));
                fields.append(&confirm);

                let mismatch = gtk::Label::new(None);
                mismatch.add_css_class("error");
                mismatch.add_css_class("caption");
                mismatch.set_halign(gtk::Align::Start);
                mismatch.set_visible(false);
                fields.append(&mismatch);

                dialog.set_extra_child(Some(&fields));
                dialog.set_response_enabled("set", false);

                // Enabled only when both fields agree and neither is empty,
                // so the failure is visible before the button rather than in a
                // toast afterwards.
                let agree = {
                    let dialog = dialog.clone();
                    let entry = entry.clone();
                    let confirm = confirm.clone();
                    let mismatch = mismatch.clone();
                    move || {
                        let first = entry.text();
                        let second = confirm.text();
                        let filled = !first.trim().is_empty();
                        let same = first == second;
                        dialog.set_response_enabled("set", filled && same);
                        mismatch.set_visible(!second.is_empty() && !same);
                        mismatch.set_label("Those do not match");
                    }
                };
                entry.connect_changed({
                    let agree = agree.clone();
                    move |_| agree()
                });
                confirm.connect_changed(move |_| agree());

                {
                    let sender = sender.clone();
                    let entry = entry.clone();
                    dialog.connect_response(None, move |_, response| match response {
                        "set" => {
                            sender.input(AppMsg::SetWatchOnlyPassword(crate::ui::send::Password(
                                zeroize::Zeroizing::new(entry.text().to_string()),
                            )))
                        }
                        "clear" => sender.input(AppMsg::ClearWatchOnlyPassword),
                        _ => {}
                    });
                }
                dialog.present(Some(&window));
            }

            AppMsg::SetWatchOnlyPassword(password) => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                if password.0.trim().is_empty() {
                    self.prefs
                        .add_toast(adw::Toast::new("A password cannot be empty"));
                    return;
                }
                match wallet::set_watch_only_password(&paths, password.0.as_bytes()) {
                    Ok(()) => {
                        self.prefs.add_toast(adw::Toast::new(
                            "This wallet will ask for that password when it is opened",
                        ));
                        self.rebuild_preferences(&sender);
                    }
                    Err(e) => {
                        tracing::error!(%e, "could not set the wallet password");
                        self.prefs
                            .add_toast(adw::Toast::new(&crate::ui::send::capitalise(
                                &e.to_string(),
                            )));
                    }
                }
            }

            AppMsg::ClearWatchOnlyPassword => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                match wallet::clear_watch_only_password(&paths) {
                    Ok(()) => {
                        self.prefs
                            .add_toast(adw::Toast::new("This wallet now opens without asking"));
                        self.rebuild_preferences(&sender);
                    }
                    Err(e) => {
                        tracing::error!(%e, "could not remove the wallet password");
                        self.prefs
                            .add_toast(adw::Toast::new(&crate::ui::send::capitalise(
                                &e.to_string(),
                            )));
                    }
                }
            }

            AppMsg::AskRescan => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                let birthday = wallet::Meta::load(&paths).map(|m| m.birthday_height);

                // Said in terms of what it costs, because that is the whole
                // question: the wallet is not at risk, the afternoon might be.
                let mut body = String::from(
                    "Sieve will forget every block it has checked for this wallet and check \
                     them again, one compact filter at a time, from the wallet's \
                     birthday.\n\nNothing here puts your coins at risk — the keys are \
                     untouched and the history is rebuilt from the chain itself. But a \
                     wallet that goes back years can take hours to catch up, and a payment \
                     you have broadcast but that no block holds yet will be forgotten until \
                     it confirms.",
                );
                if let Some(height) = birthday {
                    body.push_str(&format!(
                        "\n\nScanning again from block {}.",
                        crate::ui::wallet_page::thousands(height)
                    ));
                }

                let dialog = adw::AlertDialog::new(Some("Scan the chain again?"), Some(&body));
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("rescan", "Rescan");
                // Destructive of work, not of money — but the hours are real,
                // so it is not the answer a stray Return key gives.
                dialog.set_response_appearance("rescan", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                {
                    let sender = sender.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response == "rescan" {
                            sender.input(AppMsg::Rescan);
                        }
                    });
                }

                if let Some(window) = self.nav.root() {
                    dialog.present(Some(&window));
                }
            }

            AppMsg::Rescan => {
                let Some(paths) = self.active.clone() else {
                    return;
                };

                // The node holds these database files open, so it goes first.
                if let Some(session) = self.session.take() {
                    session.shutdown();
                }
                self.generation += 1;
                self.wallet.emit(WalletPageMsg::Reset);
                self.restate_wallet(&sender);

                sender.spawn_oneshot_command(move || {
                    AppCmd::Rescanned(wallet::rescan(&paths).map_err(|e| e.to_string()))
                });
            }

            AppMsg::AskRemoveWallet => {
                let Some(paths) = self.active.clone() else {
                    return;
                };
                let id = paths
                    .dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let name = wallet::Meta::load(&paths)
                    .map(|m| m.display_name(&id))
                    .unwrap_or_else(|| id.clone());
                let network = wallet::Meta::load(&paths)
                    .map(|m| m.network)
                    .unwrap_or_else(|| "bitcoin".into());

                // The whole content of this warning is the difference between
                // deleting a file and losing money: the coins stay on the
                // chain, and the recovery phrase is what reaches them. For a
                // wallet nobody wrote down, this file is the only way back.
                let mut body = format!(
                    "This deletes {name} from this computer: the encrypted key file, its \
                     history, everything.\n\nYour coins stay on the Bitcoin network. Only \
                     the recovery phrase can reach them again — if you have not written it \
                     down, they are gone."
                );
                let holds = self.balance_sats.unwrap_or(0);
                if holds > 0 {
                    body.push_str(&format!(
                        "\n\nThis wallet holds {}.",
                        self.settings.denomination.format(holds, &network)
                    ));
                }

                let dialog = adw::AlertDialog::new(Some(&format!("Remove {name}?")), Some(&body));
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("remove", "Remove");
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                // The safe answer is the one a stray Return key gives.
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                // A wallet with coins in it asks for its name to be typed. The
                // ceremony is the point: it is the difference between a slip
                // and a decision.
                if holds > 0 {
                    let group = adw::PreferencesGroup::new();
                    let entry = adw::EntryRow::new();
                    entry.set_title(&format!("Type “{name}” to confirm"));
                    group.add(&entry);
                    dialog.set_extra_child(Some(&group));
                    dialog.set_response_enabled("remove", false);

                    let confirm = dialog.clone();
                    let wanted = name.clone();
                    entry.connect_changed(move |row| {
                        confirm.set_response_enabled("remove", row.text().trim() == wanted);
                    });
                }

                {
                    let sender = sender.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response == "remove" {
                            sender.input(AppMsg::RemoveWallet(paths.clone()));
                        }
                    });
                }

                if let Some(window) = self.nav.root() {
                    dialog.present(Some(&window));
                }
            }

            AppMsg::RemoveWallet(paths) => {
                let was_open = self.active.as_ref().map(|p| &p.dir) == Some(&paths.dir);
                if was_open {
                    // Nothing may still be reading the files about to go.
                    if let Some(session) = self.session.take() {
                        session.shutdown();
                    }
                    self.generation += 1;
                }

                if let Err(e) = wallet::remove(&paths) {
                    tracing::error!(%e, "could not remove the wallet");
                    self.prefs
                        .add_toast(adw::Toast::new(&crate::ui::send::capitalise(
                            &e.to_string(),
                        )));
                    return;
                }

                let id = paths
                    .dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string());
                if self.settings.last_wallet == id {
                    self.settings.last_wallet = None;
                    self.settings.save();
                }

                if was_open {
                    self.active = None;
                    self.balance_sats = None;
                    self.unlocked = false;
                    self.wallet.emit(WalletPageMsg::Reset);
                    self.wallet.emit(WalletPageMsg::SetLocked(true));
                }

                self.chooser.emit(ChooserMsg::Refresh);
                self.close_prefs();

                // Somewhere to go afterwards: another wallet if there is one,
                // and the way to make a first one if there is not.
                if wallet::list_wallets().is_empty() {
                    self.nav.push_by_tag("onboarding");
                } else {
                    sender.input(AppMsg::ShowPreferences);
                    sender.input(AppMsg::ShowWallets);
                }
            }

            AppMsg::ShowRecoveryPhrase => {
                // The row is already insensitive while locked; this is the
                // same rule at the place that acts on it, so the screen stays
                // unreachable however the message arrived.
                let Some(paths) = self.active.clone().filter(|_| self.unlocked) else {
                    return;
                };
                // Prepare first: it clears whatever the last visit decrypted,
                // so the page never slides in already showing a phrase.
                self.reveal.emit(RevealMsg::Prepare(Box::new(paths)));
                self.prefs.push_subpage(&self.reveal_page);
            }

            AppMsg::PromptUnlock => {
                if let Some(root) = self.nav.root().and_then(|r| r.root()) {
                    self.unlock_dialog.present(Some(&root));
                    self.unlock_open = true;
                    self.wallet.emit(WalletPageMsg::SetAskingToUnlock(true));
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

                self.unlocked = false;
                self.wallet.emit(WalletPageMsg::SetLocked(true));
                self.wallet.emit(WalletPageMsg::SetName(name.clone()));
                self.close_prefs();

                // A watch-only wallet has no vault, so there is nothing for a
                // password to decrypt — but there is still a balance and a
                // history on screen, and somebody may reasonably want those
                // shut. A wallet that has been given a lock asks for it; one
                // that has not opens straight away, as before.
                if wallet::Meta::load(&paths).is_some_and(|m| m.watch_only) && !paths.lock.exists()
                {
                    let opening = paths.clone();
                    sender.oneshot_command(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            wallet::open_watch_only(&opening)
                                .map(|summary| (opening, summary))
                                .map_err(|e| e.to_string())
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                        AppCmd::Opened(result)
                    });
                    return;
                }

                self.unlock.emit(UnlockMsg::Open { paths, name });
                sender.input(AppMsg::PromptUnlock);
            }

            AppMsg::Ready { paths, summary } => {
                // Opening a different wallet must retire the running client.
                // Otherwise the previous wallet's node keeps feeding this
                // screen, and a freshly imported wallet shows the old one's
                // sync state — including a reassuring "Up to date" it has not
                // earned.
                let switched = self.active.as_ref().map(|p| &p.dir) != Some(&paths.dir);
                if switched {
                    // Cleared whether or not a client was running. A wallet
                    // whose node never started still left its balance, its
                    // chain and its peers on screen for the next one.
                    if let Some(session) = self.session.take() {
                        tracing::info!("switching wallets; stopping the previous light client");
                        session.shutdown();
                    }
                    // Anything still in flight belongs to the wallet being left.
                    self.generation += 1;
                    self.wallet.emit(WalletPageMsg::Reset);
                }

                let id = paths
                    .dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let meta = wallet::Meta::load(&paths);
                let watch_only = meta.as_ref().is_some_and(|m| m.watch_only);

                self.active = Some(paths.clone());
                self.balance_sats = Some(summary.balance_sats);
                self.unlocked = true;
                self.wallet.emit(WalletPageMsg::SetWatchOnly(watch_only));

                // The name, which this path used to leave alone. Opening a
                // wallet from the list sets it; arriving here from an import
                // or from creating one did not, so a freshly imported wallet
                // wore the previous wallet's name over its own balance, its
                // own chain and its own peers. Everything on screen was right
                // except the one word saying whose it was.
                self.wallet.emit(WalletPageMsg::SetName(
                    meta.as_ref()
                        .map(|m| m.display_name(&id))
                        .unwrap_or_else(|| id.clone()),
                ));

                // And it is the wallet to come back to next time. Without
                // this, importing a wallet and restarting opened the old one.
                if self.settings.last_wallet.as_deref() != Some(id.as_str()) {
                    self.settings.last_wallet = Some(id);
                    self.settings.save();
                }

                if let Some(meta) = &meta {
                    // So the sync status can say how big the job is, rather
                    // than only how far through it is.
                    self.wallet
                        .emit(WalletPageMsg::SetBirthday(meta.birthday_height));
                    self.wallet
                        .emit(WalletPageMsg::SetNetwork(meta.network.clone()));
                    self.wallet
                        .emit(WalletPageMsg::SetMatchedBlocks(meta.matched_blocks));
                }
                self.wallet.emit(WalletPageMsg::SetLabels(Box::new(
                    wallet::labels::Labels::load(&paths.dir),
                )));
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

                self.start_session(&sender);
            }

            AppMsg::RevealAddress(script_type) => {
                let Some(session) = self.session.clone() else {
                    // Nothing to reveal from until the client is up.
                    return;
                };
                let generation = self.generation;
                sender.oneshot_command(async move {
                    AppCmd::Revealed {
                        generation,
                        result: session
                            .reveal_next(script_type)
                            .await
                            .map_err(|e| e.to_string()),
                    }
                });
            }

            AppMsg::PlanSend(draft) => {
                let Some(session) = self.session.clone() else {
                    self.wallet.emit(WalletPageMsg::Planned(Box::new(Err(
                        "Not connected to the network yet — wait for peers".into(),
                    ))));
                    return;
                };
                sender.oneshot_command(async move {
                    AppCmd::Planned(session.plan(&draft).await.map_err(|e| e.to_string()))
                });
            }

            AppMsg::SendNow { plan, password } => {
                // Travels with the plan, so it describes this signature and no
                // other.
                self.bumping = plan.replaces.clone();
                let (Some(session), Some(paths)) = (self.session.clone(), self.active.clone())
                else {
                    self.wallet.emit(WalletPageMsg::Sent(Box::new(Err(
                        "Not connected to the network yet — wait for peers".into(),
                    ))));
                    return;
                };

                sender.oneshot_command(async move {
                    // Argon2 would hold a runtime worker for the best part of a
                    // second, so the unwrapping happens on the blocking pool.
                    let opened = tokio::task::spawn_blocking(move || {
                        let blob = std::fs::read(&paths.vault)
                            .map_err(|e| format!("Cannot read the wallet file: {e}"))?;
                        crate::vault::open(&blob, password.0.as_bytes()).map_err(|e| e.to_string())
                    })
                    .await;

                    let secret = match opened {
                        Ok(Ok(secret)) => secret,
                        Ok(Err(message)) => return AppCmd::Sent(Err(message)),
                        Err(e) => return AppCmd::Sent(Err(format!("Signing failed: {e}"))),
                    };
                    let text = match std::str::from_utf8(&secret) {
                        Ok(text) => text.to_string(),
                        Err(_) => {
                            return AppCmd::Sent(
                                Err("The wallet file is not readable text".into()),
                            );
                        }
                    };

                    AppCmd::Sent(
                        session
                            .sign_and_send(*plan, &text, None)
                            .await
                            .map(|(txid, summary)| (txid.to_string(), summary))
                            .map_err(|e| e.to_string()),
                    )
                });
            }

            AppMsg::ColorSchemeChanged(dark) => {
                if let Some(provider) = &self.accent_provider {
                    apply_palette(provider);
                }
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
            AppCmd::Started {
                generation,
                result: Ok(session),
            } if self.current(generation) => {
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
            AppCmd::Started {
                generation,
                result: Err(message),
            } if self.current(generation) => {
                tracing::error!(%message, "could not start the light client");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }
            AppCmd::Chain {
                generation,
                result: Ok(info),
            } if self.current(generation) => {
                // What makes a block-derived fee estimate stale.
                self.chain_tip = Some(info.tip_height);
                self.wallet.emit(WalletPageMsg::SetChain(Some(info)));
            }
            AppCmd::Rescanned(Ok(())) => {
                tracing::info!("chain data cleared; scanning again from the birthday");
                self.start_session(&sender);
            }
            AppCmd::Rescanned(Err(message)) => {
                tracing::error!(%message, "could not clear the chain data");
                self.wallet
                    .emit(WalletPageMsg::Toast(crate::ui::send::capitalise(&message)));
                // Whatever survived, the wallet is still watchable.
                self.start_session(&sender);
            }
            AppCmd::Idle => {
                // Trace, not debug: this fires every fifteen seconds for the
                // life of the process, and it has already answered the
                // question it was added for.
                tracing::trace!(
                    idle_s = self.last_seen.elapsed().as_secs(),
                    stirs = self.stirs,
                    unlocked = self.unlocked,
                    setting = ?self.settings.idle_lock,
                    "idle check"
                );
                if should_lock(
                    self.settings.idle_lock,
                    self.unlocked,
                    self.last_seen.elapsed(),
                ) {
                    sender.input(AppMsg::Lock(LockReason::Idle));
                }
                self.watch_for_idle(&sender);
            }

            AppCmd::Tick => {
                self.check_tor(&sender);
                // The peers separately from the chain. kyoto stops warning
                // about connections once it has enough, so between the last
                // warning and the next tick nothing was asking who is
                // connected — and reading the chain, which used to be the only
                // thing that asked, waits on a header.
                self.read_peers(&sender);
                sender.input(AppMsg::RefreshChain);
                self.schedule_tick(&sender);
            }
            AppCmd::Chain {
                generation,
                result: Err(message),
            } if self.current(generation) => {
                tracing::warn!(%message, "could not read the chain");
            }
            AppCmd::Update {
                generation,
                result: Ok(summary),
            } if self.current(generation) => {
                tracing::debug!(balance = summary.balance_sats, "wallet updated");
                // What this scan cost in blocks, so the next one can draw a
                // bar for the phase the node reports no total for. Once per
                // session: later updates are new tips, not this scan.
                if !self.blocks_recorded
                    && let (Some(session), Some(paths)) = (&self.session, &self.active)
                {
                    let read = session.blocks_read() as u32;
                    if read > 0 {
                        wallet::Meta::record_matched_blocks(paths, read);
                        self.wallet
                            .emit(WalletPageMsg::SetMatchedBlocks(Some(read)));
                        tracing::debug!(blocks = read, "recorded what this scan had to read");
                    }
                    self.blocks_recorded = true;
                }
                self.balance_sats = Some(summary.balance_sats);
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.wallet
                    .emit(WalletPageMsg::SetProgress(Progress::Synced));
                sender.input(AppMsg::RefreshChain);
                self.await_update(&sender);
            }
            AppCmd::Update {
                generation,
                result: Err(message),
            } if self.current(generation) => {
                // Do not re-arm: the loop would spin on a persistent failure.
                tracing::error!(%message, "sync failed");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }
            AppCmd::Progress {
                generation,
                progress: Some(progress),
            } if self.current(generation) => {
                // The block headers are complete the moment filter work
                // begins: kyoto walks the whole header chain before asking for
                // a single filter header. That is when to write them out.
                //
                // Storing them when a wallet *update* landed instead — as this
                // did — meant waiting for the entire scan to finish, so a
                // recovery scan that ran for an hour and was then restarted
                // saved nothing and fetched every header again. The headers
                // were ready in the first two minutes.
                // Headers are banked while they arrive, not only once they
                // are all in. The walk takes a quarter of an hour on a
                // whole-chain wallet, and a restart inside that window used to
                // lose every one of them — which is the window anybody
                // watching a slow wallet actually restarts in.
                self.record_scan_progress(&progress, &sender);
                self.wallet.emit(WalletPageMsg::SetProgress(progress));
                self.await_progress(&sender);
            }
            AppCmd::Progress { progress: None, .. } => {
                tracing::warn!("the node stopped emitting progress")
            }
            AppCmd::Warning {
                generation,
                notice: Some(notice),
            } if self.current(generation) => {
                match notice {
                    Notice::Peers {
                        connected,
                        required,
                    } => {
                        self.wallet.emit(WalletPageMsg::Peers {
                            connected,
                            required,
                        });
                        // Just the peers, not the whole chain view: reading the
                        // chain waits on a header, which during a header
                        // download is the slowest thing the node is doing. That
                        // is why the list used to stay empty until the sync had
                        // finished — precisely when it stopped being useful.
                        self.read_peers(&sender);
                    }
                    Notice::Problem(message) => self.wallet.emit(WalletPageMsg::Note(message)),
                    Notice::Ignorable => {}
                }
                self.await_warning(&sender);
            }
            AppCmd::Warning { notice: None, .. } => {
                tracing::warn!("the node stopped emitting warnings")
            }
            AppCmd::Opened(Ok((paths, summary))) => {
                sender.input(AppMsg::Ready { paths, summary });
            }
            AppCmd::Opened(Err(message)) => {
                tracing::error!(%message, "could not open the watch-only wallet");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }

            AppCmd::Peers { generation, peers } if self.current(generation) => {
                self.wallet.emit(WalletPageMsg::SetPeers(peers))
            }

            AppCmd::TorProgress(message) => {
                self.tor_status = Some(message);
                self.refresh_tor_row();
            }

            AppCmd::TorReady {
                result: Ok(proxy), ..
            } => {
                tracing::info!(%proxy, "Tor is ready");
                self.tor_failed = false;
                self.tor_active = Some(proxy);
                self.wallet.emit(WalletPageMsg::TorProblem(None));
                self.tor_status = Some(if crate::tor::daemon::is_ours() {
                    format!("Connected through Tor at {proxy}, started by Sieve")
                } else {
                    format!("Connected through Tor at {proxy}")
                });
                self.refresh_tor_row();
                self.restart_session(&sender);
            }

            AppCmd::TorReady {
                asked_for,
                result: Err(message),
            } => {
                tracing::warn!(%message, "could not bring Tor up");
                self.tor_active = None;
                self.tor_failed = true;
                let message = crate::ui::send::capitalise(&message);

                if asked_for {
                    // Somebody just flipped the switch and it could not be
                    // done, so the switch goes back rather than leaving the
                    // app looking as though it is on Tor when it is not.
                    self.settings.tor = false;
                    self.settings.save();
                    self.prefs.add_toast(adw::Toast::new(&message));
                    self.wallet.emit(WalletPageMsg::TorProblem(None));
                } else {
                    // Tor was already on and could not be brought up. Going
                    // out over the clear instead would be the one thing this
                    // must never do quietly, so nothing connects and the
                    // wallet says so, with a way to try again.
                    self.wallet
                        .emit(WalletPageMsg::TorProblem(Some(message.clone())));
                }

                self.tor_status = Some(message);
                self.refresh_tor_row();
            }

            AppCmd::Estimated(Ok((rate, source))) => {
                if let Some(height) = self.chain_tip {
                    self.fee_estimate = Some((height, rate, source.clone()));
                }
                self.wallet.emit(WalletPageMsg::FeeSuggestion(rate, source));
            }
            AppCmd::Estimated(Err(message)) => {
                // No suggestion is a fine outcome: the field keeps its floor
                // and whatever was typed into it.
                tracing::warn!(%message, "could not estimate a fee rate");
            }
            AppCmd::Planned(result) => {
                self.wallet.emit(WalletPageMsg::Planned(Box::new(result)));
            }
            AppCmd::PlannedBump(result) => {
                self.wallet.emit(WalletPageMsg::BumpPlanned(Box::new(
                    result.map(|plan| *plan),
                )));
            }

            AppCmd::Sent(Ok((txid, summary))) => {
                // A replacement is the same payment to the same person for the
                // same reason, so it carries the same name. Written to the new
                // id *as well as* the old rather than moved: either of them can
                // still be the one that confirms, and moving it would leave
                // whichever wins unlabelled.
                let replaced = self.bumping.take();
                if let Some(replaced) = &replaced
                    && let Some(paths) = self.active.clone()
                {
                    let mut labels = wallet::labels::Labels::load(&paths.dir);
                    if let Some(name) = labels
                        .get(wallet::labels::Kind::Tx, replaced)
                        .map(str::to_owned)
                    {
                        labels.set(wallet::labels::Kind::Tx, &txid, &name);
                        if let Err(e) = labels.save(&paths.dir) {
                            tracing::warn!(%e, "could not carry the label to the replacement");
                        } else {
                            self.wallet.emit(WalletPageMsg::SetLabels(Box::new(labels)));
                        }
                    }
                }

                // The wallet has already recorded it as pending, so the
                // activity list shows the payment straight away rather than
                // waiting for a block.
                self.wallet.emit(WalletPageMsg::Show(summary));

                match replaced {
                    // A replacement leaves the page it was started from
                    // showing a payment that no longer exists. Saying so and
                    // going to the one that does is the difference between
                    // "it worked" and a toast over a stale screen.
                    Some(_) => self.wallet.emit(WalletPageMsg::Replaced { with: txid }),
                    None => self.wallet.emit(WalletPageMsg::Sent(Box::new(Ok(txid)))),
                }
            }
            AppCmd::Sent(Err(message)) => {
                self.wallet
                    .emit(WalletPageMsg::Sent(Box::new(Err(message))));
            }
            AppCmd::Revealed {
                generation,
                result: Ok((address, summary)),
            } if self.current(generation) => {
                self.wallet.emit(WalletPageMsg::Show(summary));
                self.wallet.emit(WalletPageMsg::ShowFreshAddress(address));
            }
            AppCmd::Priced(Ok(price)) => {
                tracing::debug!(usd = price.usd, "price fetched");
                self.wallet.emit(WalletPageMsg::SetPrice(Some(price)));
            }
            AppCmd::Priced(Err(message)) => {
                // A missing price is not a wallet problem: the balance in
                // bitcoin is the real number and is already on screen.
                tracing::warn!(%message, "could not fetch a price");
                self.wallet.emit(WalletPageMsg::SetPrice(None));
            }
            AppCmd::Revealed {
                generation,
                result: Err(message),
            } if self.current(generation) => {
                tracing::error!(%message, "could not reveal an address");
                self.wallet.emit(WalletPageMsg::Failed(message));
            }

            // Everything the guards above rejected: a result for a wallet that
            // is no longer the one on screen. Dropped, and said once at debug
            // so it is visible when chasing a screen that looks stale.
            AppCmd::Started { .. }
            | AppCmd::Update { .. }
            | AppCmd::Progress { .. }
            | AppCmd::Warning { .. }
            | AppCmd::Chain { .. }
            | AppCmd::Peers { .. }
            | AppCmd::Revealed { .. } => {
                tracing::debug!("ignoring a result from a wallet that is no longer open");
            }
        }
    }

    /// Take the Tor we started down with us.
    ///
    /// Belt and braces: Tor is also started with `__OwningControllerProcess`,
    /// so it exits by itself if Sieve is killed and never reaches this.
    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        crate::tor::daemon::stop();
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

/// Follow the desktop's colour scheme. There is no other option, on purpose.
///
/// Light and dark belong to the desktop, not to one application: an app with
/// its own switch is an app that can disagree with everything around it. It
/// also made a real mess here — choosing Light under a dark desktop theme put
/// that theme's dark surfaces under libadwaita's light text, which is a class
/// of bug that a preference invites and its absence forbids.
///
/// GNOME's own setting is read directly rather than left to
/// `ColorScheme::Default`, because the settings portal is not reachable in
/// every session and this app has met one where it is not. Where GNOME's
/// schema says nothing — a KDE session, say, which publishes its choice
/// through the portal instead — `Default` is exactly right, and libadwaita
/// asks the portal itself.
fn follow_desktop_scheme(style: &adw::StyleManager) {
    let scheme = match desktop_interface_settings()
        .map(|s| s.string("color-scheme").to_string())
        .as_deref()
    {
        Some("prefer-dark") => adw::ColorScheme::PreferDark,
        Some("prefer-light") => adw::ColorScheme::PreferLight,
        // No opinion from GNOME's schema, so none from us: whatever the
        // portal says, or light if nothing says anything.
        _ => adw::ColorScheme::Default,
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
    /// Ask again shortly whether the wallet has been left alone.
    ///
    /// Polling rather than a timer armed on each keystroke: input is
    /// continuous, and rebuilding a timer per event costs more than a check
    /// every fifteen seconds that almost always says no. The cost of the
    /// coarseness is that the lock can be up to that late, which nobody can
    /// perceive against a five-minute setting.
    fn watch_for_idle(&self, sender: &ComponentSender<Self>) {
        if self.settings.idle_lock.duration().is_none() {
            return;
        }
        sender.oneshot_command(async move {
            tokio::time::sleep(IDLE_CHECK).await;
            AppCmd::Idle
        });
    }

    fn schedule_tick(&self, sender: &ComponentSender<Self>) {
        if self.session.is_none() {
            return;
        }
        sender.oneshot_command(async move {
            tokio::time::sleep(TICK).await;
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

        let proxy = self.tor_proxy();
        sender.spawn_oneshot_command(move || {
            AppCmd::Priced(crate::price::fetch(proxy).map_err(|e| e.to_string()))
        });
    }

    /// The proxy every outbound connection goes through, if any.
    ///
    /// One reader for the setting, so no call site can forget it: peers, the
    /// price and the fee rates all ask here.
    fn tor_proxy(&self) -> Option<crate::tor::Proxy> {
        self.settings.tor.then_some(self.tor_active).flatten()
    }

    /// The proxy the settings name, when they name one.
    ///
    /// An address typed in preferences is taken as an instruction: use that
    /// one, do not go starting anything.
    fn configured_proxy(&self) -> Option<crate::tor::Proxy> {
        let text = self.settings.tor_proxy.as_deref()?;
        match text.parse() {
            Ok(proxy) => Some(proxy),
            Err(e) => {
                tracing::warn!(%e, "unreadable proxy address in settings");
                None
            }
        }
    }

    /// Remember how far the scan has checked filters.
    ///
    /// bdk_kyoto only produces a wallet update when the whole filter sync
    /// finishes, so an hour of scanning leaves no trace and the next start
    /// begins at the birthday again. This is that trace.
    ///
    /// Deliberately behind the truth. kyoto reports a fraction, not a height,
    /// and a resume point past where the scan actually reached would skip
    /// blocks — which is missing money, not lost time. So the figure is
    /// derived from the fraction and then pulled back by a wide margin.
    fn record_scan_progress(&mut self, progress: &Progress, sender: &ComponentSender<Self>) {
        // A finished scan is exact: everything up to the tip has been checked,
        // so there is no estimate to be careful about.
        if matches!(progress, Progress::Synced)
            && let (Some(paths), Some(tip)) = (self.active.clone(), self.chain_tip)
        {
            self.pin_resume_point(paths, tip, sender);
            return;
        }

        let Progress::Scanning(fraction) = progress else {
            return;
        };
        let (Some(paths), Some(tip)) = (self.active.clone(), self.chain_tip) else {
            return;
        };
        let Some(meta) = wallet::Meta::load(&paths) else {
            return;
        };

        // Filter headers are the first quarter of kyoto's figure; the filters
        // themselves are the rest, and they are checked in order from the scan
        // start.
        let share = crate::wallet::node::FILTER_HEADER_SHARE;
        if *fraction <= share {
            return;
        }
        let done = (fraction - share) / (1.0 - share);
        let from = meta.scanned_to.unwrap_or(meta.birthday_height);
        if tip <= from {
            return;
        }

        let reached = from + (done * f64::from(tip - from)) as u32;
        let Some(safe) = reached.checked_sub(SCAN_MARGIN) else {
            return;
        };
        if safe <= from {
            return;
        }

        // Written at most once a minute: this rewrites the metadata file.
        let due = self
            .scan_recorded
            .map(|last| last.elapsed() >= std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.scan_recorded = Some(std::time::Instant::now());
        self.pin_resume_point(paths, safe, sender);
    }

    /// Write down a resume point: a height, and the hash that pins it.
    ///
    /// The hash comes from the node's own memory. Without it the height is
    /// useless — a checkpoint is both, and the node will not take one half.
    fn pin_resume_point(&self, paths: Paths, height: u32, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        sender.oneshot_command(async move {
            if let Some(hash) = session.header_hash(height).await {
                tracing::info!(height, "recording how far the scan has checked");
                wallet::Meta::record_scanned_to(&paths, height, &hash.to_string());
            }
            AppCmd::Tick
        });
    }

    /// Ask the node who is connected.
    ///
    /// Throttled: the connection warning fires continuously while the node is
    /// below its target, and rebuilding the peer list on each one pegged the
    /// main thread with widget churn.
    fn read_peers(&mut self, sender: &ComponentSender<Self>) {
        let due = self
            .peers_read
            .map(|last| last.elapsed() >= PEER_REFRESH)
            .unwrap_or(true);
        if !due {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        self.peers_read = Some(std::time::Instant::now());
        let generation = self.generation;
        sender.oneshot_command(async move {
            AppCmd::Peers {
                generation,
                peers: session.peers().await,
            }
        });
    }

    /// Notice if the Tor we started has gone away.
    ///
    /// Without this the node keeps trying to reach peers through a proxy that
    /// is not there — which it does as fast as the failures come back, at the
    /// cost of a whole CPU. Learned the hard way.
    fn check_tor(&mut self, sender: &ComponentSender<Self>) {
        if !self.settings.tor || self.tor_active.is_none() {
            return;
        }
        // A daemon we merely borrowed is not ours to poll or to restart.
        if !crate::tor::daemon::is_ours() || crate::tor::daemon::ours_is_alive() {
            return;
        }

        tracing::warn!("the Tor we started has exited; stopping the light client");
        self.tor_active = None;
        if let Some(session) = self.session.take() {
            session.shutdown();
            self.wallet.emit(WalletPageMsg::Reset);
        }
        self.tor_status = Some("Tor stopped. Starting it again…".into());
        self.refresh_tor_row();
        self.ensure_tor(sender);
    }

    /// Write the unit into the Amounts row.
    ///
    /// In place, for the same reason as the Tor row: rebuilding the page under
    /// somebody who has just tapped a row throws them back to the top of it.
    fn refresh_amounts_row(&self) {
        let Some((row, unit)) = &self.amounts_row else {
            return;
        };
        row.set_subtitle(match self.settings.denomination {
            crate::settings::Denomination::Sats => "Satoshis",
            crate::settings::Denomination::Btc => "Decimal BTC",
        });
        let network = self
            .active
            .as_ref()
            .and_then(wallet::Meta::load)
            .map(|m| m.network)
            .unwrap_or_else(|| "bitcoin".into());
        unit.set_label(self.settings.denomination.label(&network));
    }

    /// Write the current Tor state into the rows that show it.
    ///
    /// In place, rather than rebuilding the preferences page: the page is
    /// inside a scrolled window, and replacing it while someone is reading
    /// the Connection group sends them back to the top of Display.
    fn refresh_tor_row(&self) {
        if let Some(row) = &self.tor_row {
            row.set_subtitle(&self.tor_subtitle());
            if self.tor_failed {
                row.add_css_class("error");
            } else {
                row.remove_css_class("error");
            }
        }

        // The switch may have moved by itself — a failure turns it back — and
        // setting it must not look like someone flipping it.
        if let Some((switch, handler)) = &self.tor_switch
            && switch.is_active() != self.settings.tor
        {
            switch.block_signal(handler);
            switch.set_active(self.settings.tor);
            switch.unblock_signal(handler);
        }

        self.wallet.emit(WalletPageMsg::SetTor(self.tor_label()));
    }

    /// What the Connection row says underneath.
    fn tor_subtitle(&self) -> String {
        match self.tor_status.as_deref() {
            Some(status) => status.to_string(),
            // Only the filesystem is consulted here — the main thread must not
            // go opening sockets to find out.
            None if crate::tor::daemon::find_binary().is_some() => {
                "Tor is on this machine. Sieve will start it when you switch this on.".into()
            }
            None => "No Tor found on this machine. Install it — on Arch, `sudo pacman -S tor` \
                     — or use a packaged build of Sieve, which carries its own."
                .into(),
        }
    }

    /// How the network view describes the connection.
    fn tor_label(&self) -> Option<String> {
        let proxy = self.tor_proxy()?;
        Some(if crate::tor::daemon::is_ours() {
            format!("Through Tor, started by Sieve · {proxy}")
        } else {
            format!("Through Tor · {proxy}")
        })
    }

    /// Bring Tor up: use what is listening, or start one.
    ///
    /// Slow — a first bootstrap can take half a minute — so it reports as it
    /// goes rather than leaving a switch mid-flip with nothing to show.
    fn ensure_tor_asked(&mut self, sender: &ComponentSender<Self>) {
        self.tor_asked_for = true;
        self.ensure_tor(sender);
    }

    fn ensure_tor(&mut self, sender: &ComponentSender<Self>) {
        self.tor_failed = false;
        self.tor_status = Some("Starting Tor…".into());
        self.refresh_tor_row();

        let configured = self.configured_proxy();
        let asked_for = std::mem::take(&mut self.tor_asked_for);
        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    // An address given in preferences is used as given: it may
                    // be a proxy on another machine, and starting a local Tor
                    // would silently ignore what was asked for.
                    if let Some(proxy) = configured {
                        let result = tokio::task::spawn_blocking(move || {
                            crate::tor::check(proxy)
                                .map(|_| proxy)
                                .map_err(|e| e.to_string())
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                        let _ = out.send(AppCmd::TorReady { asked_for, result });
                        return;
                    }

                    let (progress, mut updates) = tokio::sync::mpsc::unbounded_channel();
                    let work = tokio::task::spawn_blocking(move || {
                        crate::tor::daemon::ensure(|message| {
                            let _ = progress.send(message);
                        })
                        .map_err(|e| e.to_string())
                    });

                    while let Some(message) = updates.recv().await {
                        let _ = out.send(AppCmd::TorProgress(message));
                    }

                    let result = work.await.unwrap_or_else(|e| Err(e.to_string()));
                    let _ = out.send(AppCmd::TorReady { asked_for, result });
                })
                .drop_on_shutdown()
        });
    }

    /// Start the light client, once everything it depends on is ready.
    fn start_session(&mut self, sender: &ComponentSender<Self>) {
        if self.session.is_some() {
            return;
        }
        let Some(paths) = self.active.clone() else {
            return;
        };
        if !self.unlocked {
            return;
        }

        // Tor first, always. Connecting over the clear while the switch says
        // Tor is the one outcome this must never produce.
        if self.settings.tor && self.tor_active.is_none() {
            self.ensure_tor(sender);
            return;
        }

        // A new session, so anything still in flight for the last one is
        // from a wallet that is no longer on screen.
        self.generation += 1;
        self.blocks_recorded = false;
        let generation = self.generation;
        let tor = self.tor_proxy();
        sender.oneshot_command(async move {
            AppCmd::Started {
                generation,
                result: Session::start(&paths, tor)
                    .await
                    .map(Arc::new)
                    .map_err(|e| e.to_string()),
            }
        });
    }

    /// Stop the light client and start another with the current settings.
    ///
    /// Turning Tor on or off changes how every connection is made, and a node
    /// already talking to peers over the clear cannot be converted in place.
    fn restart_session(&mut self, sender: &ComponentSender<Self>) {
        if let Some(session) = self.session.take() {
            tracing::info!("restarting the light client with new connection settings");
            session.shutdown();
            self.wallet.emit(WalletPageMsg::Reset);
            self.restate_wallet(sender);
        }
        self.start_session(sender);
    }

    /// Write every label to a file in BIP-329's format.
    fn export_labels(&self, paths: &Paths, to: &std::path::Path) -> anyhow::Result<String> {
        let labels = wallet::labels::Labels::load(&paths.dir);
        let count = labels.len();
        std::fs::write(to, labels.to_jsonl()?)
            .map_err(|e| anyhow::anyhow!("could not write {}: {e}", to.display()))?;
        Ok(match count {
            1 => "Exported 1 label".to_string(),
            n => format!("Exported {n} labels"),
        })
    }

    /// Merge a BIP-329 file into this wallet's labels.
    fn import_labels(&self, paths: &Paths, from: &std::path::Path) -> anyhow::Result<String> {
        let text = std::fs::read_to_string(from)
            .map_err(|e| anyhow::anyhow!("could not read {}: {e}", from.display()))?;
        let mut labels = wallet::labels::Labels::load(&paths.dir);
        let read = labels.import(&text)?;
        labels.save(&paths.dir)?;
        self.wallet.emit(WalletPageMsg::SetLabels(Box::new(labels)));
        Ok(match read {
            0 => "That file held no labels".to_string(),
            1 => "Imported 1 label".to_string(),
            n => format!("Imported {n} labels"),
        })
    }

    /// Put back what `Reset` cleared but a scan will not replace.
    ///
    /// The birthday and the network come from metadata and are known long
    /// before any summary exists. Without them the sync view cannot say how
    /// big the job is or how far along it is, and falls back to a spinner for
    /// the whole of it.
    fn restate_wallet(&self, sender: &ComponentSender<Self>) {
        let Some(paths) = &self.active else { return };
        let Some(meta) = wallet::Meta::load(paths) else {
            return;
        };
        self.wallet
            .emit(WalletPageMsg::SetBirthday(meta.birthday_height));
        self.wallet
            .emit(WalletPageMsg::SetNetwork(meta.network.clone()));
        self.wallet
            .emit(WalletPageMsg::SetMatchedBlocks(meta.matched_blocks));
        self.wallet
            .emit(WalletPageMsg::SetWatchOnly(meta.watch_only));
        self.wallet.emit(WalletPageMsg::SetLabels(Box::new(
            wallet::labels::Labels::load(&paths.dir),
        )));
        // The price goes with the page, and the page was just cleared. Nothing
        // else asks for it again, so a restarted session lost the dollar
        // figure until the setting was toggled off and on.
        self.fetch_price(sender);
        let id = paths
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.wallet
            .emit(WalletPageMsg::SetName(meta.display_name(&id)));
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
            self.wallet.emit(WalletPageMsg::SetAskingToUnlock(false));
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
        self.amounts_row = Some((amounts.clone(), unit.clone()));
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

        // Not a choice, a statement. Light and dark belong to the desktop, and
        // Sieve has no switch for them — but "the app ignores my theme" and
        // "the desktop is not saying anything" look identical from here, and
        // they have very different fixes, so this says which one is happening.
        let appearance = adw::ActionRow::new();
        appearance.set_title("Appearance");
        let palette = crate::palette::desktop();
        appearance.set_subtitle(&match (&palette, adw::StyleManager::default().is_dark()) {
            (Some(p), _) => format!("Following this desktop's theme, accent {}", p.accent),
            (None, true) => "Following the desktop — dark".into(),
            (None, false) => "Following the desktop — light".into(),
        });
        appearance.set_subtitle_lines(2);
        display.add(&appearance);

        page.add(&display);

        // Its own group rather than an entry under Display: this is not about
        // how the wallet looks. Hidden while locked, because every row on it
        // describes when to shut a wallet that is already shut.
        let privacy = adw::PreferencesGroup::new();
        if self.unlocked {
            privacy.set_title("Locking");

            let idle = adw::ComboRow::new();
            idle.set_title("Lock when untouched");
            // On the row rather than on the group: a paragraph in a group
            // description sits above every row in it and pushes the controls off
            // the screen. What is worth saying fits in a sentence.
            idle.set_subtitle(
                "Shuts the balance and history. Your recovery phrase is sealed either way — it is \
             only ever decrypted at the moment of signing.",
            );
            idle.set_subtitle_lines(3);
            idle.set_model(Some(&gtk::StringList::new(
                &crate::settings::IdleLock::ALL.map(|i| i.label()),
            )));
            idle.set_selected(
                crate::settings::IdleLock::ALL
                    .iter()
                    .position(|i| *i == self.settings.idle_lock)
                    .unwrap_or(0) as u32,
            );
            // Connected after the initial selection, so setting it does not fire.
            {
                let sender = sender.clone();
                idle.connect_selected_notify(move |row| {
                    if let Some(choice) =
                        crate::settings::IdleLock::ALL.get(row.selected() as usize)
                    {
                        sender.input(AppMsg::SetIdleLock(*choice));
                    }
                });
            }
            privacy.add(&idle);

            // The same thing, now, without waiting.
            let now = adw::ActionRow::new();
            now.set_title("Lock now");
            now.set_subtitle("Shut the wallet and ask for the password again");
            now.set_activatable(true);
            now.set_sensitive(self.unlocked);
            now.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            {
                let sender = sender.clone();
                now.connect_activated(move |_| sender.input(AppMsg::Lock(LockReason::Asked)));
            }
            privacy.add(&now);
            page.add(&privacy);
        }

        // Connections first among the privacy settings: it changes what every
        // other one discloses.
        let connection = adw::PreferencesGroup::new();
        connection.set_title("Connection");
        connection.set_description(Some(
            "Sieve already hides which addresses are yours — compact block filters mean no \
             server is ever told. What a peer still sees is your IP address, and when you \
             broadcast a payment, that it came from you.",
        ));

        let tor = adw::SwitchRow::new();
        tor.set_title("Route connections through Tor");
        tor.set_subtitle(
            "Peers, price and fee lookups all go through Tor. Sieve uses a Tor already \
             running on this machine, and starts one itself if there is none. If Tor cannot \
             be reached at all, Sieve refuses to connect rather than going out over the \
             clear.",
        );
        tor.set_active(self.settings.tor);
        // Readable while locked, not settable. The node keeps syncing behind
        // the lock, so turning Tor off at an unattended machine would put that
        // sync on the clear and hand a peer the IP address — a real change to
        // how this wallet reaches the network, made without the password.
        // Everything else on this page only describes the app.
        tor.set_sensitive(self.unlocked);
        if !self.unlocked {
            tor.set_subtitle(
                "Unlock the wallet to change this. It is left as it is because the wallet \
                 keeps syncing while locked, and switching Tor off would put that on the \
                 clear.",
            );
            tor.set_subtitle_lines(3);
        }
        let toggled = {
            let sender = sender.clone();
            tor.connect_active_notify(move |row| {
                sender.input(AppMsg::SetTor(row.is_active()));
            })
        };
        self.tor_switch = Some((tor.clone(), toggled));
        connection.add(&tor);

        // Always shown, because the useful case is the one where Tor is off:
        // saying up front that there is no Tor on this machine beats letting
        // the switch flip back and leaving someone to guess why.
        {
            let status = adw::ActionRow::new();
            status.set_title("Proxy");
            status.set_subtitle(&self.tor_subtitle());
            status.set_subtitle_lines(4);
            if self.tor_failed {
                status.add_css_class("error");
            }
            self.tor_row = Some(status.clone());

            let check = gtk::Button::with_label("Check");
            check.set_valign(gtk::Align::Center);
            check.add_css_class("flat");
            {
                let sender = sender.clone();
                check.connect_clicked(move |_| sender.input(AppMsg::CheckTor));
            }
            status.add_suffix(&check);
            connection.add(&status);

            let address = adw::EntryRow::new();
            address.set_title("Proxy address");
            // Same rule as the switch above: where Sieve connects is not
            // changed from behind the lock.
            address.set_sensitive(self.unlocked);
            address.set_text(
                self.settings
                    .tor_proxy
                    .as_deref()
                    .unwrap_or(&crate::tor::Proxy::local(crate::tor::PORTS[0]).to_string()),
            );
            address.set_show_apply_button(true);
            {
                let sender = sender.clone();
                address.connect_apply(move |row| {
                    sender.input(AppMsg::SetTorProxy(row.text().to_string()));
                });
            }
            connection.add(&address);
        }

        page.add(&connection);

        let sending = adw::PreferencesGroup::new();
        sending.set_title("Fees");
        sending.set_description(Some(
            "By default Sieve reads the average fee from the last block it downloaded, \
             which tells nobody anything.",
        ));

        let mempool = adw::SwitchRow::new();
        mempool.set_title("Fee rates from mempool.space");
        mempool.set_sensitive(self.unlocked);
        mempool.set_subtitle(
            "A better estimate, bought with a disclosure. It sends no wallet data, but asking \
             for fee rates tells the server your IP address and that you are about to send a \
             payment.",
        );
        mempool.set_active(self.settings.mempool_fees);
        {
            let sender = sender.clone();
            mempool.connect_active_notify(move |row| {
                sender.input(AppMsg::SetMempoolFees(row.is_active()));
            });
        }
        sending.add(&mempool);
        page.add(&sending);

        // Labels are written in BIP-329's format precisely so they can leave.
        // A wallet that holds your notes hostage is a wallet you cannot
        // replace, which is the opposite of what a recovery phrase is for.
        //
        // Behind the lock, though: a label is the name somebody gave their own
        // payment, and exporting them all is reading the wallet.
        if let Some(paths) = self.active.clone().filter(|_| self.unlocked) {
            let labels = adw::PreferencesGroup::new();
            labels.set_title("Labels");
            let count = wallet::labels::Labels::load(&paths.dir).len();
            labels.set_description(Some(&match count {
                0 => "Names you give transactions and addresses are stored beside this wallet, unencrypted, readable only by you."
                    .to_string(),
                1 => "1 label, stored beside this wallet, unencrypted and readable only by you."
                    .to_string(),
                n => format!(
                    "{n} labels, stored beside this wallet, unencrypted and readable only by you."
                ),
            }));

            let export = adw::ActionRow::new();
            export.set_title("Export labels…");
            export.set_subtitle("A BIP-329 file any wallet that reads the standard can import");
            export.set_subtitle_lines(2);
            export.set_activatable(true);
            export.set_sensitive(count > 0);
            export.add_suffix(&gtk::Image::from_icon_name("document-save-symbolic"));
            {
                let sender = sender.clone();
                export.connect_activated(move |_| sender.input(AppMsg::ExportLabels));
            }
            labels.add(&export);

            let import = adw::ActionRow::new();
            import.set_title("Import labels…");
            import.set_subtitle(
                "Merges a BIP-329 file into this wallet. Where both name the same thing, the imported name wins.",
            );
            import.set_subtitle_lines(3);
            import.set_activatable(true);
            import.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
            {
                let sender = sender.clone();
                import.connect_activated(move |_| sender.input(AppMsg::ImportLabels));
            }
            labels.add(&import);
            page.add(&labels);
        }

        let this = adw::PreferencesGroup::new();
        // Titled only when there is something under it. "This wallet" over a
        // single row saying the wallet is locked is a heading for a section
        // that is not there.
        if self.unlocked {
            this.set_title("This wallet");
        }

        // What is on this page divides cleanly: how Sieve looks and how it
        // connects belong to the app and are nobody's secret, but a wallet's
        // name, its phrase and its removal belong to the wallet — and the
        // wallet is shut. Switching to another one stays available, since that
        // is how you get somewhere you can act.
        if self.active.is_some() && !self.unlocked {
            let shut = adw::ActionRow::new();
            shut.set_title("Locked");
            shut.set_subtitle(
                "Unlock this wallet to rename it, see its recovery phrase, export its \
                 labels, or remove it.",
            );
            shut.set_subtitle_lines(3);
            shut.set_activatable(true);
            shut.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            {
                let sender = sender.clone();
                shut.connect_activated(move |_| sender.input(AppMsg::PromptUnlock));
            }
            this.add(&shut);
            page.add(&this);
            // Replace whatever was there, so reopening never stacks pages.
            if let Some(existing) = self.prefs_page.take() {
                self.prefs.remove(&existing);
            }
            self.prefs.add(&page);
            self.prefs_page = Some(page);
            return;
        }

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

        // The phrase is shown once, when the wallet is made, and the moment
        // someone is most likely to put off writing twelve words down is
        // exactly that one. This is the way back to it.
        if self.active.is_some() {
            let phrase = adw::ActionRow::new();
            phrase.set_title("Recovery phrase");
            let watch_only = self
                .active
                .as_ref()
                .and_then(wallet::Meta::load)
                .is_some_and(|m| m.watch_only);
            phrase.set_subtitle(match (watch_only, self.unlocked) {
                // Nothing was ever sealed here, so there is nothing to open.
                (true, _) => {
                    "This wallet holds no keys — its recovery phrase lives wherever the keys do"
                }
                (false, true) => "Show the words again to write them down",
                (false, false) => "Unlock this wallet to show the words",
            });
            phrase.set_subtitle_lines(2);
            let can_reveal = self.unlocked && !watch_only;
            phrase.set_activatable(can_reveal);
            phrase.set_sensitive(can_reveal);
            phrase.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            {
                let sender = sender.clone();
                phrase.connect_activated(move |_| sender.input(AppMsg::ShowRecoveryPhrase));
            }
            this.add(&phrase);

            // A wallet with no keys still has a balance and a history, and
            // until now nothing shut them: opening Sieve showed everything.
            // The password gates the wallet inside the app; it does not
            // encrypt anything, and the subtitle says which of those it is.
            if watch_only && let Some(paths) = self.active.clone() {
                let locked = paths.lock.exists();
                let lock = adw::ActionRow::new();
                lock.set_title(if locked { "Password" } else { "Set a password" });
                lock.set_subtitle(if locked {
                    "Asked for when this wallet is opened. It does not encrypt the files on \
                     disk — there are no keys here to protect, only what is on screen."
                } else {
                    "This wallet opens without asking. A password would shut its balance and \
                     history to somebody at this machine — the files on disk stay readable."
                });
                lock.set_subtitle_lines(3);
                lock.set_activatable(true);
                lock.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
                {
                    let sender = sender.clone();
                    lock.connect_activated(move |_| sender.input(AppMsg::AskWatchOnlyPassword));
                }
                this.add(&lock);
            }
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

        // Last in the group, and the only destructive thing in preferences.
        if self.active.is_some() {
            let remove = adw::ActionRow::new();
            remove.set_title("Remove this wallet");
            remove.set_subtitle(
                "Deletes it from this computer. Your coins stay on the network, and only \
                 the recovery phrase can reach them again.",
            );
            remove.set_subtitle_lines(3);

            let button = gtk::Button::with_label("Remove…");
            button.set_valign(gtk::Align::Center);
            // Adwaita's own destructive styling, so it reads the same as every
            // other irreversible button in GNOME.
            button.add_css_class("destructive-action");
            {
                let sender = sender.clone();
                button.connect_clicked(move |_| sender.input(AppMsg::AskRemoveWallet));
            }
            remove.add_suffix(&button);
            this.add(&remove);
        }

        page.add(&this);

        // Replace whatever was there, so reopening never stacks pages.
        if let Some(existing) = self.prefs_page.take() {
            self.prefs.remove(&existing);
        }
        self.prefs.add(&page);
        self.prefs_page = Some(page);
    }

    /// Is this result from the session currently on screen?
    ///
    /// Anything older belongs to a wallet that has been left, and applying it
    /// would put one wallet's numbers under another's name.
    fn current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    fn await_update(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let generation = self.generation;
        sender.oneshot_command(async move {
            AppCmd::Update {
                generation,
                result: session.next_update().await.map_err(|e| e.to_string()),
            }
        });
    }

    fn await_progress(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let generation = self.generation;
        sender.oneshot_command(async move {
            AppCmd::Progress {
                generation,
                progress: session.next_progress().await,
            }
        });
    }

    fn await_warning(&self, sender: &ComponentSender<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let generation = self.generation;
        sender.oneshot_command(async move {
            AppCmd::Warning {
                generation,
                notice: session.next_warning().await,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::IdleLock;
    use std::time::Duration;

    #[test]
    fn an_untouched_wallet_locks_once_its_interval_passes() {
        let five = Duration::from_secs(5 * 60);

        // The ordinary case, and the one that cannot be reached from a test
        // through the interface: a wallet somebody unlocked and walked away
        // from.
        assert!(!should_lock(
            IdleLock::After5Minutes,
            true,
            five - Duration::from_secs(1)
        ));
        assert!(should_lock(IdleLock::After5Minutes, true, five));
        assert!(should_lock(
            IdleLock::After5Minutes,
            true,
            Duration::from_secs(3600)
        ));

        // Never means never, however long it has been.
        assert!(!should_lock(
            IdleLock::Never,
            true,
            Duration::from_secs(86_400)
        ));

        // And a wallet that is already shut is not shut again — that would
        // toast "Locked" at somebody staring at a password prompt.
        assert!(!should_lock(
            IdleLock::After5Minutes,
            false,
            Duration::from_secs(86_400)
        ));
    }
}
