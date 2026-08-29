# Sieve

A privacy-focused Bitcoin wallet for Linux.

Sieve is a light client that uses BIP157/158 compact block filters: it downloads filters and
matches them locally, so it never reveals which addresses belong to you. No server is told your
balance, your history, or your addresses.

Built with Rust, [Relm4](https://relm4.org) and libadwaita, so it looks and behaves like a
native GNOME application.

## Status

Early scaffold. The encrypted vault and the application shell work; wallet sync and spending
do not exist yet. Not usable with real funds.

## Building

Requires Rust 1.93+, GTK 4 and libadwaita development packages.

```sh
cargo run
```

## Security

The seed is sealed with XChaCha20-Poly1305 under a key derived from your passphrase with
Argon2id (512 MiB, 4 passes). The wallet runs watch-only from public descriptors; the seed is
decrypted only to sign.

## License

MIT OR Apache-2.0
