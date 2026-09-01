# Desktop notifications

Design, not a plan. Nothing here exists, and it may not get built.

The idea is small: a wallet that keeps syncing while nobody is looking at it
knows things worth telling somebody — money arrived, a payment confirmed, an
hour-long rescan finished. GNOME can put that on screen. The mechanism is a
dozen lines.

The reason this is a document rather than a commit is that **the mechanism is
not the hard part, and the useful version and the safe version are not the same
version.** What follows is what it would take, what it would cost, and why the
verdict at the bottom is "probably not, and here is the version that would be".

## Relm4 has nothing for this, and does not need to

Checked against relm4 0.11: there is no notification abstraction, and the only
matches for the word in its source are comments about component updates. That
is not a gap. Notifications belong to GIO, one layer below GTK, and everything
needed is already in the tree through the `relm4::{gtk, adw}` re-exports.

```rust
let note = gio::Notification::new("Payment received");
note.set_priority(gio::NotificationPriority::Normal);
relm4::main_adw_application().send_notification(Some(&txid), &note);
```

`gio::Notification` offers `set_title`, `set_body`, `set_icon`, `set_category`,
`set_priority`, `set_default_action` and `add_button`; the application side is
`send_notification` and `withdraw_notification`. Actions are ordinary `GAction`s
on the application, under the `app.` prefix.

Two mechanical facts worth writing down before anyone rediscovers them:

- **The notification id is a deduplication key.** Sending twice with the same id
  replaces rather than stacks, and `withdraw_notification(id)` takes one back off
  the screen. Using the txid means a "payment confirmed" notice can be withdrawn
  if RBF replaces that transaction out from under it — which matters here,
  because Sieve has both a fee bump and a cancel-by-replacement.
- **The desktop entry is load-bearing.** GNOME routes these over
  `org.gtk.Notifications`, which resolves the application id against an
  *installed* `com.galaxoidlabs.Sieve.desktop`. A build run straight from the
  source tree has no such entry and sends nothing, silently — the same failure
  shape as the icon problem, and it wants the same treatment `ICONS` in `app.rs`
  already gives: check at startup and warn, so it is caught before somebody
  reports it as a bug.

Sending is main-thread work, which sits fine with hard rule 3: the confirmation
arrives from the node as plain data and the component sends the notification.

## The two traps

### A rescan would fire four hundred of them

This is the one that would ship broken. A rescan, or importing a wallet with
history, replays years of transactions through the same path that observes a
live confirmation. The naïve implementation notifies once per transaction found,
at three in the morning, when an import finishes. `Meta::scanned_to` makes it
worse rather than better: an interrupted scan resumes and does it again.

So the rule has to be that **a notification comes from a state transition
observed on a wallet that was already caught up** — never from a transaction
merely discovered.

Concretely, that puts the event at `AppCmd::Update { result: Ok(summary) }` in
`app.rs`, diffing the incoming `Vec<TxSummary>` against the last one held, and
suppressed entirely until the first update of the session has established a
baseline. `self.blocks_recorded` in the same handler is already exactly this
shape — a once-per-session flag guarding work that only means something after
the first update — so the pattern is in the file to copy.

`TxSummary` carries what the diff needs: `txid` to key on, `height` going from
`None` to `Some` for a confirmation, and `net_sats` for direction. No new
plumbing, which is why the temptation to raise the event at the wrong layer —
where rows are rendered — should be resisted. Rows are rendered on a rescan too.

### It fires exactly when the screen is locked

Locking deliberately leaves the node running: stopping it would mean
re-downloading filters to see a balance that was on screen a minute ago. So the
wallet is still syncing while locked, which means notifications arrive precisely
when the screen is locked and in front of whoever is standing there.

And the application name is not ours to omit. GNOME renders it in the
notification header, so the lock screen reads **Sieve — Payment received**, and
generic wording does nothing about the two disclosures that actually matter for
the threat model this wallet is built around: *this machine runs a Bitcoin
wallet*, and *money just arrived in it*. The amount was never the sensitive
part. To somebody at risk of being handed a wrench, that is strictly worse than
a number nobody can attribute.

GNOME's per-app "Lock Screen Notifications" toggle is the user's lever, and it
shows content by default. Sieve's lever is whether to send at all while
`self.unlocked` is false.

## What it would look like if it were built

### Two switches, not one

The events split cleanly by how much they disclose, and the split should be
visible in preferences rather than buried in one toggle. Both `bool`, both
`#[serde(default)]` false in `settings.rs`, both disclosed in the row that turns
them on, as `show_fiat` and `mempool_fees` already are.

- **When a scan finishes** — *"Sieve finished scanning"*. Carries no financial
  information whatsoever. It is also the one genuinely worth having: a mainnet
  rescan runs for hours and nobody watches it. Safe on a lock screen, safe in a
  screen share, safe over somebody's shoulder.
- **When money moves** — *"Payment received"*, *"Payment confirmed"*, and **no
  body at all**. A body that has been carefully drained of meaning is worse than
  no body; the title is the whole message.

Folding these together would mean somebody who wants the first has to accept the
second, and they are not comparable risks.

The disclosure wording differs in kind from the existing opt-in rows, which is
worth getting right: the price and fee rows disclose a *network request*. These
disclose a *screen*. "Sieve will not say what the payment was for or how much"
is the sentence, and it should be true rather than approximately true.

### Queue the sensitive ones past the lock

Three ways to handle the lock-screen problem, and only the third is worth
building:

1. **Send regardless.** Most useful, worst exposure, and quietly undermines the
   auto-lock feature that ships on by default at five minutes.
2. **Suppress while locked.** Safe, and guts the feature — away from the machine
   is the only time a notification is worth anything.
3. **Send the harmless one, queue the rest.** Scan-finished goes out always;
   money events accumulate and are delivered as one summary on unlock. You learn
   *"3 payments received"* when you sit down, which is when you would have
   looked anyway.

Three costs almost nothing on top of the storm suppression the first trap already
requires — it is the same queue, with the lock state deciding when it drains.

## Where this leaves it

**Not now.** The honest accounting is that the version which is clearly safe —
scan-finished, no financial content, sent whenever — is a convenience worth
about ten minutes of anyone's time, and the version people actually mean when
they ask for notifications drags in a queue, a lock-state interaction, a session
baseline, and a new class of disclosure to get right in a wallet whose entire
argument is that it does not disclose things. That is a poor ratio while M4a
cannot sign and M8 cannot publish.

**If it does get built, build the scan one first and alone.** It is the useful
half, it is the safe half, it needs neither the queue nor the lock interaction,
and it would be finished in an afternoon. Whether the money half is ever worth
adding can then be answered by somebody who has lived with the first one, rather
than guessed at here.

**What must not happen is the middle version**: money notifications with a
generic body and no queue, shipped because the wording felt careful. The wording
is not what leaks. The header is.

## Sources

- [`GNotification`](https://docs.gtk.org/gio/class.Notification.html)
- [`g_application_send_notification`](https://docs.gtk.org/gio/method.Application.send_notification.html)
- [GNOME HIG: notifications](https://developer.gnome.org/hig/patterns/feedback/notifications.html)
- [Desktop Notifications Specification](https://specifications.freedesktop.org/notification-spec/latest/)
