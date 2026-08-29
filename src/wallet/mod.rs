//! Wallet state and the background subsystems that feed it.
//!
//! Nothing here touches GTK. The split is deliberate: BDK objects and key
//! material stay on worker threads, and only plain data crosses back to the UI
//! as messages.
//!
//! Planned shape:
//!
//! - `signer` — a `relm4::Worker` owning the decrypted descriptor. A Worker
//!   handles one message at a time, which is exactly the serialization a signer
//!   wants. It receives PSBTs and returns signed PSBTs; the seed never leaves it.
//! - `node` — the `bdk_kyoto` BIP157/158 light client. `CbfNode::run()` detaches
//!   its own thread; `CbfClient::update()` is async and is awaited from a relm4
//!   command, applying each `Update` to the wallet.
//!
//! The BDK wallet itself is watch-only: `bdk_wallet::ChangeSet` persists only
//! `Descriptor<DescriptorPublicKey>`, so the SQLite store never contains private
//! keys. Browsing balances and building PSBTs needs no secret at all.
