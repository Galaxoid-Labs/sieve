# Sieve development plan

Nine milestones in dependency order. Each ends with something usable. Mainnet is gated
behind the last one.

A rendered version of this plan: https://claude.ai/code/artifact/27f20a16-bce2-44a6-aa84-da33aa11112d

## Decide before M1

These get baked into the vault format, the descriptors, or both. Changing them later means
a migration and a re-scan.

| Decision | Recommendation | Why |
|---|---|---|
| Script type | **BIP86 taproot** | Single-sig key-path spends are indistinguishable on-chain. Costs acceptance at a few older services that reject `bc1p`. |
| Dev network | **Signet** (regtest for tests) | Real block times and enough `NODE_COMPACT_FILTERS` peers to exercise sync honestly. |
| Phrase length | **12 words**, not user-selectable | 128 bits is beyond brute force; transcription error is the realistic loss vector. |
| Password vs passphrase | **Both, named distinctly** | The *password* always encrypts the file. The *passphrase* is the optional BIP-39 25th word. A wrong password errors; a wrong passphrase silently derives an empty wallet, so the UI must never blur them. |
| Wallet count | **One per vault** | Keeps unlock, sync, and the signer singular. The header carries a version, so multi-account stays open. |

## Milestones

### M0 — Scaffold — SHIPPED
Adwaita shell, vault (Argon2id KEK wrapping a random DEK, XChaCha20-Poly1305, header bound
as AAD), atomic writes, process hardening, six vault tests.

### M1 — Wallet creation and unlock — MOSTLY DONE
*Done when: create a wallet, close the app, reopen, unlock back into the same wallet.*

- [x] First-run detection routes to onboarding instead of unlock
- [x] 12-word mnemonic via `bdk_wallet::keys::bip39`
- [x] Display-once screen; three-word verification challenge before the wallet is created
- [x] Passphrase with confirmation and a minimum length
- [x] Seal to `vault.sieve`, derive BIP86 descriptors, initialise the BDK SQLite store
- [x] Unlock loads watch-only from the database; a lost database is rebuilt from the vault
- [x] KDF retuned to 256 MiB / 3 passes (~0.7s) after measuring; params travel in the header
- [x] Database is owner-only — it holds the xpub and full transaction graph
- [x] Restore from a recovery phrase, a WIF key, or (stub) a descriptor
- [x] Optional BIP-39 passphrase on restore, kept distinct from the wallet password
- [x] All four standard derivation paths searched on import, with a per-path breakdown
- [x] Mainnet selectable on import behind an explicit unreviewed-software acknowledgement
- [ ] Offer a BIP-39 passphrase when *creating* a wallet, not only when importing one.
      Creation passes `None`, so a wallet made here can never have one. Two things must be
      right first: the phrase step has to say the words alone will no longer restore the
      wallet, and verification has to ask for the passphrase back as well — one written down
      wrong is indistinguishable from a correct one until the money is gone, since it derives
      a valid empty wallet rather than an error.
- [x] Show the recovery phrase again, for backing it up later — `ui/reveal.rs`, reached from
      the Recovery phrase row in preferences. Asks for the password, because the vault is the
      only place the phrase exists. The row is insensitive until the wallet is unlocked, and
      the words are dropped when preferences closes. A wallet imported from a key shows that
      key, with copy saying so, rather than appearing broken.
- [ ] Descriptor / xpub watch-only import
- [ ] Signer worker owning the decrypted descriptor, one message at a time

The mnemonic gets the same treatment as `Passphrase`: `Zeroizing`, redacted `Debug`, never
crosses a component boundary as a message.

### M2 — Compact filter sync
*Done when: a funded signet wallet shows the right balance after a cold start.*

- `CbfBuilder` wiring; peer discovery via `lookup_host` (prefixes seeders with `x849`)
- `CbfNode::run()` on its thread; `CbfClient::update()` awaited in a Relm4 command
- Apply each `Update`, persist the changeset
- `ScanType::Recovery` with lookahead sized to wallet history — undersizing silently misses
  transactions rather than erroring
- Progress from the `Info` stream; `Warning` stream to an `adw::Banner`

### M3 — Receive
`reveal_next_address` with persistence, QR into a `gtk::DrawingArea` (theme-aware — see rule 5),
BIP-21 URIs, issued-address list with used/unused state.

### M4 — Send
- [x] Address and amount validation — wrong-network addresses get their own message, and
      amounts are read with integer arithmetic in whichever unit is on display.
- [x] Watch-only PSBT construction, so the form and the review cost nothing secret.
- [x] Password only at signing, in an `adw::AlertDialog` that restates every number.
- [x] Signing from the vault, checked against the account's descriptor first.
- [x] Broadcast via `Requester::submit_package`, then recorded locally as unconfirmed.
- [x] Drain the wallet ("Max"), where the fee comes out of the amount.
- [ ] Fee suggestion from `average_fee_rate`. Costs a full block download, which is why it is
      not simply on: the field is floored at what peers will relay and left to the person
      sending. A light client has no mempool and there is no fee API to ask.
- [ ] A BIP-39 passphrase at signing time, for wallets imported with one. Refused clearly for
      now rather than silently failing to finalize.
- [ ] Coin control and RBF.

### M5 — Transaction history
`adw::ActionRow` list, detail page on an `adw::NavigationView`, confirmation depth, fee paid,
pending and replaced states.

### M6 — Privacy controls
Tor via `socks5_proxy`, manual peer pinning with `only_configured_peers`, coin control,
BIP-329 labels, and an audit that nothing but Bitcoin p2p leaves the machine.

### M7 — Lock and key hygiene
Idle auto-lock, lock on suspend via logind `PrepareForSleep`, opt-in Secret Service storage
(labelled as convenience, not a boundary), FIDO2 `hmac-secret` as a second wrap, PSBT
export/import then HWI.

### M8 — Package and release — MAINNET GATE
Flatpak (the only real app-to-app isolation on Linux), `org.freedesktop.portal.Secret`,
AppStream metainfo and icon, reproducible builds, signed tags, external review of the vault
format and signing path.

**Mainnet stays unreachable from the UI until this milestone closes.**

## Running alongside

- regtest harness with `bitcoind -blockfilterindex=1` for integration tests
- CI: `cargo test`, `clippy -D warnings`, `cargo fmt --check`
- `cargo audit` and `cargo deny`
- Keep CLAUDE.md current as milestones land

## Known risks

- **bdk_kyoto is pre-1.0** — pin exact versions; expect M2 wiring to churn on upgrades
- **Mainnet recovery is slow** — needs honest progress, not a spinner that looks hung
- **Argon2 at 512 MiB** — comfortable on desktop, hostile on a 2 GB machine; measure before locking
- **X11 leaks keystrokes** — Wayland is the supported target; say so in the docs
