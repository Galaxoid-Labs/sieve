# Dice, and other entropy a person brings themselves

**Built.** This began as a design note; the reasoning is kept because the two
schemes it argues against are the ones somebody will propose again.

The verdict was build it, and the reason was five weeks old.

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

## What Sieve does without dice

`wallet::generate_mnemonic` asks for its own entropy, and that path is unchanged
— it is what every wallet made without touching the switch still uses:

```rust
let mut entropy = Zeroizing::new([0u8; 32]);
getrandom::fill(entropy.as_mut()).context("no entropy available from the OS")?;
Mnemonic::generate_with_entropy((count, Language::English), *entropy)
```

`getrandom::fill` is the `getrandom(2)` syscall on Linux — the same call the vault
uses for its salt, its nonces and its data key. There is one entropy source in the
program and this is it. BDK takes 32 bytes and uses the first `bits / 8`: sixteen for
a twelve-word phrase, all thirty-two for twenty-four.

**That function is the seam**, and dice changed how those 32 bytes are arrived at
and nothing else. `generate_mnemonic_with_rolls` is the same function with one XOR
in the middle; `generate_mnemonic` still exists and still means "the operating
system alone".

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

## How it is built

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
| **d6** | 2.585 | **50** | **100** | default |
| d8 | 3 | 43 | 86 | |
| d10 | 3.322 | 39 | 78 | |
| d12 | 3.585 | 36 | 72 | |
| d20 | 4.322 | 30 | 60 | |

d6 is the default for unglamorous reasons: everybody owns one, and every verification
tool in the ecosystem expects base-6 digits.

**d6 is also the floor, and the table is why.** Everything below it is *more* work for
the same bits — a d4 wants 128 rolls against d6's 100, a coin 256 — so there is no
trade-off to offer, only a worse option. The usual argument for supporting a coin is
that everybody has one, and that fails here too: anybody who owns a d4 owns a d6, and
anybody with neither is not going to finish 256 flips. The small rows stay in the table
because the arithmetic is what rules them out, and somebody will otherwise ask.

**The d6 count is 100 here, not the 99 everyone else quotes.** Ninety-nine rolls
carry 255.9 bits, which is not a real shortfall, and it is the number Coldcard
and SeedSigner both use. Sieve rounds the way the arithmetic does rather than
the way the convention does — `rolls_needed` is `ceil(bits / log2(sides))` — and
under mixing the difference cannot matter either way.

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

**A grid of faces, clicked. No keyboard entry at all.**

An earlier draft of this document argued for buttons as the affordance with the
number keys as a fast path, on the arithmetic that a keystroke is a fifth of a
second where a click is closer to a whole one. That was right about the speed
and wrong about the feature, because **a keyboard cannot express a die**.

One keystroke is one digit. A d12 or a d20 has faces that take two, so typing
`17` records a 1 and then a 7 — two rolls, neither of them the one that was
thrown. Every workaround makes it worse: reading `0` as ten covers a d10 and
nothing above it, and a two-key sequence needs a timeout, which is a race
condition in the middle of a hundred-repetition task. Keyboard entry worked only
for d6, d8 and d10, which made it a trap on exactly the dice that need the
fewest throws.

So the buttons are the input. The keyboard is not shut out — every face is a
`gtk::Button` in a `gtk::FlowBox`, so Tab reaches it and Space or Return presses
it, and a screen reader reads it — but there is no digit shortcut, and nothing on
screen implies one.

**Whole rows, sized to the die.** The row width is the largest divisor of the
face count that is at most five: 3×2 for a d6, 5×4 for a d20. A ragged last row
reads as a rendering fault rather than a layout.

**Not homogeneous.** A `FlowBox` set homogeneous stretches every child to an
equal share of the clamp, which turned a d6 into three buttons the width of a
finger. Sized to their content and centred instead.

**Nothing else changes on screen.** No Enter, no confirmation, no per-roll
animation, no toast — only the count and the bar move. Anything that redraws more
than that will be felt a hundred times.

**Undo beside the grid.** They will press one wrong at roll 73. Without an undo
the only options are to start over or to accept a roll that never happened, and
both are worse than the mistake. It is the most important control on the screen
after the faces themselves.

**Show the rolls as they are entered.** Somebody needs to see that the die that
read 4 was recorded as 4. This is safe *only because the scheme mixes* — the rolls
are not the seed and cannot reconstruct it, which is the same property that lets
the screen tell people not to keep them. Under replace-mode the same display
would be putting a seed on screen in plaintext for the length of a long,
distracted task. Another dividend of the right scheme.

**The target is a gate, not a suggestion.** `73 of 100 rolls`, a bar, and a
finish button that stays shut until the count is met — its label counting down
(`27 more rolls`) until it flips to `Use these 100 rolls`.

An earlier draft argued the opposite: that under mixing a short session costs
benefit rather than safety, so the button should stay pressable and a hard gate
would only be pressure. The first half of that is still true and the conclusion
does not follow. Somebody who asked for a hundred rolls of their own entropy and
stopped at thirty has most of what they came for missing, and **nothing
afterwards will ever tell them** — the wallet is not weak, so no screen has cause
to say anything. The count is the only moment it can be said, and it is said by
not opening the door. The handler re-checks it too: a threshold enforced only by
the sensitivity of the widget that crosses it is enforced by the view.

**Say what the target buys, not just what it is.** The picker's row reads
`50 rolls — at least the 128 bits a 12-word phrase carries`, recomputed from both
the die and the phrase length. A bare number is a demand without a reason, and
the reason is the whole point: the target is not a house rule, it is how many
throws of *this* die carry the bits *this* phrase holds. Somebody who can see
that can also see why a d20 halves the work.

**Offer the die type, because the button grid makes it pay.** Typed, a d20 would
have been two keystrokes per roll — 120 presses against d6's 100, worse despite
needing fewer throws. Clicked, it is one press per roll either way, so the table
reads straight: **60 interactions against 100**, and forty fewer times picking a
die up off the floor.

It is also the *right kind* of choice, which the schemes in the first half of this
document were not. "Which die is in your hand?" is a question about the physical world
that somebody answers instantly and cannot answer wrongly. "Which entropy scheme?" is a
question they would have to become a cryptographer to answer. Offering the first is not
a precedent for offering the second.

Show the roll count beside each option — *d6, 100 rolls · d10, 78 · d20, 60* — so the
trade is visible at the moment of choosing, and default to d6 because it is what is in
the drawer.

One caveat on target size: twenty buttons are each smaller than six, and a smaller
target is a slower one, so the win is real but less than 100-against-60 suggests.

**Watch the idle timer.** Auto-lock defaults to five minutes, and this is the one screen
where somebody legitimately stops for a while — to pick a die up off the floor, or
because a hundred throws is long enough to be interrupted. The capture-phase controller
in `app.rs` counts every key, click, scroll and pointer move, so ordinary rolling keeps
the wallet awake either way. But a wallet that locks at roll 90 and discards them would
be an expensive thing to find out, and the pause that triggers it is likelier here than
anywhere else in the app. Worth an explicit test rather than an assumption.

## Where this leaves it

**Built as the mixed version**, one advanced switch on the wallet-creation screen:
`wallet::generate_mnemonic_with_rolls` does `os_bytes XOR SHA256(rolls)`, and a
test asserts that the *same rolls twice give different phrases*. That test is the
whole design in one assertion — if it ever passes as equal, Sieve has silently
become replace-mode, and nothing else on the screen would show it.

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
