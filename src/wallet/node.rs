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

use anyhow::{Context, Result, anyhow, bail};
use bdk_kyoto::builder::{Builder, BuilderExt};
use bdk_kyoto::{Info, LightClient, ScanType, UpdateSubscriber, wallets::Multiple};
use bdk_wallet::Wallet;
use tokio::sync::Mutex as AsyncMutex;

use bdk_kyoto::bip157::HashCheckpoint;

use super::accounts::Portfolio;
use super::{Meta, Paths, Summary};

/// How many peers to hold open — and, in kyoto, how many must agree on the
/// filter headers before a single filter is downloaded.
///
/// Those are not the same question, and conflating them cost a day. kyoto
/// passes this straight into `FilterHeaderAgreements`, so eight — chosen by
/// analogy with Bitcoin Core's outbound default — meant demanding that eight
/// peers *serving compact filters* agree before the scan could start. Nodes
/// that serve filters are a small minority: most nodes do not run
/// `-blockfilterindex=1`. So filter headers would complete, quorum would never
/// be reached, and the sync sat at exactly twenty-five percent for ever.
///
/// Eight is reachable on a direct connection — measured, not assumed: with
/// Tor off the same wallet finds eight filter-serving peers and downloads at
/// full speed. So the strong quorum stays where it works.
pub const REQUIRED_PEERS: u8 = 8;

/// The quorum over Tor, where filter-serving peers are much harder to reach:
/// exits are a bottleneck, every connection is a circuit, and eight never
/// arrived.
///
/// Two peers agreeing is a weaker check than eight, and it is the honest
/// trade. A peer that lies about filter headers can hide a payment from this
/// wallet — worth defending against — but the alternative here is not a
/// stronger check, it is a wallet that never syncs at all, which protects
/// nobody. Anyone who wants the stronger guarantee can turn Tor off, and the
/// network view says which one is in force.
pub const REQUIRED_PEERS_OVER_TOR: u8 = 2;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
/// The same two, over Tor.
///
/// kyoto's default handshake timeout is two seconds, which is a sensible
/// figure for a direct connection and hopeless through a circuit: building one
/// and reaching a peer at the far end takes five to twenty seconds, so every
/// attempt died at the two second mark and the node never got past "waiting
/// for peers". Replies are slower for the same reason.
const TOR_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Three minutes for a reply, over Tor.
///
/// Thirty was not enough and the way it failed was instructive: peers
/// connected, sat for exactly thirty seconds, were dropped as unresponsive,
/// and the whole thing began again — so filter headers, which are small and
/// quick, flew through, and filters, which are a thousand times larger, never
/// finished a single batch. A peer that is slow is not a peer that is gone,
/// and through three relays slow is the normal case.
const TOR_RESPONSE_TIMEOUT: Duration = Duration::from_secs(180);
/// If the node says nothing for this long, tell the user so rather than
/// leaving a spinner turning against a frozen label.
const QUIET_BEFORE_WAITING: Duration = Duration::from_secs(20);
/// The same, over Tor. Every message crosses three relays, and filter batches
/// arrive in gaps that would look like a stall at the direct-connection
/// figure.
const QUIET_OVER_TOR: Duration = Duration::from_secs(60);

/// The least time an *empty* wallet update may take.
///
/// A real update takes as long as it takes and is not affected by this. An
/// empty one arriving in microseconds means the stream behind it has nothing
/// left to say, and the caller re-arms on every one — so without a floor the
/// pair becomes a hot loop that pegs the interface thread.
const QUIET_UPDATE_FLOOR: Duration = Duration::from_millis(250);

/// Group digits so six-figure block heights stay readable.
fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How much of kyoto's progress figure is the filter-header phase.
///
/// Its fraction is `(filter_headers + 3 * filters) / (4 * total)`, so filter
/// headers are exactly the first quarter — which is why a sync that cannot
/// reach filters stops at precisely twenty-five percent.
pub const FILTER_HEADER_SHARE: f64 = 0.25;

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
    /// Every filter is in and the blocks they matched are being fetched and
    /// read. Carries how many blocks have arrived.
    ///
    /// A filter only says a block *probably* touches this wallet; the block
    /// itself is what holds the transaction. So this is the phase where the
    /// balance is actually worked out, it runs after the bar is full, and it
    /// takes as long as fetching those blocks from peers takes. Left unsaid,
    /// it reads as a finished sync that has hung.
    Blocks(usize),
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
            // The height reached, not a percentage: the percentage belongs to
            // the bar beside it, which knows the wallet's starting point and
            // can estimate where the chain ends. This says where it is now.
            Progress::Headers(height) => {
                format!(
                    "Downloading block headers — at block {}",
                    thousands(*height)
                )
            }
            // Two phases behind one number, and they are nothing alike. The
            // first quarter of kyoto's figure is filter *headers* — thirty-two
            // bytes each, thousands a second. The rest is the filters
            // themselves, fifteen kilobytes each, which is the whole download.
            // Calling both "scanning block filters" made the fast part look
            // like the slow part and the boundary between them look like a
            // stall.
            //
            // Two decimals on the filters: on a chain of this size one decimal
            // would sit still for thousands of filters and read as frozen.
            Progress::Scanning(f) if *f < FILTER_HEADER_SHARE => {
                format!("Fetching filter headers — {:.1}%", f * 100.0)
            }
            Progress::Scanning(f) => {
                format!("Downloading and checking filters — {:.2}%", f * 100.0)
            }
            // Named for what it is rather than "finishing up": these are
            // the blocks that matched, and reading them is what produces the
            // balance. One is a real step; a hundred is a wallet with a lot of
            // history, and either way something is happening.
            Progress::Blocks(1) => "Reading 1 block that matched this wallet…".into(),
            Progress::Blocks(n) => {
                format!("Reading blocks that matched this wallet — {n} so far…")
            }
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

/// Where the light client runs, and the only thing that reliably stops it.
///
/// **kyoto spawns a task per peer and never cancels one.**
/// `ClientMessage::Shutdown` is handled as `return Ok(())`, so `Node::run`
/// simply returns — dropping the `PeerMap` and with it every `JoinHandle` it
/// was holding. A dropped handle in tokio *detaches* the task rather than
/// cancelling it, and nothing in kyoto calls `abort` or implements `Drop`, so
/// each peer carries on running with nobody left to talk to.
///
/// What it does then is spin. `peer.rs` selects on its channel from the node
/// and treats a closed one as `None => continue`, so once the node is gone
/// `recv()` completes instantly on every pass and the loop turns as fast as the
/// processor allows. It stops at `max_connection_time`, which kyoto defaults to
/// **two hours**.
///
/// That is the CPU that appeared when Tor was switched on and stayed after it
/// was switched off. Both toggles restart the session, because a node already
/// talking to peers cannot change how it connects; each restart stranded
/// another set of peers, and each set spun for two hours. Aborting the task
/// returned by `managed_start` does nothing about it — that task has already
/// returned by then, and the peers were never its children in any sense tokio
/// tracks.
///
/// So the node gets a runtime of its own, and stopping the session shuts that
/// runtime down. Every task kyoto spawned dies with it, tracked or not. This is
/// the one lever that does not depend on kyoto's cooperation.
struct NodeHost {
    /// Taken by `stop`, so stopping twice is not an error and dropping after a
    /// stop has nothing left to do.
    runtime: std::sync::Mutex<Option<tokio::runtime::Runtime>>,
}

impl NodeHost {
    /// Start a node on a runtime of its own.
    fn start(node: bdk_kyoto::bip157::Node) -> Result<Self> {
        // Sized like the runtime this used to share with relm4, so nothing
        // about the scan gets slower: kyoto's filter matching is one
        // sequential task, and the rest is a socket per peer.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .thread_name("sieve:node")
            .enable_all()
            .build()
            .map_err(|e| anyhow!("could not start a runtime for the light client: {e}"))?;

        runtime.spawn(async move { node.run().await });

        Ok(Self {
            runtime: std::sync::Mutex::new(Some(runtime)),
        })
    }

    /// Shut the runtime down, cancelling everything on it.
    ///
    /// `shutdown_background` rather than a plain drop, and deliberately:
    /// dropping a runtime blocks, and blocking is a panic inside an async
    /// context. A `Session` is held by `Arc` and clones of it are moved into
    /// relm4 commands, so the last one can very well fall out of scope inside
    /// a future — which would turn this cleanup into a crash. Nothing here
    /// needs to be waited for anyway: the node does network I/O, and
    /// everything persisted goes through `next_update` on our side of the
    /// channel, so there is no half-written state to protect.
    fn stop(&self) {
        if let Ok(mut slot) = self.runtime.lock()
            && let Some(runtime) = slot.take()
        {
            runtime.shutdown_background();
        }
    }
}

impl Drop for NodeHost {
    /// So a session that is dropped without being shut down still takes its
    /// node with it, and still does not block to do it.
    fn drop(&mut self) {
        self.stop();
    }
}

/// A running light client bound to one wallet.
pub struct Session {
    /// The runtime the node runs on, and the only reliable way to stop it.
    /// See `NodeHost`.
    node: NodeHost,
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
    /// Set when filter progress reaches the end. Until then a received block
    /// is one match among many and must not interrupt the bar; after it, it is
    /// the only thing still happening.
    filters_done: Arc<AtomicBool>,
    /// Blocks fetched and read this session.
    blocks_read: Arc<std::sync::atomic::AtomicUsize>,
    /// Which chain this session is on, so remembered peers never cross over.
    network: bdk_wallet::bitcoin::Network,
    /// The last thing the node actually said about progress.
    ///
    /// Silence is not news. When a scan is under way and nothing arrives for a
    /// while, the honest report is the last real state, not "waiting for
    /// peers" — the node is working, and saying otherwise reads as a stall
    /// that is not happening.
    last_progress: std::sync::Mutex<Option<Progress>>,
    /// How long silence has to last before it is worth reporting. Longer over
    /// Tor, where every message crosses three relays and gaps are ordinary.
    quiet: Duration,
    /// The median time past, and the tip it was worked out for.
    ///
    /// Eleven header round trips is nothing on a direct connection and a great
    /// deal through Tor, where each one is a circuit. It only changes when the
    /// tip does, so it is worked out once per block rather than once per
    /// refresh.
    median_time: std::sync::Mutex<Option<(u32, u64)>>,
}

impl Session {
    /// Load every watched path, start the node, and begin fetching.
    ///
    /// Must be called from inside the tokio runtime — the node is spawned onto
    /// it. Relm4's async commands satisfy that.
    pub async fn start(paths: &Paths, tor: Option<crate::tor::Proxy>) -> Result<Self> {
        let meta = Meta::load(paths).context("this wallet has no metadata file")?;
        let network = meta.network();
        let dir = paths.db.parent().unwrap_or(&paths.db).to_path_buf();

        // Created here or brought from elsewhere, which decides which
        // receive address is safe to offer. `created_at` is set by `create`
        // and by nothing else.
        let portfolio = Portfolio::load(&dir, &meta.script_types, meta.primary, network)?
            .imported(meta.created_at.is_none());
        if portfolio.is_empty() {
            anyhow::bail!("no wallet databases found — unlock first");
        }

        // Handed over, and thrown away by this version of the node:
        // `bip157::Node::new` destructures its config with `data_path: _`.
        // Nothing is written here and nothing is read back, so headers and
        // filters are downloaded again on every start and a second wallet on
        // this network gains nothing from the first.
        //
        // Kept because it costs nothing and is where a future version would
        // put its store — but it is a promise the library does not currently
        // keep, and the comment that used to sit here claimed otherwise.
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
                match resume_point(&meta).or_else(|| {
                    meta.birthday_hash
                        .parse()
                        .ok()
                        .map(|hash| (meta.birthday_height, hash))
                }) {
                    Some((height, hash)) => ScanType::Recovery {
                        // A floor, not just the current index: recovery peeks
                        // this many scripts, and a fresh wallet reporting 0
                        // would check almost nothing against the filters.
                        used_script_index: account
                            .wallet
                            .derivation_index(bdk_wallet::KeychainKind::External)
                            .unwrap_or(0)
                            .max(25),
                        checkpoint: HashCheckpoint::new(height, hash),
                    },
                    None => {
                        tracing::warn!("birthday hash unreadable; scanning from genesis");
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
        // The stored headers are deliberately *not* handed to the node.
        //
        // `bdk_kyoto::build_with_wallets` sets its own chain state one line
        // before it builds — `self.chain_state(ChainState::Checkpoint(cp_min))`
        // — so a snapshot given to the builder is discarded. Nine hundred
        // thousand validated headers were loaded and thrown away on every
        // start until that line was found.
        //
        // The store earns its keep through `resume_point` instead: the
        // recovery checkpoint *is* `cp_min`, so moving it forward is the one
        // lever this API offers, and moving it needs the block hash at that
        // height — which only the stored chain has.

        // An onion address is only reachable through Tor. Handing one to a
        // node connecting directly spends an attempt on something that cannot
        // work, and the remembered list is mostly onions after a run with Tor
        // on.
        let usable: Vec<String> = remembered
            .into_iter()
            .filter(|address| tor.is_some() || !crate::tor::onion::looks_like_onion(address))
            .collect();
        let peers = usable.len();

        if let Some(proxy) = tor {
            builder = builder
                .socks5_proxy(proxy.addr())
                .handshake_timeout(TOR_HANDSHAKE_TIMEOUT)
                .response_timeout(TOR_RESPONSE_TIMEOUT);
            tracing::info!(%proxy, "routing peer connections through Tor");

            // kyoto falls back to a DNS lookup when it runs out of peers to
            // try, and that lookup is not proxied — it would go out over the
            // clear from this machine while everything else went through Tor.
            // Resolving the same seeds here, through the proxy, keeps the node
            // supplied so it never reaches for the resolver.
            //
            // Always, not only when the remembered list is short. Remembered
            // peers are addresses that worked *once*: nodes go away, and a
            // list of eight dead ones leaves the node with nothing to dial and
            // no way to find anything else. Fresh seeds every start keep the
            // pool alive, and cost a handful of RESOLVE round trips.
            let network_name = network.to_string();
            let seeded = tokio::task::spawn_blocking(move || {
                crate::tor::resolve_seeds(proxy, &network_name, REQUIRED_PEERS as usize)
            })
            .await
            .unwrap_or_default();

            if seeded.is_empty() && peers == 0 {
                // Without peers the node would resolve them itself, over the
                // clear. Refusing is the honest outcome: Tor was asked for,
                // and it could not be delivered.
                anyhow::bail!(
                    "could not find any peers through Tor. Check that the proxy at \
                     {proxy} is running, or turn Tor off in preferences."
                );
            }

            tracing::info!(
                count = seeded.len(),
                remembered = peers,
                "seeded peers resolved through Tor"
            );
            // Before the remembered ones. kyoto hands out configured peers in
            // order, and these were asked for by service bits — they are the
            // ones that can actually serve a filter.
            for ip in seeded {
                builder = builder.add_peer(bdk_kyoto::bip157::TrustedPeer::from_ip(ip));
            }
        }

        // On a direct connection, seed from DNS ourselves rather than
        // leaving it to the node.
        //
        // Handing kyoto a single remembered peer and letting it find the rest
        // produced eight connections to one machine — every filter coming from
        // one place, which is slow and is the opposite of what a
        // filter-matching wallet is for. Its own DNS fallback only runs when
        // its address book is empty, and one configured peer is enough to keep
        // it from ever getting there.
        //
        // An ordinary DNS query answers with a couple of dozen addresses at
        // once, where Tor's RESOLVE gives one, so this is cheap and it is
        // where the diversity comes from.
        if tor.is_none() {
            let resolved = resolve_seeds_directly(network, REQUIRED_PEERS as usize * 3).await;
            tracing::info!(count = resolved.len(), %network, "seeded peers from DNS");
            for ip in resolved {
                builder = builder.add_peer(bdk_kyoto::bip157::TrustedPeer::from_ip(ip));
            }
        }

        // Remembered peers last, after any seeded ones.
        for address in usable {
            if let Some(peer) = trusted_peer(&address) {
                builder = builder.add_peer(peer);
            }
        }

        if tor.is_none() {
            builder = builder.response_timeout(RESPONSE_TIMEOUT);
        }

        let client: LightClient<_, Multiple> = builder
            .required_peers(if tor.is_some() {
                REQUIRED_PEERS_OVER_TOR
            } else {
                REQUIRED_PEERS
            })
            .data_dir(headers)
            .build_with_wallets(wallets)
            .map_err(|e| anyhow!("could not build the light client: {e}"))?;

        let (client, logging, updates) = client.subscribe();
        // `managed_start` hands back the node so it is spawned explicitly on
        // relm4's runtime rather than whichever runtime happens to be current.
        let (client, node) = client.managed_start();
        // On a runtime of its own, which is what makes it possible to stop.
        // See `NodeHost`.
        let host = NodeHost::start(node)?;

        let session = Session {
            node: host,
            portfolio: Arc::new(AsyncMutex::new(portfolio)),
            updates: Arc::new(AsyncMutex::new(updates)),
            info: Arc::new(AsyncMutex::new(logging.info_subscriber)),
            warnings: Arc::new(AsyncMutex::new(logging.warning_subscriber)),
            requester: client.requester(),
            last_progress: std::sync::Mutex::new(None),
            quiet: if tor.is_some() {
                QUIET_OVER_TOR
            } else {
                QUIET_BEFORE_WAITING
            },
            median_time: std::sync::Mutex::new(None),
            synced: Arc::new(AtomicBool::new(false)),
            scanning: Arc::new(AtomicBool::new(false)),
            filters_done: Arc::new(AtomicBool::new(false)),
            blocks_read: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            network,
        };

        // A wallet made on this machine has no history, so scanning for one is
        // wasted work — but "start at the tip" needs the tip's *hash*, and a
        // hash only comes from the chain. So creation records a floor and this
        // moves it, once, as soon as a node can answer.
        session.adopt_tip_birthday(paths).await;

        Ok(session)
    }

    /// Move a newly made wallet's birthday up to the chain tip.
    ///
    /// Only for a wallet Sieve created: it cannot hold a payment older than
    /// itself, which is the fact that makes this safe. An imported wallet's
    /// seed may have been spending for years, and moving its birthday forward
    /// would skip the blocks holding its coins.
    ///
    /// Best effort throughout. Every failure here leaves the wallet exactly as
    /// it was — starting from the compiled-in floor, which is slower and always
    /// correct — so none of them is worth failing a session over.
    async fn adopt_tip_birthday(&self, paths: &Paths) {
        let Some(mut meta) = Meta::load(paths) else {
            return;
        };
        if !meta.birthday_pending {
            return;
        }
        let Ok(tip) = self.requester.chain_tip().await else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let Some(height) = meta.tip_birthday(tip.height, now) else {
            return;
        };
        // The hash has to be the one at that exact height. Asking the node
        // rather than assuming the tip's own hash is the whole point: a
        // checkpoint whose height and hash disagree is refused, and one taken
        // from the wrong block is worse than refused.
        let Ok(Some(header)) = self.requester.get_header(height).await else {
            tracing::debug!(height, "no header yet for the new wallet's birthday");
            return;
        };

        meta.birthday_height = height;
        meta.birthday_hash = header.block_hash().to_string();
        meta.birthday_pending = false;
        if let Err(e) = meta.save(paths) {
            tracing::warn!(%e, "could not record the birthday");
            return;
        }
        tracing::info!(
            height,
            tip = tip.height,
            "new wallet starts watching near the tip rather than from a checkpoint"
        );

        // Narrow the scan already under way. Without this the birthday would
        // only take effect on the next launch, and the first run — the one
        // somebody is watching — would still walk the whole chain.
        if let Err(e) = self.requester.rescan_from(height) {
            tracing::warn!(%e, "could not narrow the scan; it will start correctly next time");
        }
    }

    /// Await the next round of wallet updates, apply them, and persist.
    ///
    /// Returns once the node has caught up to the tip or a new block arrives,
    /// so the caller loops on it.
    pub async fn next_update(&self) -> Result<Summary> {
        // Timed in three parts, because the gap between a full progress bar
        // and a balance on screen is made of all three and nothing said which.
        let waited = std::time::Instant::now();
        let updates: Vec<_> = {
            let mut subscriber = self.updates.lock().await;
            subscriber
                .updates()
                .await
                .map_err(|e| anyhow!("sync failed: {e}"))?
                .collect()
        };
        let waited = waited.elapsed();

        // **An empty update that returns instantly is a loop, not an update.**
        //
        // `updates()` yields `Ok` with nothing in it once the node behind it
        // has gone, and the caller re-arms on every `Ok` — so this returned
        // immediately, rebuilt the whole summary, drove a full transaction-list
        // rebuild on the main thread, and came straight back. Left overnight it
        // burned four hours of CPU on the GTK thread, which is why the window
        // went sluggish and its animations stuttered.
        //
        // The cure for the cause is elsewhere: the app rebuilds a client whose
        // channels have closed. This is the floor under it, so that no future
        // way of producing an empty update can spin the interface again. It
        // costs nothing when updates are real, because then they are not empty.
        if updates.is_empty() && waited < QUIET_UPDATE_FLOOR {
            tokio::time::sleep(QUIET_UPDATE_FLOOR - waited).await;
        }

        let applying = std::time::Instant::now();
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
                // Without the id: it is derived from the descriptor, so it
                // is a stable fingerprint for this wallet, and this is a
                // `warn` — visible by default, in the log somebody pastes when
                // they are asking for help with the very problem it reports.
                None => {
                    tracing::warn!("an update arrived for a descriptor this wallet is not watching")
                }
            }
        }

        let applying = applying.elapsed();
        tracing::debug!(
            waited_ms = waited.as_millis(),
            applied_ms = applying.as_millis(),
            blocks_read = self.blocks_read.load(Ordering::Relaxed),
            "wallet updates arrived"
        );

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

        // The last of the three: walking every transaction on every path and
        // writing the changeset back to sqlite. Nothing on screen moves while
        // it runs, so if it is the expensive part it is worth knowing.
        let summarising = std::time::Instant::now();
        let summary = Summary::from_portfolio(&mut portfolio);
        tracing::debug!(
            summarised_ms = summarising.elapsed().as_millis(),
            "summary built"
        );
        summary
    }

    /// Reveal a fresh receive address on one path.
    ///
    /// `next_unused_address` returns the same address until someone pays to it,
    /// which is correct for a single payer but links two payers who are each
    /// given it. This advances the keychain so a second payer gets a different
    /// address, and persists the reveal so the new script is watched.
    /// Hand out the next address on a path, and say where it sits.
    ///
    /// The index travels with it because it is not always the same as the
    /// summary's: revealing several addresses without using them leaves
    /// `next_unused_address` pointing at the *first* unused one while this
    /// returns the *last* revealed. A device asked to show "the same address"
    /// is asked by index, so carrying the wrong one would report a mismatch on
    /// an address that is perfectly correct.
    pub async fn reveal_next(
        &self,
        script_type: super::accounts::ScriptType,
    ) -> Result<(String, u32, Summary)> {
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
            (revealed.address.to_string(), revealed.index)
        };

        let summary = Summary::from_portfolio(&mut portfolio)?;
        Ok((address.0, address.1, summary))
    }

    /// Rebuild an unconfirmed payment at a higher fee.
    ///
    /// The replacement spends the same coins as the original, so only one of
    /// them can ever confirm — and if the original wins, the replacement
    /// becomes permanently invalid and costs nothing. That reconciliation is
    /// not ours to do: a confirmed transaction outranks an unconfirmed
    /// conflict in BDK's canonicalisation, so the wallet corrects itself when
    /// the block arrives.
    ///
    /// Watch-only builds this happily; only signing needs a key.
    pub async fn plan_bump(
        &self,
        txid: &str,
        script_type: super::accounts::ScriptType,
        fee_rate: bdk_wallet::bitcoin::FeeRate,
    ) -> Result<super::send::Plan> {
        let txid: bdk_wallet::bitcoin::Txid = txid
            .parse()
            .map_err(|_| anyhow!("that is not a transaction id"))?;

        let mut portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter_mut()
            .find(|a| a.script_type == script_type)
            .context("that derivation path is not part of this wallet")?;

        // What the original paid, and to whom: the replacement pays the same
        // people, and the screen has to be able to say so.
        let original = account
            .wallet
            .get_tx(txid)
            .context("this wallet does not hold that transaction")?;
        let was = account
            .wallet
            .calculate_fee(&original.tx_node.tx)
            .map(|fee| fee.to_sat())
            .unwrap_or(0);
        let (payees, change) = super::send::split_outputs_of(&original.tx_node.tx, &account.wallet);

        let psbt = super::send::build_replacement(
            &mut account.wallet,
            txid,
            fee_rate,
            bdk_wallet::bitcoin::Amount::from_sat(was),
            None,
        )?;

        let fee = psbt
            .fee()
            .map_err(|e| anyhow!("the replacement has no readable fee: {e}"))?;

        Ok(super::send::Plan {
            from: script_type,
            payees,
            change,
            fee,
            // A replacement rebuilds from the original's outputs, so whatever
            // it published is still there and the screen should say so.
            data: super::send::data_in(&psbt.unsigned_tx),
            replaces: Some(txid.to_string()),
            was_fee: Some(was),
            cancels: false,
            psbt,
        })
    }

    /// Replace an unconfirmed payment with one that pays nobody.
    ///
    /// The same coins, back to this wallet, at a higher fee — so the two
    /// conflict and only one can confirm. This is the closest thing to
    /// cancelling a payment that Bitcoin has, and it is not a guarantee: if
    /// the original is mined first the money is gone as intended, and nothing
    /// here can promise otherwise. What it does is give the network a better
    /// reason to prefer the replacement.
    ///
    /// The money returns on the *change* keychain rather than a receive
    /// address. It is not a payment from anybody — nobody sent it — and using
    /// a receive address would consume one that was meant to be handed out.
    pub async fn plan_cancel(
        &self,
        txid: &str,
        script_type: super::accounts::ScriptType,
        fee_rate: bdk_wallet::bitcoin::FeeRate,
    ) -> Result<super::send::Plan> {
        let txid: bdk_wallet::bitcoin::Txid = txid
            .parse()
            .map_err(|_| anyhow!("that is not a transaction id"))?;

        let mut portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter_mut()
            .find(|a| a.script_type == script_type)
            .context("that derivation path is not part of this wallet")?;

        let original = account
            .wallet
            .get_tx(txid)
            .context("this wallet does not hold that transaction")?;
        let was = account
            .wallet
            .calculate_fee(&original.tx_node.tx)
            .map(|fee| fee.to_sat())
            .unwrap_or(0);
        // Who the original was paying, which is what cancelling calls off.
        let (payees, _) = super::send::split_outputs_of(&original.tx_node.tx, &account.wallet);

        let back = account
            .wallet
            .reveal_next_address(bdk_wallet::KeychainKind::Internal)
            .address;
        account.persist()?;

        let psbt = super::send::build_replacement(
            &mut account.wallet,
            txid,
            fee_rate,
            bdk_wallet::bitcoin::Amount::from_sat(was),
            Some(back.script_pubkey()),
        )?;

        let fee = psbt
            .fee()
            .map_err(|e| anyhow!("the cancellation has no readable fee: {e}"))?;

        Ok(super::send::Plan {
            from: script_type,
            // Kept so the screen can name what is being called off rather
            // than describing a payment to nobody.
            payees,
            // Everything the replacement holds comes back here.
            change: Some(psbt.unsigned_tx.output.iter().map(|o| o.value).sum()),
            fee,
            // A cancellation drops the original's outputs, data included.
            data: Vec::new(),
            replaces: Some(txid.to_string()),
            was_fee: Some(was),
            cancels: true,
            psbt,
        })
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

        // A payment already broadcast and not yet in a block is not money to
        // spend again: it can still be dropped or replaced, and anything built
        // on it dies with it.
        let pending = super::send::unconfirmed_outpoints(&account.wallet);
        let waiting_on_a_block = !pending.is_empty();

        // Frozen coins join the pending ones: both are money the wallet can see
        // and must not reach for. The difference is only that one of them will
        // become spendable on its own.
        let mut held_back = pending.clone();
        held_back.extend(draft.frozen.iter().copied());

        let psbt = {
            let mut builder = account.wallet.build_tx();
            builder.unspendable(held_back);
            builder.fee_rate(draft.fee_rate);

            // Chosen coins mean exactly those. `manually_selected_only` is
            // what makes that true: without it BDK treats them as a starting
            // point and adds more when they fall short, which would quietly
            // undo the decision that was made.
            if !draft.coins.is_empty() {
                builder
                    .add_utxos(&draft.coins)
                    .map_err(|e| anyhow!("those coins cannot be spent: {e}"))?;
                builder.manually_selected_only();
            }

            if let Some(bytes) = draft.data.as_deref() {
                let data: &bdk_wallet::bitcoin::script::PushBytes = bytes
                    .try_into()
                    .map_err(|_| anyhow!("that is more data than a transaction can carry"))?;
                builder.add_data(&data);
            }
            for payee in &draft.payees {
                match payee.amount {
                    Sending::Exact(amount) => {
                        builder.add_recipient(payee.to.script_pubkey(), amount);
                    }
                    // No change output, and the fee comes out of what is sent
                    // rather than being added to it. With coins chosen this
                    // drains exactly those, which is how somebody empties one
                    // source deliberately.
                    Sending::Everything => {
                        builder.drain_wallet();
                        builder.drain_to(payee.to.script_pubkey());
                    }
                }
            }
            builder.finish().map_err(|e| {
                // Otherwise the message reports less money than the balance
                // above it shows, with no explanation for the difference.
                if waiting_on_a_block {
                    anyhow!("{e}. Coins from a payment that has not confirmed yet cannot be spent.")
                } else {
                    anyhow!("{e}")
                }
            })?
        };

        // Laying out change revealed an address on the internal keychain.
        // Persist it: an unwatched change address is money the wallet cannot
        // see afterwards.
        account.persist()?;

        let fee = psbt
            .fee()
            .map_err(|e| anyhow!("could not work out the fee: {e}"))?;
        // Read back off the built transaction rather than trusting what was
        // asked for: "everything" is only known once the fee is worked out,
        // and a payee paid twice on one transaction appears once here.
        let (payees, change) = super::send::split_outputs_of(&psbt.unsigned_tx, &account.wallet);

        let data = super::send::data_in(&psbt.unsigned_tx);

        Ok(super::send::Plan {
            psbt,
            from: draft.from,
            payees,
            fee,
            change,
            data,
            // An ordinary payment replaces nothing.
            replaces: None,
            was_fee: None,
            cancels: false,
        })
    }

    /// What the last block actually paid, in sat/vB.
    ///
    /// The only fee estimate available without asking anyone: kyoto downloads
    /// the block at the tip and works the rate out from its coinbase — total
    /// output minus subsidy, over weight. No server is told a payment is
    /// coming, which is the whole point.
    ///
    /// It is an average, so a single enormous fee drags it up, and it
    /// describes the block that just closed rather than the one being bid for.
    /// Good enough to fill a field with; not good enough to hide where it came
    /// from, which is why the caller says.
    ///
    /// Costs a block download — up to four megabytes on mainnet — so callers
    /// should ask once per tip, not once per keystroke.
    pub async fn average_fee_at_tip(&self) -> Result<(u32, f64)> {
        let tip = self
            .requester
            .chain_tip()
            .await
            .map_err(|e| anyhow!("could not read the chain tip: {e}"))?;

        let rate = self
            .requester
            .average_fee_rate(tip.hash)
            .await
            .map_err(|e| anyhow!("could not fetch the last block: {e}"))?;

        Ok((tip.height, rate.to_sat_per_kwu() as f64 / 250.0))
    }

    /// Sign a plan with the key from the vault and hand it to the network.
    ///
    /// The secret arrives already decrypted and leaves nothing behind: the
    /// signing wallet exists for the length of this call.
    ///
    /// Broadcast first, then record locally. A transaction no peer accepted is
    /// not a transaction, and showing it as pending would be a lie the wallet
    /// then has to walk back.
    /// The signing policy for a path, in the form a Ledger asks for.
    ///
    /// Read off the descriptor the wallet already holds rather than kept
    /// separately: they describe the same account, and two records of one fact
    /// drift. `<0;1>/*` becomes `/**` — the same statement, both chains, in the
    /// notation the device's policy language uses.
    pub async fn device_policy(&self, script_type: super::accounts::ScriptType) -> Result<String> {
        let portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter()
            .find(|a| a.script_type == script_type)
            .context("that derivation path is not part of this wallet")?;
        let descriptor = account
            .wallet
            .public_descriptor(bdk_wallet::KeychainKind::External)
            .to_string();
        Ok(crate::hardware::policy_from_descriptor(&descriptor))
    }

    /// The master fingerprint this wallet's keys belong to, when its
    /// descriptors name one.
    ///
    /// Read off the descriptor rather than out of the metadata, so a wallet
    /// imported before Sieve recorded anything about its device still knows
    /// which device it came from — the origin has said so all along.
    pub async fn key_fingerprint(&self) -> Option<String> {
        let portfolio = self.portfolio.lock().await;
        portfolio.accounts.iter().find_map(|account| {
            crate::hardware::fingerprint_of(
                &account
                    .wallet
                    .public_descriptor(bdk_wallet::KeychainKind::External)
                    .to_string(),
            )
        })
    }

    /// Finish a payment somebody else signed, and broadcast it.
    ///
    /// The counterpart to `sign_and_send` for a wallet that holds no keys: the
    /// signatures came from a device or a file, and all that is left is to
    /// finalise, check the result is a real transaction, and announce it.
    ///
    /// **Finalising is done by us, from our own descriptors.** A device hands
    /// back partial signatures; turning those into script witnesses is the
    /// wallet's job, and doing it here means a file that claims to be finished
    /// is checked rather than believed.
    pub async fn finalize_and_send(
        &self,
        mut plan: super::send::Plan,
    ) -> Result<(bdk_wallet::bitcoin::Txid, Summary)> {
        let mut portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter_mut()
            .find(|a| a.script_type == plan.from)
            .context("that derivation path is not part of this wallet")?;

        if !account
            .wallet
            .finalize_psbt(&mut plan.psbt, bdk_wallet::SignOptions::default())
            .map_err(|e| anyhow!("the signed payment could not be finished: {e}"))?
        {
            bail!(
                "that payment is not completely signed yet. Every input needs a signature \
                 before it can be broadcast."
            );
        }

        let tx = plan
            .psbt
            .extract_tx()
            .map_err(|e| anyhow!("the signed transaction is not valid: {e}"))?;
        let txid = tx.compute_txid();

        self.requester
            .submit_package(tx.clone())
            .await
            .map_err(|e| anyhow!("the transaction could not be announced: {e}"))?;

        let seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        account.wallet.apply_unconfirmed_txs([(tx, seen)]);
        account.persist()?;

        let summary = Summary::from_portfolio(&mut portfolio)?;
        Ok((txid, summary))
    }

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
        // `submit_package` returns once the transaction is queued and
        // announced, not once anybody accepted it — and nobody ever says they
        // did. This error means the announcement could not be made at all;
        // the previous wording claimed no peer accepted it, which is a thing
        // this call is in no position to know.
        self.requester
            .submit_package(tx.clone())
            .await
            .map_err(|e| anyhow!("the transaction could not be announced: {e}"))?;

        let seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        account.wallet.apply_unconfirmed_txs([(tx, seen)]);
        account.persist()?;

        let summary = Summary::from_portfolio(&mut portfolio)?;
        Ok((txid, summary))
    }

    /// Announce an unconfirmed transaction again.
    ///
    /// Worth having because a broadcast reaches exactly one peer: kyoto picks
    /// a random one and announces to it, which is plenty for an ordinary
    /// payment — every node accepts those — and is a coin toss for anything a
    /// peer might refuse on policy. A transaction carrying data is the first
    /// such thing Sieve can build, and the refusal is silent: BIP-61 `reject`
    /// messages are gone, so a peer that will not relay simply does not, and
    /// nothing distinguishes that from being ignored.
    ///
    /// Observed, not theorised: the first transaction Sieve sent carrying an
    /// `OP_RETURN` never reached a mempool, and one press of this put it
    /// there. Ordinary payments from the same wallet had always propagated on
    /// the first attempt.
    ///
    /// Each call is another peer told this transaction is probably ours, which
    /// is why this is asked for rather than done on a timer.
    pub async fn rebroadcast(
        &self,
        txid: &str,
        script_type: super::accounts::ScriptType,
    ) -> Result<()> {
        let txid: bdk_wallet::bitcoin::Txid = txid
            .parse()
            .map_err(|_| anyhow!("that is not a transaction id"))?;

        let portfolio = self.portfolio.lock().await;
        let account = portfolio
            .accounts
            .iter()
            .find(|a| a.script_type == script_type)
            .context("that derivation path is not part of this wallet")?;
        let tx = account
            .wallet
            .get_tx(txid)
            .context("this wallet does not hold that transaction")?;
        if tx.chain_position.is_confirmed() {
            bail!("that payment is already in a block");
        }
        let tx = tx.tx_node.tx.as_ref().clone();
        drop(portfolio);

        self.requester
            .submit_package(tx)
            .await
            .map_err(|e| anyhow!("the transaction could not be announced: {e}"))?;
        Ok(())
    }

    /// Who is connected, right now.
    ///
    /// Split out from `chain_info` because that one waits on the chain tip and
    /// a block header, which during a header download can take as long as the
    /// response timeout — thirty seconds over Tor. The peer list was therefore
    /// stale exactly when it was most interesting, and only filled in once the
    /// sync had finished. This asks the node for its own connection table and
    /// returns.
    pub async fn peers(&self) -> Vec<PeerInfo> {
        let Ok(peers) = self.requester.peer_info().await else {
            return Vec::new();
        };
        distinct_peers(peers)
    }

    /// The hash of the block at a height, for pinning a resume point.
    ///
    /// A read of the node's own memory, not the network. This replaced a store
    /// that kept the entire header chain on disk: the chain was only ever
    /// consulted for this one hash, because the library discards any header
    /// snapshot it is given.
    pub async fn header_hash(&self, height: u32) -> Option<bdk_wallet::bitcoin::BlockHash> {
        match self.requester.get_header(height).await {
            Ok(Some(header)) => Some(header.header.block_hash()),
            _ => None,
        }
    }

    /// Await the next progress event. `None` when the node has stopped.
    ///
    /// Silence is itself reportable: if nothing arrives for a while the caller
    /// gets `Waiting`, so the UI can distinguish "working" from "hung" instead
    /// of spinning against a label that never changes.
    pub async fn next_progress(&self) -> Option<Progress> {
        let mut info = self.info.lock().await;
        loop {
            let event = match tokio::time::timeout(self.quiet, info.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => return None,
                Err(_elapsed) => {
                    // Silence after the first sync is the normal resting state —
                    // the node is simply waiting for the next block. Reporting
                    // that as "waiting for peers" made a finished wallet look
                    // broken.
                    if self.synced.load(Ordering::Relaxed) {
                        return Some(Progress::Synced);
                    }
                    // Mid-scan silence is not the same thing. The node is working
                    // through filters and simply has nothing to announce; saying
                    // "waiting for peers" claims a problem that is not there, and
                    // hides the progress already made.
                    if self.scanning.load(Ordering::Relaxed)
                        && let Some(last) = self.last_progress.lock().unwrap().clone()
                    {
                        return Some(last);
                    }
                    return Some(Progress::Waiting);
                }
            };
            // Peer churn is constant and orthogonal to scan progress. Once
            // scanning has started, a handshake must not overwrite the status with
            // something that reads like going backwards — the peer count has its
            // own row for that.
            let scanning = self.scanning.load(Ordering::Relaxed);
            return Some(match event {
                // The tail of a scan: the filters are all in and what remains is
                // fetching the blocks they matched, which is the only work left
                // and the only thing worth saying.
                Info::BlockReceived(_) if scanning && self.filters_done.load(Ordering::Relaxed) => {
                    let read = self.blocks_read.fetch_add(1, Ordering::Relaxed) + 1;
                    // The count, never the hash. A block that matched this
                    // wallet's filters is a block holding this wallet's
                    // transactions, and naming it in a log hands that to
                    // whoever the log is shared with — which, for a log, is
                    // routinely somebody helping debug something else.
                    tracing::debug!(read, "reading a block that matched a filter");
                    Progress::Blocks(read)
                }
                Info::SuccessfulHandshake | Info::ConnectionsMet | Info::BlockReceived(_)
                    if scanning =>
                {
                    // Mid-scan a matched block is one of many and arrives between
                    // filters; letting it speak would flicker against the bar.
                    if matches!(event, Info::BlockReceived(_)) {
                        self.blocks_read.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
                Info::SuccessfulHandshake => Progress::Connecting,
                Info::ConnectionsMet => Progress::Connected,
                Info::Progress(p) => {
                    self.scanning.store(true, Ordering::Relaxed);
                    let fraction = p.fraction_complete() as f64;
                    // What separates "a block matched along the way" from "the
                    // filters are done and this is the last of the work".
                    self.filters_done.store(fraction >= 1.0, Ordering::Relaxed);
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
            // Remembered, so a quiet spell can report the last real state rather
            // than inventing a problem.
            .inspect(|progress| {
                *self.last_progress.lock().unwrap() = Some(progress.clone());
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
            bdk_kyoto::Warning::NeedConnections {
                connected,
                required,
            } => Notice::Peers {
                connected,
                required,
            },
            // Nothing a person can act on, and constant during normal peer
            // churn. Showing these as a standing message trains people to
            // ignore the row that will one day matter.
            bdk_kyoto::Warning::CouldNotConnect
            | bdk_kyoto::Warning::PeerTimedOut
            | bdk_kyoto::Warning::NoCompactFilters
            | bdk_kyoto::Warning::UnsolicitedMessage
            | bdk_kyoto::Warning::EvaluatingFork => Notice::Ignorable,

            bdk_kyoto::Warning::PotentialStaleTip => {
                Notice::Problem("No new blocks for a while. The connection may be stale.".into())
            }
            bdk_kyoto::Warning::TransactionRejected { payload } => Notice::Problem(format!(
                "A transaction was rejected by the network: {payload:?}"
            )),
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

    /// How many blocks this session has fetched because a filter matched.
    ///
    /// Recorded when a scan completes, so the next scan of the same wallet has
    /// a measured total to draw a bar against.
    pub fn blocks_read(&self) -> usize {
        self.blocks_read.load(Ordering::Relaxed)
    }

    /// Stop the light client, and make sure it actually stopped.
    ///
    /// Asked first, so a node that can stop itself does — it closes its
    /// sockets on the way out, which is a politer goodbye to eight peers than
    /// having the connections vanish. Then the runtime goes, because asking is
    /// not enough on its own: kyoto's peers outlive the node that spawned them
    /// and spin when it is gone. `NodeHost` has the detail.
    pub fn shutdown(&self) {
        let _ = self.requester.shutdown();
        self.node.stop();
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

    /// This process's own processor time, in milliseconds.
    ///
    /// `/proc/self/stat`, because the question is what *this* program is
    /// burning and there is no portable way to ask. Fields 14 and 15 are user
    /// and system jiffies; the two before them are inside the comm field,
    /// which can itself contain spaces and brackets, so the split is after the
    /// last `)` rather than on whitespace.
    #[cfg(target_os = "linux")]
    fn cpu_ms() -> u64 {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("this is Linux");
        let after_comm = stat.rsplit_once(')').expect("a comm field").1;
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        let jiffies: u64 =
            fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime");
        // `CLK_TCK` is 100 everywhere Linux runs in practice, and this is a
        // test measuring a difference of seconds against a threshold of
        // hundreds of milliseconds — precision beyond this is not the thing
        // that would make it wrong.
        jiffies * 10
    }

    /// A light client that has been shut down stops using the processor.
    ///
    /// **The regression this exists for was reported as "turning Tor on costs
    /// 9% of a CPU, and turning it off does not give it back".** Both toggles
    /// restart the session; the restart left kyoto's peer tasks running with
    /// their node gone, and a stranded peer spins on its closed channel for up
    /// to two hours. `NodeHost` has the mechanism written out.
    ///
    /// It is measured rather than reasoned about because the reasoning had
    /// already been wrong twice: the first fix stopped re-arming a closed
    /// update channel, the second aborted the task `managed_start` returns,
    /// and neither touched the tasks actually burning the time. What settles
    /// it is a number.
    ///
    /// Real peers on signet, because the bug needs a peer: a node that
    /// connected to nobody has nothing to strand, and would pass this test
    /// with the fix reverted. That is also why it asserts it *had* peers
    /// first — a network failure has to fail loudly here rather than quietly
    /// passing.
    ///
    /// `cargo test --release -- --ignored --nocapture stranded_peers`
    #[test]
    #[ignore = "connects to real peers on signet"]
    #[cfg(target_os = "linux")]
    fn stranded_peers_do_not_go_on_spinning_after_a_shutdown() {
        use std::time::Duration;

        let network = bdk_wallet::bitcoin::Network::Signet;
        let outer = tokio::runtime::Runtime::new().expect("a runtime");

        let mut builder = bdk_kyoto::bip157::Builder::new(network);
        for ip in outer.block_on(resolve_seeds_directly(network, REQUIRED_PEERS as usize * 3)) {
            builder = builder.add_peer(bdk_kyoto::bip157::TrustedPeer::from_ip(ip));
        }
        let (node, client) = builder.required_peers(REQUIRED_PEERS).build();

        let host = NodeHost::start(node).expect("a runtime for the node");
        let requester = client.requester;

        // Long enough for a handshake with several peers. The node is left to
        // get on with headers; what matters is that peer tasks exist.
        let peers = outer.block_on(async {
            let mut seen = Vec::new();
            for _ in 0..30 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if let Ok(info) = requester.peer_info().await
                    && !info.is_empty()
                {
                    seen = info;
                    break;
                }
            }
            seen
        });
        assert!(
            !peers.is_empty(),
            "no peer ever connected, so there was nothing to strand — check the \
             network before reading anything into this test"
        );
        println!("{} peers connected", peers.len());

        let _ = requester.shutdown();
        host.stop();

        // Settle first: the runtime is being torn down, and the tail of that
        // is work this test is not asking about.
        outer.block_on(async { tokio::time::sleep(Duration::from_secs(2)).await });

        const WINDOW: Duration = Duration::from_secs(5);
        let before = cpu_ms();
        outer.block_on(async { tokio::time::sleep(WINDOW).await });
        let spent = cpu_ms() - before;

        println!("{spent} ms of processor time in the {WINDOW:?} after shutdown");

        // A stranded peer spins flat out, so the failing case is hundreds of
        // milliseconds per peer per second and the fixed case is close to
        // nothing. The threshold sits far enough above idle that a busy build
        // machine does not fail it, and far enough below one spinning task
        // that the bug cannot hide under it.
        assert!(
            spent < 500,
            "{spent} ms of processor time in {WINDOW:?} with the node shut down: \
             something kyoto spawned is still running. See `NodeHost`."
        );
    }

    /// Every chain the interface offers can be seeded.
    ///
    /// The port lived in a `match` with a catch-all that returned no
    /// addresses, so offering a new chain without adding its port produced a
    /// wallet that connected to nobody and reported nothing — which looks
    /// exactly like a slow network rather than a missing line of code.
    #[test]
    fn every_offered_chain_has_a_port() {
        for network in crate::wallet::NETWORKS {
            assert!(
                p2p_port(network).is_some(),
                "{network} is offered in the interface with no p2p port"
            );
        }
        // The ports are the chains' own, and no two share one.
        assert_eq!(p2p_port(bdk_wallet::bitcoin::Network::Bitcoin), Some(8333));
        assert_eq!(p2p_port(bdk_wallet::bitcoin::Network::Signet), Some(38333));
        assert_eq!(
            p2p_port(bdk_wallet::bitcoin::Network::Testnet4),
            Some(48333)
        );
        assert_eq!(p2p_port(bdk_wallet::bitcoin::Network::Regtest), None);
    }

    /// Does a chain actually have filter-serving peers to find?
    ///
    /// Opted into by name because it goes to the network, which the rest of
    /// the suite never does. It is the check that answers the question
    /// testnet4 was added on: the DNS seeders are asked for
    /// `NODE_NETWORK | NODE_COMPACT_FILTERS` nodes with the `x49` prefix, and
    /// this reports how many each chain gives back.
    ///
    /// A seeder's answer is a crawler's record of what a node *advertised*,
    /// not proof it will serve a filter when dialled — so this is a floor, and
    /// the only real test is a scan that gets past the filter-header quarter.
    /// What it does prove is the plumbing: a chain with no port or no seed
    /// list returns zero here, silently, which is the bug it was written for.
    ///
    /// `cargo test --release -- --ignored --nocapture filter_peers`
    #[test]
    #[ignore = "asks DNS seeders on the real network"]
    fn filter_peers_can_be_found_for_every_offered_chain() {
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        for network in crate::wallet::NETWORKS {
            let found = runtime.block_on(resolve_seeds_directly(network, 64));
            println!(
                "{network}: {} filter-serving addresses, need {REQUIRED_PEERS}",
                found.len()
            );
            assert!(
                !found.is_empty(),
                "{network} is offered and no seeder answered — check its port \
                 and its seed list before believing the network is empty"
            );
        }
    }

    /// A checkpoint the resume can start from, or refuse to.
    fn meta_scanned(to: Option<u32>, hash: Option<&str>) -> Meta {
        let mut meta = Meta::new(
            bdk_wallet::bitcoin::Network::Signet,
            crate::wallet::checkpoints(bdk_wallet::bitcoin::Network::Signet)[0],
            vec![crate::wallet::accounts::ScriptType::Taproot],
            crate::wallet::accounts::ScriptType::Taproot,
            None,
            false,
        );
        meta.scanned_to = to;
        meta.scanned_hash = hash.map(str::to_owned);
        meta
    }

    #[test]
    fn a_scan_resumes_only_from_a_point_it_can_prove() {
        // This is where the next scan starts, and the node will not take a
        // height without the hash that belongs to it. Every way of being
        // unsure has to come back as "start at the birthday": a scan that
        // begins too late skips blocks, and skipped blocks are money this
        // wallet never learns about.
        let birthday = crate::wallet::checkpoints(bdk_wallet::bitcoin::Network::Signet)[0].height;
        let real = "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6";

        // Nothing recorded yet.
        assert!(resume_point(&meta_scanned(None, Some(real))).is_none());
        // A height with no hash to go with it.
        assert!(resume_point(&meta_scanned(Some(birthday + 5_000), None)).is_none());
        // A hash that is not one.
        assert!(resume_point(&meta_scanned(Some(birthday + 5_000), Some("nonsense"))).is_none());
        // At or behind the birthday, which is where a scan would start anyway.
        assert!(resume_point(&meta_scanned(Some(birthday), Some(real))).is_none());
        assert!(resume_point(&meta_scanned(Some(birthday - 1), Some(real))).is_none());

        // And a real one, which is the only case that resumes.
        let ahead = meta_scanned(Some(birthday + 5_000), Some(real));
        let (height, hash) = resume_point(&ahead).expect("a proven point must be used");
        assert_eq!(height, birthday + 5_000);
        assert_eq!(hash.to_string(), real);
    }

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
        // Nor for the tail: nothing says how many matched blocks are left.
        assert!(Progress::Blocks(3).fraction().is_none());
    }

    #[test]
    fn the_block_phase_says_what_it_is_doing() {
        // The gap between a full bar and a balance was silent, and silence
        // after 100% reads as a hang.
        assert!(Progress::Blocks(1).label().contains("1 block"));
        assert!(Progress::Blocks(7).label().contains("7 so far"));
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
    /// The median of the last eleven block timestamps.
    ///
    /// Not a curiosity: this is the clock consensus actually uses. Timelocks
    /// and BIP-113 are measured against it, and it can be well behind the
    /// tip's own timestamp.
    pub median_time_past: Option<u64>,
}

/// The block subsidy at a height, in satoshis.
///
/// Halves every 210,000 blocks and reaches zero after 33 of them, which is why
/// this shifts rather than divides — a shift past the width is not defined the
/// way the schedule is.
pub fn subsidy_sats(height: u32) -> u64 {
    let epoch = height / HALVING_INTERVAL;
    if epoch >= 33 {
        0
    } else {
        (50 * 100_000_000) >> epoch
    }
}

/// The height of the next halving after this one.
pub fn next_halving(height: u32) -> u32 {
    (height / HALVING_INTERVAL + 1) * HALVING_INTERVAL
}

/// Everything the schedule has issued up to and including this height.
///
/// The schedule, not the chain: a few miners have claimed less than they were
/// owed, so the real figure is slightly lower. Said as "by the schedule"
/// wherever it is shown.
pub fn issued_sats(height: u32) -> u64 {
    let mut issued = 0u64;
    let epoch = height / HALVING_INTERVAL;
    for past in 0..epoch {
        issued += HALVING_INTERVAL as u64 * subsidy_sats(past * HALVING_INTERVAL);
    }
    // Blocks are counted from zero, so the current epoch has one more than the
    // remainder suggests.
    let into_epoch = (height % HALVING_INTERVAL) as u64 + 1;
    issued + into_epoch * subsidy_sats(height)
}

/// Blocks between halvings.
pub const HALVING_INTERVAL: u32 = 210_000;

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
/// Where a previous scan of this wallet got to, if it can be proved.
///
/// Resuming needs the hash of the block to resume at, and the only place that
/// hash exists locally is the stored header chain. No headers, no resume —
/// which is the safe direction: starting again costs time, while starting too
/// late costs coins.
fn resume_point(meta: &Meta) -> Option<(u32, bdk_wallet::bitcoin::BlockHash)> {
    let scanned_to = meta.scanned_to?;
    if scanned_to <= meta.birthday_height {
        return None;
    }
    let hash = meta.scanned_hash.as_ref()?.parse().ok()?;
    tracing::info!(
        from = scanned_to,
        birthday = meta.birthday_height,
        "resuming a scan that was interrupted"
    );
    Some((scanned_to, hash))
}

/// The port peers listen on for a chain.
///
/// `None` for a chain with no public network to seed from. Worth being its own
/// function rather than a `match` inside the resolver: a missing arm there
/// returned *no addresses at all*, so a newly offered chain would have started
/// with nothing to connect to and simply sat there — no error, no peers, a
/// progress bar at zero. `every_offered_chain_has_a_port` is the guard.
fn p2p_port(network: bdk_wallet::bitcoin::Network) -> Option<u16> {
    match network {
        bdk_wallet::bitcoin::Network::Bitcoin => Some(8333),
        bdk_wallet::bitcoin::Network::Signet => Some(38333),
        bdk_wallet::bitcoin::Network::Testnet => Some(18333),
        bdk_wallet::bitcoin::Network::Testnet4 => Some(48333),
        // A chain on this machine is not seeded over DNS.
        _ => None,
    }
}

/// Ask the ordinary resolver for peers that serve compact filters.
///
/// The same hostnames and service-bit prefixes the Tor path uses — `x49` is
/// `NODE_NETWORK | NODE_COMPACT_FILTERS` — but over plain DNS, where one query
/// returns every address the seeder has rather than the single one Tor's
/// RESOLVE gives back.
async fn resolve_seeds_directly(
    network: bdk_wallet::bitcoin::Network,
    wanted: usize,
) -> Vec<std::net::IpAddr> {
    let Some(port) = p2p_port(network) else {
        return Vec::new();
    };

    let mut found: Vec<std::net::IpAddr> = Vec::new();
    for host in crate::tor::seeds(&network.to_string()) {
        if found.len() >= wanted {
            break;
        }
        match tokio::net::lookup_host((host.as_str(), port)).await {
            Ok(addresses) => {
                for address in addresses {
                    let ip = address.ip();
                    if !found.contains(&ip) {
                        found.push(ip);
                    }
                }
            }
            Err(e) => tracing::debug!(%host, %e, "a seed did not resolve"),
        }
    }
    found.truncate(wanted);
    found
}

/// One entry per machine, from kyoto's one entry per connection.
fn distinct_peers(
    peers: Vec<(
        bdk_wallet::bitcoin::p2p::address::AddrV2,
        bdk_wallet::bitcoin::p2p::ServiceFlags,
    )>,
) -> Vec<PeerInfo> {
    // Every connection as the node describes it, before deduplication. When
    // eight connections collapse into one row, this is the only way to tell
    // whether that is eight connections to one machine or eight machines the
    // node describes identically — and those want opposite fixes.
    tracing::debug!(
        connections = peers.len(),
        raw = ?peers.iter().map(|(a, s)| (format_address(a), s.to_string())).collect::<Vec<_>>(),
        "peer table as the node reports it"
    );

    let mut seen = std::collections::HashSet::new();
    peers
        .into_iter()
        .map(|(address, services)| (format_address(&address), services))
        .filter(|(address, _)| seen.insert(address.clone()))
        .map(|(address, services)| PeerInfo {
            address,
            // `NONE` is "not reported", which is most of the time — not "does
            // not serve filters". Saying otherwise would libel the peer.
            serves_filters: (services != bdk_wallet::bitcoin::p2p::ServiceFlags::NONE)
                .then(|| services.has(bdk_wallet::bitcoin::p2p::ServiceFlags::COMPACT_FILTERS)),
        })
        .collect()
}

/// A remembered address, as kyoto wants it.
///
/// Both kinds: a plain address, and an onion one, which is the whole point of
/// remembering peers found while running over Tor — those are exactly the
/// peers reachable the next time Tor is on.
fn trusted_peer(address: &str) -> Option<bdk_kyoto::bip157::TrustedPeer> {
    use bdk_wallet::bitcoin::p2p::address::AddrV2;

    if crate::tor::onion::looks_like_onion(address) {
        let pubkey = crate::tor::onion::decode(address)?;
        return Some(bdk_kyoto::bip157::TrustedPeer::new(
            AddrV2::TorV3(pubkey),
            None,
            bdk_wallet::bitcoin::p2p::ServiceFlags::NONE,
        ));
    }

    // Written with brackets when it is IPv6, the way the list shows it.
    let trimmed = address.trim_start_matches('[').trim_end_matches(']');
    let ip: std::net::IpAddr = trimmed.parse().ok()?;
    Some(bdk_kyoto::bip157::TrustedPeer::from_ip(ip))
}

fn format_address(address: &bdk_wallet::bitcoin::p2p::address::AddrV2) -> String {
    use bdk_wallet::bitcoin::p2p::address::AddrV2;
    match address {
        AddrV2::Ipv4(ip) => ip.to_string(),
        AddrV2::Ipv6(ip) => format!("[{ip}]"),
        // The real address, not the word "onion": it is what identifies the
        // peer, what gets remembered, and what someone would compare against
        // their own node.
        AddrV2::TorV3(pubkey) => crate::tor::onion::encode(pubkey),
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
            info.peers = distinct_peers(peers);
        }

        // Remember the peers connected while filters are syncing.
        //
        // Every one of them serves compact filters, by construction rather
        // than by hearsay: kyoto drops any peer whose version message lacks
        // NODE_COMPACT_FILTERS | NODE_NETWORK as soon as it is past the
        // block-header phase. So "still connected while filters are coming in"
        // is proof, where the service flags `peer_info` reports are absent for
        // most connections and prove nothing either way.
        //
        // Not gated on a finished sync. A recovery scan can run for an hour,
        // and the peers doing that work are exactly the ones worth having next
        // time — waiting for the end meant learning nothing from a scan that
        // was interrupted, which is how a wallet ends up with no pinned peers
        // at all.
        if self.scanning.load(Ordering::Relaxed) && !info.peers.is_empty() {
            let addresses: Vec<String> = info.peers.iter().map(|p| p.address.clone()).collect();
            tracing::debug!(count = addresses.len(), "remembering filter-serving peers");
            crate::peers::remember(self.network, &addresses);
        }

        // Eleven headers. Cached against the tip they describe: over Tor
        // that is eleven circuit round trips, and repeating them every refresh
        // both wasted the network and stretched the window in which a result
        // for the previous wallet could still arrive.
        let cached = *self.median_time.lock().unwrap();
        match cached {
            Some((height, median)) if height == tip.height => {
                info.median_time_past = Some(median);
            }
            _ => {
                let mut recent = Vec::with_capacity(11);
                for back in 0..11u32 {
                    let Some(height) = tip.height.checked_sub(back) else {
                        break;
                    };
                    match self.requester.get_header(height).await {
                        Ok(Some(header)) => recent.push(header.header.time as u64),
                        _ => break,
                    }
                }
                if recent.len() == 11 {
                    recent.sort_unstable();
                    info.median_time_past = Some(recent[5]);
                    *self.median_time.lock().unwrap() = Some((tip.height, recent[5]));
                }
            }
        }

        if let Ok(rate) = self.requester.broadcast_min_feerate().await {
            info.min_relay_fee = Some(rate.to_sat_per_kwu() as f64 / 250.0);
        }

        Ok(info)
    }
}

#[cfg(test)]
mod issuance_tests {
    use super::*;

    /// Checked against the schedule everyone knows, because a wrong subsidy
    /// would put a wrong halving date and a wrong supply figure on screen.
    #[test]
    fn the_subsidy_halves_on_schedule() {
        assert_eq!(subsidy_sats(0), 50 * 100_000_000);
        assert_eq!(subsidy_sats(209_999), 50 * 100_000_000);
        assert_eq!(subsidy_sats(210_000), 25 * 100_000_000);
        assert_eq!(subsidy_sats(420_000), 1_250_000_000);
        assert_eq!(subsidy_sats(630_000), 625_000_000);
        assert_eq!(subsidy_sats(840_000), 312_500_000, "the 2024 halving");
        assert_eq!(subsidy_sats(1_050_000), 156_250_000);
        // The schedule ends rather than going negative or wrapping.
        assert_eq!(subsidy_sats(33 * HALVING_INTERVAL), 0);
        assert_eq!(subsidy_sats(u32::MAX), 0);
    }

    #[test]
    fn the_next_halving_is_the_next_multiple() {
        assert_eq!(next_halving(0), 210_000);
        assert_eq!(next_halving(839_999), 840_000);
        assert_eq!(next_halving(840_000), 1_050_000);
        assert_eq!(next_halving(964_726), 1_050_000);
    }

    #[test]
    fn issuance_matches_the_known_figures() {
        // The first halving: 210,000 blocks at 50, counting the genesis block.
        assert_eq!(issued_sats(209_999), 210_000 * 50 * 100_000_000);

        // Never more than the cap, at any height, ever.
        assert!(issued_sats(u32::MAX) < 21_000_000 * 100_000_000);
        assert!(issued_sats(33 * HALVING_INTERVAL) < 21_000_000 * 100_000_000);

        // The 2024 halving left the schedule at 19,687,500 exactly.
        assert_eq!(issued_sats(839_999), 1_968_750_000_000_000);

        // And a little over twenty million by the height this was written
        // against — the twenty-million mark falls in 2026, not before it.
        let issued = issued_sats(964_726) as f64 / 100_000_000.0;
        assert!((20_000_000.0..20_200_000.0).contains(&issued), "{issued}");
    }
}
