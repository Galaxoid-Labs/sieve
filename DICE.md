# Dice, and other entropy a person brings themselves

Design, not a plan. Nothing here exists yet — but unlike `NOTIFICATIONS.md`, the
verdict is build it, and the reason is five weeks old.

## Why this stopped being theoretical

In **July 2026** an attacker drained roughly **1,816 BTC — about $88 million — from
more than 5,200 addresses**, in waves, the first of which emptied 1,196 addresses in
41 minutes. The wallets were Coldcards.

The cause was not a broken cipher or a stolen phrase. A firmware integration error
in **March 2021** set a build flag that routed seed generation to a deterministic
software PRNG instead of the STM32 hardware random number generator. It shipped that
way for **five years**. Every affected device displayed twenty-four ordinary-looking
words, restored correctly, signed correctly, and produced a seed an attacker could
brute-force.

Nothing about a weak seed is visible. That is the entire problem: a wallet made from
128 bits and a wallet made from 30 look exactly alike, on screen and on paper and on
the chain, right up until somebody sweeps it.

From Coinkite's own advisory, the seeds that survived were the ones with **50 or more
independent dice rolls** (or a strong unique BIP-39 passphrase). And Coldcard's
standard dice mode **mixes** the rolls with the device RNG rather than replacing it —
so mixing alone was enough to defeat an RNG that was, for five years, worthless.

That is the case for this feature, and it is worth stating in the order it actually
happened: the argument for dice is not that `getrandom(2)` is suspect. It is that a
*wiring mistake around* a good RNG is invisible, has happened, and was survivable for
exactly the people who had brought their own randomness.

## What Sieve does today

`wallet::generate_mnemonic` asks for its own entropy:

```rust
let mut entropy = Zeroizing::new([0u8; 32]);
getrandom::fill(entropy.as_mut()).context("no entropy available from the OS")?;
Mnemonic::generate_with_entropy((count, Language::English), *entropy)
```

`getrandom::fill` is the `getrandom(2)` syscall on Linux — the same call the vault
uses for its salt, its nonces and its data key. There is one entropy source in the
program and this is it. BDK takes 32 bytes and uses the first `bits / 8`: sixteen for
a twelve-word phrase, all thirty-two for twenty-four.

**That function is the seam.** Everything below is a change to how those 32 bytes are
arrived at, and to nothing else.

## Three schemes, and why only one is right

### Replace — the rolls become the seed

`entropy = SHA256(rolls)`, the OS not consulted. This is the only scheme that lets a
person **verify the result completely**: they write down the rolls, and later, on a
different machine with different software, run

```sh
printf '%s' 53412661… | sha256sum
```

If it matches, Sieve used their entropy and nothing else — no adaptive substitution
is possible, because the output is a deterministic function of inputs they control.
SeedSigner does this; Coldcard offers it as an advanced mode with published
verification scripts.

**And it is still the wrong choice**, because it hands the user a way to make a
wallet weaker than the one they would have got for free. Sparrow's Craig Raw,
declining exactly this:

> We have many recent examples of Coldcard users losing funds by simply rolling a
> dice too few times.

Electrum is the stronger evidence: it **had** this option, from version 2.0 to
3.1.2, and **removed it**. A feature that gets built, shipped for a dozen releases,
and then withdrawn is telling you something that no amount of reasoning from first
principles will.

### Mix and prove it — commit-then-reveal

The obvious repair to plain mixing is to show the arithmetic so the user can check
it. It does not work, and the reason is worth writing down so nobody rebuilds it:

If Sieve is the thing being distrusted, it does not need a weak `os_bytes`. It picks
them *after* seeing the rolls:

```
os_bytes = SHA256(rolls) XOR a_seed_the_attacker_already_knows
```

Now `final = SHA256(rolls) XOR os_bytes` is a seed the attacker knows, and every
number displayed checks out perfectly. The fix is to commit to `SHA256(os_bytes)`
*before* accepting any rolls and reveal `os_bytes` afterwards — a protocol that is
sound, that nobody will perform, and that guards against an adversary who already
controls the process drawing the screen.

Electrum's old replace-mode carried a probabilistic verification scheme of this
family. It went out with the rest of it, and their maintainer's summary of the
replacement is that XOR-mixing is *"simpler and I think easier to write and
validate."*

### Mix — the OS bytes, XOR the rolls

```
entropy = os_bytes XOR SHA256(rolls)
```

**This is the one to build.** It cannot produce a phrase weaker than Sieve would have
produced unaided, whatever the rolls are: XOR with anything the attacker does not know
leaves the result unknown, and XOR with anything they do know leaves the other input
intact. There is no roll count at which this becomes dangerous, and no die so loaded
that it costs anything.

Electrum's open PR #8839 puts it exactly: *"As the extra entropy is simply XOR-ed in,
into the OS-generated randomness, **it cannot be used as a footgun**."*

## The mistake to avoid when reasoning about this

Replace-mode defends against a **malicious** implementation. Mixing defends against a
**broken** one. It is tempting to rank them by that and conclude replace-mode is
strictly stronger.

Ask instead which one has ever happened. RANDU. The Debian OpenSSL key generation bug
of 2008. The Kerberos RNG flaw of 1996. Coldcard, last month. **Every RNG failure that
has cost people money was a bug**, and a bug does not get to see the rolls and choose
its output in response — it simply emits garbage, which XOR annihilates. There is no
notable case of a wallet adaptively subverting user-supplied entropy.

So mixing covers the failure that occurs. Replace-mode additionally covers one that
never has, and introduces one that demonstrably does.

**Mouse movement is a different question and the answer is no.** It looks like a third
option and it is not: the kernel already harvests input event timing into the pool
`getrandom` draws from, so collecting it again in userspace adds essentially nothing.
Dice are entropy from outside the machine entirely. A progress bar that fills while
somebody waves a mouse would be the first thing in Sieve that implies more than it
does.

## What the field does

| Wallet | Dice | Scheme | Notes |
|---|---|---|---|
| **Electrum** | not in the GUI | — | had **replace** in 2.0–3.1.2, removed it; PR #8839 (open) re-adds XOR-mixed `extra_entropy` for CLI/RPC |
| **Sparrow** | no | — | request open since April 2024; dice-from-zero has *"significantly more disadvantages than advantages"* |
| **Coldcard** | yes | both, **mixed by default** | 50-roll minimum, 99 recommended; "Dice Rolls Only" is an advanced mode with published verification scripts |
| **SeedSigner** | yes | **replace** | 50 rolls for 12 words, 99 for 24 |
| **Feather** (Monero) | yes | **mixed** | *"system entropy in addition to entropy generated from rolls, to reduce the impact of human error"* |

Mixed is the majority position among the wallets that shipped it at all, and the two
projects that chose replace are both **dedicated air-gapped signing devices** whose
users are self-selected for exactly this care. Sieve is a desktop wallet whose first
screen offers mainnet.

## If it is built

**One new switch, not a choice of schemes.** Not "which dice mode" — that is a
decision a person must make at the moment they know least about it, where both answers
are fine and only the confusion is real. The switch is *let the computer do it*
(default, exactly what happens today) versus *add my own rolls* (advanced).

**Any die works, and the code does not need to know which.** Because the scheme hashes
the roll *string* rather than converting from base *n*, a d20 sequence is simply a
longer run of digits. The die affects exactly one thing: the number of rolls to aim
for, which is `bits / log₂(sides)`.

| Die | Bits/roll | → 128 bits | → 256 bits | |
|---|---|---|---|---|
| Coin (d2) | 1 | 128 | 256 | not offered |
| d4 | 2 | 64 | 128 | not offered |
| **d6** | 2.585 | **50** | **99** | default |
| d8 | 3 | 43 | 86 | |
| d10 | 3.322 | 39 | 78 | |
| d12 | 3.585 | 36 | 72 | |
| d20 | 4.322 | 30 | 60 | |

d6 is the default for unglamorous reasons: everybody owns one, and every verification
tool in the ecosystem expects base-6 digits.

**d6 is also the floor, and the table is why.** Everything below it is *more* work for
the same bits — a d4 wants 128 rolls against d6's 99, a coin 256 — so there is no
trade-off to offer, only a worse option. The usual argument for supporting a coin is
that everybody has one, and that fails here too: anybody who owns a d4 owns a d6, and
anybody with neither is not going to finish 256 flips. The small rows stay in the table
because the arithmetic is what rules them out, and somebody will otherwise ask.

**Derive these in code, never copy them from another wallet.** Feather documents
59 / 42 / 36 rolls for d6 / d12 / d20, and those numbers are correct — for Monero's
Polyseed, which encodes ~150 bits. Copied into a 24-word BIP-39 wallet they are **a
hundred bits short**, and the result looks entirely normal. `bits / log₂(sides)`
rounded up cannot make that mistake; a table of constants somebody transcribed can.

**Roll counts are guidance, not a gate.** Under mixing, stopping short costs *benefit*,
never *safety* — which is what makes it safe to show a count and let somebody stop.
Under replace it would be the difference between keeping the money and not, which is
the whole reason replace is out. Say the number, show progress against it, and do not
pretend a short roll has ruined anything.

**Tell people not to keep the rolls.** Feather's instruction is the right one and it
is only available to a wallet that mixes:

> Do not write down the outcome of your dice rolls or coin flips. This information
> can not be used to recreate your seed.

Under replace, the rolls *are* the seed, and anybody keeping them for future
verification has made a second copy of their key — usually written down less carefully
than the phrase. Mixing turns that hazard into a plain instruction.

**Do not re-roll a sequence that looks wrong.** Runs are what randomness looks like;
a person who re-rolls the streaks is deliberately removing entropy. Feather says this
and it belongs on the screen.

**State the guarantee honestly, and state its limit.** What mixing buys is: *if
Sieve's entropy path is ever broken — a bad build flag, a dependency bump, a
`getrandom` that fails in a way nobody notices — a wallet made with rolls is still
strong.* That is the Coldcard scenario, verbatim. What it does **not** buy is any way
to check that Sieve used the rolls at all. A screen that implied otherwise would be
worse than one that said nothing.

**Hard rule 2 applies at full strength.** The roll entry is key material in the widget
tree. It needs the `Secret(Zeroizing<String>)` treatment with a hand-written redacted
`Debug` from `ui/onboarding.rs`, zeroized on the way out, and it must never reach a
message type Relm4 will trace.

## The input has to be effortless, or the feature is a trap

Ninety-nine rolls is the longest uninterrupted task Sieve will ever ask of anybody.
Every fraction of a second of friction is multiplied by ninety-nine, and a person who
gives up at roll 60 out of tedium is exactly the person the roll-count guidance was
written for. **The entry screen is not a detail of this feature; it is most of it.**

**A grid of faces, and the keyboard too — the buttons are the affordance, not the
input method.** One button per face of the chosen die, clicked or tapped to record a
roll, *and* the matching digit key doing the same thing. Neither alone is right:

- **Click-only is too slow, and the reason is physical.** Typing a digit is about a
  fifth of a second and needs no visual targeting. A click is locate, travel, press —
  closer to a second even with large buttons, and it costs an eye movement to the
  screen on every single roll. Over ninety-nine rolls that is twenty seconds of typing
  against a minute and a half of mousing, and the eye movement is the worse half. It
  also breaks the one-handed anchor: a person is holding a die and a cup, and a hand
  resting on the keypad never has to look, where a pointer must be aimed.
- **Keyboard-only leaves the screen mute.** Nothing tells somebody what to press, a
  touchscreen is locked out, and a die with more than ten faces has no single-key form
  at all.

Together they cost nothing and each covers the other's gap. Lay the d6 grid out in
**numeric-keypad order** — `4 5 6` above `1 2 3` — so the shortcut teaches itself.

**Accept the numeric keypad as well as the number row.** The number row is a
two-handed reach; keypad `1`–`6` is a 3×2 block under one hand.

**Nothing else changes on screen.** No Enter, no confirmation, no per-roll animation,
no toast — only the count and the bar move. Anything that redraws more than that will
be felt ninety-nine times.

**Backspace must undo the last roll**, and the grid needs a visible Undo beside it for
whoever is clicking. They will get one wrong at roll 73. Without an undo the only
options are to start over or to accept a roll that never happened, and both are worse
than the mistake. This is the most important control on the screen after the faces
themselves.

**Ignore invalid keys silently.** `7`, `8`, `9`, `0` and every letter do nothing — no
error label, no red border, no shake. A person glancing between a die and a screen will
hit them, and an error state dismissed ninety-nine times is intolerable. The count not
advancing is feedback enough. (The buttons make this moot for clicks, which is one of
the things they are for.)

**Keep it reachable without a mouse and legible to a screen reader.** A grid of
`gtk::Button`s in a `gtk::FlowBox` — the same widget the word grid already uses — is
focusable, labelled and tab-navigable for free. The tempting shortcut of a bare
`gtk::EventControllerKey` on a `StatusPage` gives up all of that to save a few lines.

**Show the rolls as they are entered.** Somebody needs to see that the die that read 4
was recorded as 4. This is safe *only because the scheme mixes* — the rolls are not
the seed and cannot reconstruct it, which is the same property that lets the screen
tell people not to keep them. Under replace-mode the same display would be putting
the seed on screen in plaintext for the length of a long, distracted task. Another
dividend of the right scheme.

**Show progress against the target, and let them stop anyway.** `73 / 99` and a bar.
The finish button stays enabled the whole way with the honest count on it, because
under mixing a short roll costs benefit and not safety. A hard gate would be
pressure — and pressure at roll 90 is what makes somebody invent the last nine.

**Offer the die type, because the button grid makes it pay.** This reverses an earlier
draft of this document, and the button grid is why. Typed, a d20 is two keystrokes per
roll — 120 presses against d6's 99, worse despite needing fewer throws. Clicked, it is
one press per roll either way, so the table is read straight: **60 interactions against
99**, and forty fewer times picking a die up off the floor.

It is also the *right kind* of choice, which the schemes in the first half of this
document were not. "Which die is in your hand?" is a question about the physical world
that somebody answers instantly and cannot answer wrongly. "Which entropy scheme?" is a
question they would have to become a cryptographer to answer. Offering the first is not
a precedent for offering the second.

Show the roll count beside each option — *d6, 99 rolls · d10, 78 · d20, 60* — so the
trade is visible at the moment of choosing, and default to d6 because it is what is in
the drawer.

One caveat on target size: twenty buttons are each smaller than six, and a smaller
target is a slower one, so the win is real but less than 99-against-60 suggests.

**Watch the idle timer.** Auto-lock defaults to five minutes, and this is the one screen
where somebody legitimately stops for a while — to pick a die up off the floor, or
because a hundred throws is long enough to be interrupted. The capture-phase controller
in `app.rs` counts every key, click, scroll and pointer move, so ordinary rolling keeps
the wallet awake either way. But a wallet that locks at roll 90 and discards them would
be an expensive thing to find out, and the pause that triggers it is likelier here than
anywhere else in the app. Worth an explicit test rather than an assumption.

## Where this leaves it

**Build the mixed version, as one advanced switch on the wallet-creation screen.** It
is contained — `generate_mnemonic` already takes its own 32 bytes, so the change is a
function that returns those bytes differently — it cannot make any wallet worse than
Sieve already makes it, and it answers a failure that emptied five thousand wallets
last month.

**Do not build replace-mode, now or later.** Not as an expert option, not behind a
warning. Electrum shipped it and withdrew it, Sparrow declined it, and the losses it
causes are the kind nobody discovers until the money is gone.

**Do not build mouse movement.**

## Sources

- [Coldcard security advisory](https://blog.coinkite.com/coldcard-mk3-seed-generation-warning/)
- [Coldcard flaw linked to $70M theft](https://thehackernews.com/2026/08/coldcard-hardware-wallet-flaw-linked-to.html)
- [Stolen amounts reach $88.6M](https://cyberinsider.com/coldcard-warns-of-wallet-seed-flaw-as-stolen-amounts-reach-88-6-million/)
- [Sparrow #1351 — allow additional entropy sources](https://github.com/sparrowwallet/sparrow/issues/1351)
- [Electrum #523 — custom entropy for seed generation](https://github.com/spesmilo/electrum/issues/523)
- [Electrum PR #8839 — XOR-mixed `extra_entropy`](https://github.com/spesmilo/electrum/pull/8839)
- [Feather — additional seed entropy from dice rolls](https://docs.featherwallet.org/guides/entropy-from-dice)
- [Coldcard paranoid guide](https://coldcard.com/docs/paranoid/)
- [SeedSigner dice verification](https://github.com/SeedSigner/seedsigner/blob/dev/docs/dice_verification.md)
