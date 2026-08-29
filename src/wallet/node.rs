//! The compact block filter light client.
//!
//! A `bdk_kyoto` node runs on the shared tokio runtime and gathers transactions
//! by downloading BIP158 filters and matching them locally. No server is told
//! which scripts belong to this wallet — that is the whole point of the design,
//! and the reason this crate exists rather than an Electrum client.
//!
//! Ownership: the wallet and its database connection live behind a mutex here,
//! because updates arrive continuously and each one has to be applied and
//! persisted. Nothing in this module touches GTK; the UI drives it by awaiting
//! `next_update` and `next_info` from relm4 commands.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bdk_kyoto::builder::{Builder, BuilderExt};
use bdk_kyoto::{Info, LightClient, ScanType, UpdateSubscriber, wallets::Single};
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};
use tokio::sync::Mutex as AsyncMutex;

use bdk_kyoto::bip157::HashCheckpoint;

use super::{Birthday, NETWORK, Paths, Summary, restrict};

/// How many peers to hold open.
///
/// Filters are fetched in parallel across peers, so this is the main lever on
/// sync speed. It is set high relative to a normal wallet because the peers
/// that serve `NODE_COMPACT_FILTERS` are a small minority of the network — most
/// nodes do not run `-blockfilterindex=1` — and connection attempts to peers
/// that turn out not to serve filters are common.
const REQUIRED_PEERS: u8 = 4;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
/// If the node says nothing for this long, tell the user so rather than
/// leaving a spinner turning against a frozen label.
const QUIET_BEFORE_WAITING: Duration = Duration::from_secs(20);

/// Group digits so six-figure block heights stay readable.
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Sync progress, as the UI wants to render it.
#[derive(Debug, Clone)]
pub enum Progress {
    Connecting,
    Connected,
    /// Block headers still coming in. Carries the chain height reached so far.
    ///
    /// This phase has no meaningful fraction — the node does not know the tip
    /// until it gets there — so it reports height rather than a percentage.
    Headers(u32),
    /// Filters downloading, 0.0 to 1.0.
    Scanning(f64),
    Synced,
    /// The node has been silent long enough to be worth mentioning.
    Waiting,
}

/// Something from the node worth putting in front of a person, as distinct
/// from progress.
#[derive(Debug, Clone)]
pub enum Notice {
    /// Routine connection bookkeeping — a count, not a problem.
    Peers { connected: usize, required: usize },
    /// Worth showing, and worth clearing once sync moves again.
    Problem(String),
    /// Chatter the user cannot act on.
    Ignorable,
}

impl Progress {
    pub fn label(&self) -> String {
        match self {
            Progress::Connecting => "Looking for peers…".into(),
            Progress::Connected => "Connected to peers".into(),
            Progress::Headers(height) => {
                format!("Downloading block headers — {} blocks", thousands(*height))
            }
            // Two decimals: on a chain of this size one decimal would sit
            // still for thousands of filters and read as frozen.
            Progress::Scanning(f) => format!("Scanning block filters — {:.2}%", f * 100.0),
            Progress::Synced => "Up to date".into(),
            Progress::Waiting => "Waiting for peers to respond…".into(),
        }
    }

    /// `None` means indeterminate, and the bar should pulse rather than fill.
    pub fn fraction(&self) -> Option<f64> {
        match self {
            Progress::Scanning(f) => Some(*f),
            Progress::Synced => Some(1.0),
            _ => None,
        }
    }
}

/// A running light client bound to one wallet.
pub struct Session {
    inner: Arc<AsyncMutex<Inner>>,
    updates: Arc<AsyncMutex<UpdateSubscriber<Single>>>,
    info: Arc<AsyncMutex<bdk_kyoto::Receiver<Info>>>,
    warnings: Arc<AsyncMutex<bdk_kyoto::UnboundedReceiver<bdk_kyoto::Warning>>>,
    requester: bdk_kyoto::Requester,
    /// Once the first sync lands, silence from the node is normal rather than
    /// a symptom, and must not be reported as waiting.
    synced: Arc<AtomicBool>,
}

struct Inner {
    wallet: PersistedWallet<Connection>,
    conn: Connection,
}

impl Session {
    /// Load the wallet, start the node, and begin fetching.
    ///
    /// Must be called from inside the tokio runtime — the node is spawned onto
    /// it. Relm4's async commands satisfy that.
    pub async fn start(paths: &Paths) -> Result<Self> {
        let mut conn = Connection::open(&paths.db)?;
        restrict(&paths.db)?;
        let wallet: PersistedWallet<Connection> = Wallet::load()
            .check_network(NETWORK)
            .load_wallet(&mut conn)
            .map_err(|e| anyhow!("could not load the wallet database: {e}"))?
            .context("the wallet database is empty — unlock first")?;

        let headers = paths.db.parent().unwrap_or(&paths.db).join("headers");
        std::fs::create_dir_all(&headers)?;

        // A wallet that has never synced sits at the genesis checkpoint, so
        // `ScanType::Sync` would walk the entire chain. If we recorded a
        // birthday when the wallet was created, start there instead.
        let scan_type = match (wallet.latest_checkpoint().height(), Birthday::load(paths)) {
            (0, Some(birthday)) => match birthday.hash.parse() {
                Ok(hash) => {
                    tracing::info!(height = birthday.height, "scanning from the wallet birthday");
                    ScanType::Recovery {
                        // A floor, not just the current index: recovery peeks
                        // this many scripts when testing filters, and a fresh
                        // wallet reporting 0 would check almost nothing.
                        used_script_index: wallet
                            .derivation_index(KeychainKind::External)
                            .unwrap_or(0)
                            .max(25),
                        checkpoint: HashCheckpoint::new(birthday.height, hash),
                    }
                }
                Err(e) => {
                    tracing::warn!(%e, "birthday hash is unreadable; scanning from genesis");
                    ScanType::Sync
                }
            },
            (0, None) => {
                tracing::warn!("no birthday recorded; scanning from genesis");
                ScanType::Sync
            }
            _ => ScanType::Sync,
        };

        let client: LightClient<_, Single> = Builder::new(NETWORK)
            .required_peers(REQUIRED_PEERS)
            .data_dir(headers)
            .response_timeout(RESPONSE_TIMEOUT)
            .build_with_wallet(&wallet, scan_type)
            .map_err(|e| anyhow!("could not build the light client: {e}"))?;

        let (client, logging, updates) = client.subscribe();
        // `managed_start` hands back the node so it is spawned explicitly on
        // relm4's runtime rather than whichever runtime happens to be current.
        let (client, node) = client.managed_start();
        relm4::spawn(async move { node.run().await });

        Ok(Session {
            inner: Arc::new(AsyncMutex::new(Inner { wallet, conn })),
            updates: Arc::new(AsyncMutex::new(updates)),
            info: Arc::new(AsyncMutex::new(logging.info_subscriber)),
            warnings: Arc::new(AsyncMutex::new(logging.warning_subscriber)),
            requester: client.requester(),
            synced: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Await the next wallet update, apply it, and persist.
    ///
    /// Returns once the node has caught up to the tip or a new block arrives,
    /// so the caller loops on it.
    pub async fn next_update(&self) -> Result<Summary> {
        let update = {
            let mut updates = self.updates.lock().await;
            updates
                .update()
                .await
                .map_err(|e| anyhow!("sync failed: {e}"))?
        };

        let mut inner = self.inner.lock().await;
        let Inner { wallet, conn } = &mut *inner;
        wallet
            .apply_update(update)
            .map_err(|e| anyhow!("could not apply the update: {e}"))?;
        wallet
            .persist(conn)
            .map_err(|e| anyhow!("could not persist the wallet: {e}"))?;

        let address = wallet.next_unused_address(KeychainKind::External);
        wallet
            .persist(conn)
            .map_err(|e| anyhow!("could not persist the wallet: {e}"))?;

        self.synced.store(true, Ordering::Relaxed);

        let balance = wallet.balance();
        Ok(Summary {
            balance_sats: balance.confirmed.to_sat(),
            pending_sats: (balance.trusted_pending + balance.untrusted_pending).to_sat(),
            tip: wallet.latest_checkpoint().height(),
            next_address: address.address.to_string(),
        })
    }

    /// Await the next progress event. `None` when the node has stopped.
    ///
    /// Silence is itself reportable: if nothing arrives for a while the caller
    /// gets `Waiting`, so the UI can distinguish "working" from "hung" instead
    /// of spinning against a label that never changes.
    pub async fn next_progress(&self) -> Option<Progress> {
        let mut info = self.info.lock().await;
        let event = match tokio::time::timeout(QUIET_BEFORE_WAITING, info.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => return None,
            // Silence after the first sync is the normal resting state — the
            // node is simply waiting for the next block. Reporting that as
            // "waiting for peers" made a finished wallet look broken.
            Err(_elapsed) => {
                return Some(if self.synced.load(Ordering::Relaxed) {
                    Progress::Synced
                } else {
                    Progress::Waiting
                });
            }
        };
        Some(match event {
            Info::SuccessfulHandshake => Progress::Connecting,
            Info::ConnectionsMet => Progress::Connected,
            Info::Progress(p) => {
                let fraction = p.fraction_complete() as f64;
                // A zero fraction means no filter header has arrived yet, so
                // the node is still walking the header chain. Reporting that as
                // "scanning filters, 0%" reads as stuck when it is not.
                if fraction <= 0.0 {
                    Progress::Headers(p.chain_height())
                } else if fraction >= 1.0 {
                    Progress::Synced
                } else {
                    Progress::Scanning(fraction)
                }
            }
            Info::BlockReceived(_) => Progress::Connected,
        })
    }

    /// Await the next warning from the node. `None` when it has stopped.
    ///
    /// Without this, a node that cannot find peers serving compact filters
    /// stalls in complete silence.
    pub async fn next_warning(&self) -> Option<Notice> {
        let mut warnings = self.warnings.lock().await;
        let warning = warnings.recv().await?;
        tracing::debug!(%warning, "node warning");
        Some(match warning {
            // A peer count is information, not an alarm. Dropping and
            // re-establishing connections is ordinary behaviour on a network
            // where most nodes do not serve filters.
            bdk_kyoto::Warning::NeedConnections { connected, required } => {
                Notice::Peers { connected, required }
            }
            // Nothing a person can act on, and constant during normal peer
            // churn. Showing these as a standing message trains people to
            // ignore the row that will one day matter.
            bdk_kyoto::Warning::CouldNotConnect
            | bdk_kyoto::Warning::PeerTimedOut
            | bdk_kyoto::Warning::NoCompactFilters
            | bdk_kyoto::Warning::UnsolicitedMessage
            | bdk_kyoto::Warning::EvaluatingFork => Notice::Ignorable,

            bdk_kyoto::Warning::PotentialStaleTip => Notice::Problem(
                "No new blocks for a while. The connection may be stale.".into(),
            ),
            bdk_kyoto::Warning::TransactionRejected { payload } => {
                Notice::Problem(format!("A transaction was rejected by the network: {payload:?}"))
            }
            bdk_kyoto::Warning::UnexpectedSyncError { warning } => {
                Notice::Problem(format!("Sync error: {warning}"))
            }
            other => Notice::Problem(format!("{other}")),
        })
    }

    pub fn shutdown(&self) {
        let _ = self.requester.shutdown();
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Session")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_are_grouped() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(205_008), "205,008");
    }

    #[test]
    fn the_header_phase_is_indeterminate() {
        // A bar cannot be drawn for a phase with no known total.
        assert!(Progress::Headers(1000).fraction().is_none());
        assert!(Progress::Connecting.fraction().is_none());
        assert_eq!(Progress::Scanning(0.5).fraction(), Some(0.5));
        assert_eq!(Progress::Synced.fraction(), Some(1.0));
    }
}
