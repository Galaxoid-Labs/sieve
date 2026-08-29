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
use bdk_kyoto::{Info, LightClient, ScanType, UpdateSubscriber, wallets::Multiple};
use bdk_wallet::Wallet;
use tokio::sync::Mutex as AsyncMutex;

use bdk_kyoto::bip157::HashCheckpoint;

use super::accounts::Portfolio;
use super::{Meta, Paths, Summary};

/// How many peers to hold open. Kyoto clamps this to 1–15 and defaults to 1.
///
/// Eight, matching Bitcoin Core's outbound default, for two reasons that point
/// the same way. Filters are fetched in parallel, so this is the main lever on
/// sync speed, and the peers serving `NODE_COMPACT_FILTERS` are a small
/// minority — most nodes do not run `-blockfilterindex=1` — so a good share of
/// connections are useless for our purposes.
///
/// It also helps privacy rather than hurting it. When a filter matches, the
/// block is fetched from one peer, and that peer learns a block interested us.
/// Spreading those requests over more peers means no single one sees the whole
/// pattern; holding few connections concentrates it.
///
/// The cost is bandwidth and memory, which is why this is not simply 15.
pub const REQUIRED_PEERS: u8 = 8;
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
    portfolio: Arc<AsyncMutex<Portfolio>>,
    updates: Arc<AsyncMutex<UpdateSubscriber<Multiple>>>,
    info: Arc<AsyncMutex<bdk_kyoto::Receiver<Info>>>,
    warnings: Arc<AsyncMutex<bdk_kyoto::UnboundedReceiver<bdk_kyoto::Warning>>>,
    requester: bdk_kyoto::Requester,
    /// Once the first sync lands, silence from the node is normal rather than
    /// a symptom, and must not be reported as waiting.
    synced: Arc<AtomicBool>,
    /// Set once the node reports real scan progress, after which connection
    /// events stop driving the status line.
    scanning: Arc<AtomicBool>,
    /// Which chain this session is on, so remembered peers never cross over.
    network: bdk_wallet::bitcoin::Network,
}

impl Session {
    /// Load every watched path, start the node, and begin fetching.
    ///
    /// Must be called from inside the tokio runtime — the node is spawned onto
    /// it. Relm4's async commands satisfy that.
    pub async fn start(paths: &Paths) -> Result<Self> {
        let meta = Meta::load(paths).context("this wallet has no metadata file")?;
        let network = meta.network();
        let dir = paths.db.parent().unwrap_or(&paths.db).to_path_buf();

        let portfolio =
            Portfolio::load(&dir, &meta.script_types, meta.primary, network)?;
        if portfolio.is_empty() {
            anyhow::bail!("no wallet databases found — unlock first");
        }

        // Headers are public chain data and identical for every wallet, so a
        // second wallet on a network Sieve has already seen starts with the
        // chain already downloaded instead of fetching it again.
        let headers = super::chain_dir(network);
        std::fs::create_dir_all(&headers)?;

        // Every path shares one node. A compact block filter covers a whole
        // block regardless of what is being matched, so watching four paths
        // downloads exactly what watching one would.
        let mut wallets: Vec<(&Wallet, ScanType)> = Vec::new();
        for account in &portfolio.accounts {
            // A path that has never synced starts at the recorded birthday; one
            // that has starts from its own checkpoint.
            let scan_type = if account.wallet.latest_checkpoint().height() == 0 {
                match meta.birthday_hash.parse() {
                    Ok(hash) => ScanType::Recovery {
                        // A floor, not just the current index: recovery peeks
                        // this many scripts, and a fresh wallet reporting 0
                        // would check almost nothing against the filters.
                        used_script_index: account
                            .wallet
                            .derivation_index(bdk_wallet::KeychainKind::External)
                            .unwrap_or(0)
                            .max(25),
                        checkpoint: HashCheckpoint::new(meta.birthday_height, hash),
                    },
                    Err(e) => {
                        tracing::warn!(%e, "birthday hash unreadable; scanning from genesis");
                        ScanType::Sync
                    }
                }
            } else {
                ScanType::Sync
            };
            tracing::info!(
                path = %account.script_type,
                ?scan_type,
                "watching derivation path"
            );
            wallets.push((&account.wallet, scan_type));
        }

        // Peers that were part of a working sync last time. Kyoto rediscovers
        // the network from DNS on every start, which is most of the wait
        // before anything happens; these turn that into direct connections.
        let remembered = crate::peers::remembered(network);
        tracing::info!(count = remembered.len(), %network, "seeding with remembered peers");
        let mut builder = Builder::new(network);
        for ip in remembered {
            builder = builder.add_peer(bdk_kyoto::bip157::TrustedPeer::from_ip(ip));
        }

        let client: LightClient<_, Multiple> = builder
            .required_peers(REQUIRED_PEERS)
            .data_dir(headers)
            .response_timeout(RESPONSE_TIMEOUT)
            .build_with_wallets(wallets)
            .map_err(|e| anyhow!("could not build the light client: {e}"))?;

        let (client, logging, updates) = client.subscribe();
        // `managed_start` hands back the node so it is spawned explicitly on
        // relm4's runtime rather than whichever runtime happens to be current.
        let (client, node) = client.managed_start();
        relm4::spawn(async move { node.run().await });

        Ok(Session {
            portfolio: Arc::new(AsyncMutex::new(portfolio)),
            updates: Arc::new(AsyncMutex::new(updates)),
            info: Arc::new(AsyncMutex::new(logging.info_subscriber)),
            warnings: Arc::new(AsyncMutex::new(logging.warning_subscriber)),
            requester: client.requester(),
            synced: Arc::new(AtomicBool::new(false)),
            scanning: Arc::new(AtomicBool::new(false)),
            network,
        })
    }

    /// Await the next round of wallet updates, apply them, and persist.
    ///
    /// Returns once the node has caught up to the tip or a new block arrives,
    /// so the caller loops on it.
    pub async fn next_update(&self) -> Result<Summary> {
        let updates: Vec<_> = {
            let mut subscriber = self.updates.lock().await;
            subscriber
                .updates()
                .await
                .map_err(|e| anyhow!("sync failed: {e}"))?
                .collect()
        };

        let mut portfolio = self.portfolio.lock().await;
        for (id, update) in updates {
            // Updates arrive tagged by descriptor, because one node feeds
            // several wallets. An update with no matching account is not an
            // error worth failing the sync over.
            match portfolio.account_for(id) {
                Some(account) => account
                    .wallet
                    .apply_update(update)
                    .map_err(|e| anyhow!("could not apply the update: {e}"))?,
                None => tracing::warn!(?id, "update for an unknown descriptor"),
            }
        }

        // A recovery scan only tests scripts that have been derived. If the
        // last used address sits near the edge of the derived window, there may
        // be more beyond it that were never checked, so widen and go round
        // again. Without this, coins past the window are silently invisible.
        if portfolio.extend_gaps()? {
            tracing::info!("gap window widened; requesting a rescan");
            if let Err(e) = self.requester.rescan() {
                tracing::warn!(%e, "could not request a rescan");
            }
            // Not synced: there is more to check before the balance is final.
            self.synced.store(false, Ordering::Relaxed);
        } else {
            self.synced.store(true, Ordering::Relaxed);
        }

        Summary::from_portfolio(&mut portfolio)
    }

    /// Reveal a fresh receive address on one path.
    ///
    /// `next_unused_address` returns the same address until someone pays to it,
    /// which is correct for a single payer but links two payers who are each
    /// given it. This advances the keychain so a second payer gets a different
    /// address, and persists the reveal so the new script is watched.
    pub async fn reveal_next(
        &self,
        script_type: super::accounts::ScriptType,
    ) -> Result<(String, Summary)> {
        let mut portfolio = self.portfolio.lock().await;
        let address = {
            let account = portfolio
                .accounts
                .iter_mut()
                .find(|a| a.script_type == script_type)
                .context("that derivation path is not part of this wallet")?;
            let revealed = account
                .wallet
                .reveal_next_address(bdk_wallet::KeychainKind::External);
            account.persist()?;
            revealed.address.to_string()
        };

        let summary = Summary::from_portfolio(&mut portfolio)?;
        Ok((address, summary))
    }

    /// Build a transaction from one account, without signing it.
    ///
    /// Watch-only is enough for this: BDK needs public descriptors and UTXOs to
    /// choose coins and lay out outputs, and nothing else until the moment of
    /// signing. So the numbers can be shown, checked and abandoned without the
    /// password ever being asked for.
    pub async fn plan(&self, draft: &super::send::Draft) -> Result<super::send::Plan> {
        use super::send::Sending;

        let mut portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter_mut()
            .find(|a| a.script_type == draft.from)
            .context("that derivation path is not part of this wallet")?;

        let script = draft.to.script_pubkey();
        let psbt = {
            let mut builder = account.wallet.build_tx();
            builder.fee_rate(draft.fee_rate);
            match draft.amount {
                Sending::Exact(amount) => {
                    builder.add_recipient(script.clone(), amount);
                }
                // No change output, and the fee comes out of what is sent
                // rather than being added to it.
                Sending::Everything => {
                    builder.drain_wallet();
                    builder.drain_to(script.clone());
                }
            }
            builder.finish().map_err(|e| anyhow!("{e}"))?
        };

        // Laying out change revealed an address on the internal keychain.
        // Persist it: an unwatched change address is money the wallet cannot
        // see afterwards.
        account.persist()?;

        let fee = psbt.fee().map_err(|e| anyhow!("could not work out the fee: {e}"))?;
        let (spend, change) = super::send::split_outputs(&psbt, &script);

        Ok(super::send::Plan {
            psbt,
            from: draft.from,
            to: draft.to.to_string(),
            spend,
            fee,
            change,
        })
    }

    /// Sign a plan with the key from the vault and hand it to the network.
    ///
    /// The secret arrives already decrypted and leaves nothing behind: the
    /// signing wallet exists for the length of this call.
    ///
    /// Broadcast first, then record locally. A transaction no peer accepted is
    /// not a transaction, and showing it as pending would be a lie the wallet
    /// then has to walk back.
    pub async fn sign_and_send(
        &self,
        mut plan: super::send::Plan,
        secret: &str,
        bip39_passphrase: Option<&str>,
    ) -> Result<(bdk_wallet::bitcoin::Txid, Summary)> {
        let mut portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter_mut()
            .find(|a| a.script_type == plan.from)
            .context("that derivation path is not part of this wallet")?;

        let signing = super::send::signer(secret, plan.from, self.network, bip39_passphrase)?;
        super::send::check_signer(&signing, account.descriptor_id())?;

        if !super::send::sign(&signing, &mut plan.psbt)? {
            anyhow::bail!(
                "the transaction could not be completely signed with the key in this wallet"
            );
        }
        drop(signing);

        let tx = plan
            .psbt
            .extract_tx()
            .map_err(|e| anyhow!("the signed transaction is not valid: {e}"))?;
        let txid = tx.compute_txid();

        // Broadcasting tells the peer it goes to that this transaction is
        // probably ours — the one thing a filter-based wallet cannot hide by
        // downloading more. It is inherent to sending, not to this design.
        self.requester
            .submit_package(tx.clone())
            .await
            .map_err(|e| anyhow!("no peer accepted the transaction: {e}"))?;

        let seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        account.wallet.apply_unconfirmed_txs([(tx, seen)]);
        account.persist()?;

        let summary = Summary::from_portfolio(&mut portfolio)?;
        Ok((txid, summary))
    }

    /// Await the next progress event. `None` when the node has stopped.
    ///
    /// Silence is itself reportable: if nothing arrives for a while the caller
    /// gets `Waiting`, so the UI can distinguish "working" from "hung" instead
    /// of spinning against a label that never changes.
    pub async fn next_progress(&self) -> Option<Progress> {
        let mut info = self.info.lock().await;
        loop {
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
        // Peer churn is constant and orthogonal to scan progress. Once
        // scanning has started, a handshake must not overwrite the status with
        // something that reads like going backwards — the peer count has its
        // own row for that.
        let scanning = self.scanning.load(Ordering::Relaxed);
        return Some(match event {
            Info::SuccessfulHandshake | Info::ConnectionsMet | Info::BlockReceived(_)
                if scanning =>
            {
                continue;
            }
            Info::SuccessfulHandshake => Progress::Connecting,
            Info::ConnectionsMet => Progress::Connected,
            Info::Progress(p) => {
                self.scanning.store(true, Ordering::Relaxed);
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
        });
        }
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
            // Per-peer and self-healing: the node drops that peer and carries
            // on. Surfacing it trains people to ignore the row that will one
            // day carry something they can act on.
            bdk_kyoto::Warning::UnexpectedSyncError { warning } => {
                tracing::debug!(%warning, "peer-level sync error; dropping that peer");
                Notice::Ignorable
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

/// What the chain looks like from here, derived from headers this node already
/// holds.
///
/// Everything below comes out of the header chain and the peers already
/// connected. Nothing here asks anyone a new question, so none of it costs a
/// disclosure.
#[derive(Debug, Clone, Default)]
pub struct ChainInfo {
    pub tip_height: u32,
    pub tip_hash: String,
    /// Timestamp in the tip's header.
    pub tip_time: Option<u64>,
    pub difficulty: f64,
    /// Hashes per second implied by the difficulty and the target interval.
    pub hashrate: f64,
    /// Mean seconds between blocks over the recent window.
    pub mean_interval: Option<f64>,
    /// Blocks remaining before the next difficulty adjustment.
    pub blocks_to_retarget: u32,
    /// Where the current period's pace puts the next adjustment, as a
    /// multiplier: 1.05 is five percent harder.
    pub retarget_estimate: Option<f64>,
    /// How many connections the node holds. Kyoto reports one entry per
    /// connection and several can share an address, so this is not the length
    /// of `peers`.
    pub connections: usize,
    /// Distinct addresses among those connections.
    pub peers: Vec<PeerInfo>,
    /// Lowest fee rate the connected peers will relay, in sat/vB.
    pub min_relay_fee: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub address: String,
    /// What the peer advertises, or `None` when nothing was reported.
    ///
    /// Kyoto leaves the service flags empty for connections whose version
    /// message it has not recorded, and every real node advertises at least
    /// NETWORK — so zero means "not known", not "serves nothing". Claiming the
    /// latter told people their peers were useless when they were not.
    pub serves_filters: Option<bool>,
}

/// A peer address as a person would write it, rather than as Rust prints it.
fn format_address(address: &bdk_wallet::bitcoin::p2p::address::AddrV2) -> String {
    use bdk_wallet::bitcoin::p2p::address::AddrV2;
    match address {
        AddrV2::Ipv4(ip) => ip.to_string(),
        AddrV2::Ipv6(ip) => format!("[{ip}]"),
        AddrV2::TorV3(_) => "Tor address".into(),
        other => format!("{other:?}"),
    }
}

/// Blocks between difficulty adjustments.
const RETARGET_INTERVAL: u32 = 2016;
/// The interval the protocol aims for, in seconds.
const TARGET_SPACING: f64 = 600.0;
/// How far back to look when measuring the recent pace. A day of blocks is
/// enough to be meaningful without being dominated by luck.
const PACE_WINDOW: u32 = 144;

impl Session {
    /// Gather what can be told from the headers already held.
    pub async fn chain_info(&self) -> Result<ChainInfo> {
        let tip = self
            .requester
            .chain_tip()
            .await
            .map_err(|e| anyhow!("could not read the chain tip: {e}"))?;

        let mut info = ChainInfo {
            tip_height: tip.height,
            tip_hash: tip.hash.to_string(),
            blocks_to_retarget: RETARGET_INTERVAL - (tip.height % RETARGET_INTERVAL),
            ..Default::default()
        };

        if let Ok(Some(header)) = self.requester.get_header(tip.height).await {
            info.tip_time = Some(header.header.time as u64);
            info.difficulty = header.header.difficulty_float();
            // Work per block is difficulty * 2^32 hashes; at one block per
            // target spacing that is the network's implied rate.
            info.hashrate = info.difficulty * 4_294_967_296.0 / TARGET_SPACING;

            // Pace over the recent window, which is what tells you whether the
            // next adjustment goes up or down.
            if tip.height > PACE_WINDOW
                && let Ok(Some(earlier)) = self.requester.get_header(tip.height - PACE_WINDOW).await
            {
                let elapsed = header.header.time as f64 - earlier.header.time as f64;
                if elapsed > 0.0 {
                    let mean = elapsed / PACE_WINDOW as f64;
                    info.mean_interval = Some(mean);
                    // Faster blocks mean the next period gets harder.
                    info.retarget_estimate = Some((TARGET_SPACING / mean).clamp(0.25, 4.0));
                }
            }
        }

        if let Ok(peers) = self.requester.peer_info().await {
            // One entry per connection, and several connections can share an
            // address. Both numbers are real and they are not the same
            // question: how many connections are open, and how many distinct
            // machines they reach.
            info.connections = peers.len();
            let mut seen = std::collections::HashSet::new();
            let peers: Vec<_> = peers
                .into_iter()
                .filter(|(address, _)| seen.insert(format_address(address)))
                .collect();
            info.peers = peers
                .into_iter()
                .map(|(address, services)| PeerInfo {
                    address: format_address(&address),
                    serves_filters: (services
                        != bdk_wallet::bitcoin::p2p::ServiceFlags::NONE)
                        .then(|| {
                            services.has(
                                bdk_wallet::bitcoin::p2p::ServiceFlags::COMPACT_FILTERS,
                            )
                        }),
                })
                .collect();
        }

        // Only remember peers once a sync has actually landed. Before that the
        // connected set includes peers the node is still evaluating and will
        // drop for not serving filters, and seeding with those next time buys
        // nothing.
        //
        // Prefer peers that positively advertise compact filters, but do not
        // require it: kyoto reports no service flags for most connections, so
        // demanding the flag would remember almost nobody. What is left is
        // still peers that were present through a working sync.
        if self.synced.load(Ordering::Relaxed) && !info.peers.is_empty() {
            let confirmed: Vec<String> = info
                .peers
                .iter()
                .filter(|p| p.serves_filters == Some(true))
                .map(|p| p.address.clone())
                .collect();

            let addresses = if confirmed.is_empty() {
                info.peers.iter().map(|p| p.address.clone()).collect()
            } else {
                confirmed
            };
            crate::peers::remember(self.network, &addresses);
        }

        if let Ok(rate) = self.requester.broadcast_min_feerate().await {
            info.min_relay_fee = Some(rate.to_sat_per_kwu() as f64 / 250.0);
        }

        Ok(info)
    }
}
