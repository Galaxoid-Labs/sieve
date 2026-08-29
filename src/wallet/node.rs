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
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bdk_kyoto::builder::{Builder, BuilderExt};
use bdk_kyoto::{Info, LightClient, ScanType, UpdateSubscriber, wallets::Single};
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};
use tokio::sync::Mutex as AsyncMutex;

use super::{NETWORK, Paths, Summary, restrict};

/// How many peers to hold open. Two gives some resilience to one peer stalling
/// without making the wallet noisy on the network.
const REQUIRED_PEERS: u8 = 2;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Sync progress, as the UI wants to render it.
#[derive(Debug, Clone)]
pub enum Progress {
    Connecting,
    Connected,
    /// Filters downloading, 0.0 to 1.0.
    Scanning(f64),
    Synced,
}

impl Progress {
    pub fn label(&self) -> String {
        match self {
            Progress::Connecting => "Looking for peers…".into(),
            Progress::Connected => "Connected, requesting filters…".into(),
            Progress::Scanning(f) => format!("Scanning block filters — {:.0}%", f * 100.0),
            Progress::Synced => "Up to date".into(),
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
    requester: bdk_kyoto::Requester,
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

        // `ScanType::Sync` resumes from the wallet's own checkpoint. For a
        // freshly created wallet that checkpoint is the genesis block, so the
        // first sync walks the whole chain. Recording a birthday height at
        // creation is the fix, and is tracked as M2 follow-up work.
        let client: LightClient<_, Single> = Builder::new(NETWORK)
            .required_peers(REQUIRED_PEERS)
            .data_dir(headers)
            .response_timeout(RESPONSE_TIMEOUT)
            .build_with_wallet(&wallet, ScanType::Sync)
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
            requester: client.requester(),
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

        Ok(Summary {
            balance_sats: wallet.balance().total().to_sat(),
            next_address: address.address.to_string(),
        })
    }

    /// Await the next progress event. `None` when the node has stopped.
    pub async fn next_progress(&self) -> Option<Progress> {
        let mut info = self.info.lock().await;
        let event = info.recv().await?;
        Some(match event {
            Info::SuccessfulHandshake => Progress::Connecting,
            Info::ConnectionsMet => Progress::Connected,
            Info::Progress(p) => {
                let fraction = p.fraction_complete() as f64;
                if fraction >= 1.0 { Progress::Synced } else { Progress::Scanning(fraction) }
            }
            Info::BlockReceived(_) => Progress::Connected,
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
