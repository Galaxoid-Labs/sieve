//! Typing a recovery phrase, one numbered box per word.
//!
//! A phrase pasted into a single line is the easiest thing to build and the
//! worst thing to check. Twelve or twenty-four words go in from paper, one at a
//! time, and the two mistakes people actually make — a word in the wrong place
//! and a word that is not a BIP-39 word at all — are both invisible in a
//! sentence of running text. A numbered box per word makes the position part of
//! what you are reading, and lets a wrong word be marked where it is rather
//! than reported as "that phrase is not valid" after the fact.
//!
//! Pasting still works, and works into any box: the whole phrase spills across
//! the boxes from wherever it lands. See `Spill` below.

use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::gtk;
use relm4::gtk::prelude::*;
use zeroize::Zeroizing;

use bdk_wallet::keys::bip39::Language;

/// One word on its way into a phrase.
///
/// A newtype with a redacted `Debug` for the reason `Face` has one: relm4
/// traces every message under `RUST_LOG=relm4=trace`, and these messages carry
/// one word of a seed each. A derived `Debug` would write a recovery phrase
/// into the log a word at a time, which is the same disclosure as writing it
/// out in one line, only harder to notice.
#[derive(Clone, Default)]
pub struct Word(pub Zeroizing<String>);

impl std::fmt::Debug for Word {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Word {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Whether a word is one of the 2,048, said as three states rather than two.
///
/// Empty is not wrong — most of the boxes are empty most of the time, and
/// marking them all in red while somebody is halfway through typing is noise
/// that teaches them to ignore the colour by the time it means something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Empty,
    Good,
    Bad,
}

/// Is this one of the BIP-39 English words?
pub fn known(word: &str) -> bool {
    Language::English.find_word(word).is_some()
}

/// The whole word, when a prefix can only mean one.
///
/// BIP-39's list is built so that four characters identify a word, which is
/// what makes completing one safe rather than a guess: if exactly one word
/// starts with what has been typed, there is nothing else it could become.
/// Below three characters this stays out of the way — "ac" has one completion
/// in some lists and typing two letters is not yet an intention.
pub fn completion(prefix: &str) -> Option<&'static str> {
    if prefix.len() < 3 || known(prefix) {
        return None;
    }
    match Language::English.words_by_prefix(prefix) {
        [only] => Some(only),
        _ => None,
    }
}

/// An Electrum seed: a phrase that is valid, and not valid *here*.
///
/// Electrum does not use BIP-39. It uses the same 2,048 English words — which
/// is exactly what makes this worth detecting — but validates by a version
/// number rather than a checksum, and derives at `m/0'` rather than at a BIP
/// purpose. So an Electrum seed is twelve real words that fail Sieve's
/// checksum, which is indistinguishable from a mistyped BIP-39 phrase unless
/// somebody looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Electrum {
    /// Electrum's `01`: P2PKH and multisig P2SH.
    Standard,
    /// Electrum's `100`: P2WPKH and P2WSH.
    Segwit,
    /// Electrum's `101` and `102`: two-factor wallets, which are 2-of-3 with a
    /// service holding a key. Nothing recovers one of these from words alone.
    TwoFactor,
}

impl Electrum {
    /// What to tell somebody holding it.
    ///
    /// Names the wallet it came from, because "this is not a valid recovery
    /// phrase" sends a person to check a piece of paper that is perfectly
    /// correct.
    pub fn explain(self) -> &'static str {
        match self {
            Electrum::Standard | Electrum::Segwit => {
                "This is an Electrum seed phrase. Electrum uses the same words as \
                 BIP-39 but a different format, and Sieve cannot import one yet. \
                 Your phrase is fine — it is this wallet that does not read it."
            }
            Electrum::TwoFactor => {
                "This is an Electrum two-factor seed phrase. Those wallets need \
                 Electrum and its co-signing service, so the words alone do not \
                 recover one anywhere."
            }
        }
    }
}

/// Is this a seed some other wallet would accept?
///
/// Electrum's rule, and it is a version number rather than a checksum:
/// `HMAC-SHA512("Seed version", phrase)` in hex must *start with* the version.
///
/// Normalisation here is the English case only — lowercase, single spaces.
/// Electrum's own `normalize_text` also strips combining characters and closes
/// gaps between CJK, which matters for the wordlists Sieve does not offer. A
/// phrase that needs more than this will simply not be recognised, which is
/// the safe direction: the message goes back to the ordinary one.
pub fn electrum_seed(phrase: &str) -> Option<Electrum> {
    use bdk_wallet::bitcoin::hashes::{Hash, HashEngine, Hmac, HmacEngine, sha512};

    let normalized = phrase
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if normalized.is_empty() {
        return None;
    }

    let mut engine = HmacEngine::<sha512::Hash>::new(b"Seed version");
    engine.input(normalized.as_bytes());
    let digest = Hmac::<sha512::Hash>::from_engine(engine);
    let hex = format!("{digest:x}");

    // Longest first would matter if any were a prefix of another; none are, and
    // the order is Electrum's own.
    if hex.starts_with("01") {
        Some(Electrum::Standard)
    } else if hex.starts_with("100") {
        Some(Electrum::Segwit)
    } else if hex.starts_with("101") || hex.starts_with("102") {
        Some(Electrum::TwoFactor)
    } else {
        None
    }
}

/// One numbered box.
pub struct PhraseWord {
    /// 1-based, and shown. The number is half of what makes a phrase
    /// checkable against paper.
    position: usize,
    word: Word,
    state: State,
}

impl std::fmt::Debug for PhraseWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhraseWord")
            .field("position", &self.position)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum PhraseWordMsg {
    /// The entry changed under somebody's fingers.
    Typed(Word),
    /// The parent is writing a word in, from a spill or a reset.
    Set(Word),
    /// Put the caret here.
    Focus,
}

#[derive(Debug)]
pub enum PhraseWordOutput {
    /// This box settled on a word. The parent recomputes the phrase.
    Changed,
    /// More than one word arrived at once — a paste, or a typed space. The
    /// first is kept here and the rest belong to the boxes after this one.
    ///
    /// Carried as `Vec<Word>` rather than a joined string so that no message
    /// in the program holds a whole phrase in one field.
    Spill(usize, Vec<Word>),
}

#[relm4::factory(pub)]
impl FactoryComponent for PhraseWord {
    type Init = (usize, Word);
    type Input = PhraseWordMsg;
    type Output = PhraseWordOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::FlowBox;

    view! {
        gtk::Box {
            add_css_class: "seed-word",
            set_spacing: 6,
            set_valign: gtk::Align::Center,

            gtk::Label {
                add_css_class: "seed-index",
                add_css_class: "numeric",
                set_width_chars: 2,
                set_xalign: 1.0,
                set_label: &self.position.to_string(),
            },

            #[name(entry)]
            gtk::Entry {
                add_css_class: "flat",
                add_css_class: "monospace",
                set_has_frame: false,
                set_width_chars: 9,
                set_max_width_chars: 9,
                set_hexpand: true,
                // A recovery phrase is not a password: it is being copied from
                // paper and has to be read back. Hiding it would make checking
                // it impossible, which is the whole point of this screen.
                set_visibility: true,
                set_input_purpose: gtk::InputPurpose::FreeForm,
                // Off, all of it. A word of a seed must never reach a spell
                // checker, an input-method history or a completion store.
                set_input_hints: gtk::InputHints::NO_SPELLCHECK
                    | gtk::InputHints::PRIVATE
                    | gtk::InputHints::NO_EMOJI,
                connect_changed[sender] => move |entry| {
                    sender.input(PhraseWordMsg::Typed(Word(Zeroizing::new(
                        entry.text().to_string(),
                    ))));
                },
            },
        }
    }

    fn init_model(
        (position, word): Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        let state = state_of(&word);
        PhraseWord {
            position,
            word,
            state,
        }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: FactorySender<Self>,
    ) {
        match msg {
            PhraseWordMsg::Focus => {
                widgets.entry.grab_focus();
                // grab_focus selects the whole text, which would make the next
                // keystroke replace a word the spill just delivered.
                widgets.entry.set_position(-1);
                return;
            }

            PhraseWordMsg::Set(word) => {
                if word.as_str() == self.word.as_str() {
                    return;
                }
                self.word = word;
            }

            PhraseWordMsg::Typed(typed) => {
                // One word, unchanged: the echo of our own `set_text` below, or
                // a keystroke that produced nothing new. Either way there is
                // nothing to tell the parent, and answering would loop.
                if typed.as_str() == self.word.as_str() {
                    return;
                }

                // Whitespace means a boundary — a pasted phrase, or a space
                // typed at the end of a word. Everything after the first word
                // belongs to the boxes after this one.
                if typed.as_str().split_whitespace().count() > 1
                    || typed.as_str().ends_with(char::is_whitespace)
                {
                    let mut parts = typed
                        .as_str()
                        .split_whitespace()
                        .map(|w| Word(Zeroizing::new(w.to_lowercase())));
                    let first = parts.next().unwrap_or_default();
                    let rest: Vec<Word> = parts.collect();

                    // A space after a complete-enough prefix finishes the word,
                    // which is what makes typing twenty-four of these bearable.
                    self.word = match completion(first.as_str()) {
                        Some(full) => Word(Zeroizing::new(full.to_string())),
                        None => first,
                    };
                    self.state = state_of(&self.word);
                    self.apply(widgets);
                    let _ = sender.output(PhraseWordOutput::Spill(self.position, rest));
                    return;
                }

                self.word = Word(Zeroizing::new(typed.as_str().to_lowercase()));
            }
        }

        self.state = state_of(&self.word);
        self.apply(widgets);
        let _ = sender.output(PhraseWordOutput::Changed);
    }
}

impl PhraseWord {
    pub fn word(&self) -> &Word {
        &self.word
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Push the model onto the entry, and only when they disagree.
    ///
    /// Setting the text unconditionally would move the caret to the end on
    /// every keystroke, so editing the middle of a word would be impossible —
    /// which is why this is a comparison and not a `#[watch]`.
    fn apply(&self, widgets: &PhraseWordWidgets) {
        if widgets.entry.text().as_str() != self.word.as_str() {
            widgets.entry.set_text(self.word.as_str());
            widgets.entry.set_position(-1);
        }
        // Marked where it is, rather than reported afterwards. Only a finished
        // word that is not on the list earns this: red under a half-typed word
        // is red under every word for most of the time somebody is typing.
        if self.state == State::Bad {
            widgets.entry.add_css_class("error");
        } else {
            widgets.entry.remove_css_class("error");
        }
    }
}

fn state_of(word: &Word) -> State {
    if word.is_empty() {
        State::Empty
    } else if known(word.as_str()) {
        State::Good
    } else {
        State::Bad
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_completes_only_when_it_can_mean_one_word() {
        // Four characters identify a BIP-39 word; three often do.
        assert_eq!(completion("aban"), Some("abandon"));
        // "ab" is too little to be an intention, and this stays out of the way.
        assert_eq!(completion("ab"), None);
        // A word already on the list is finished, not a prefix of something.
        assert_eq!(completion("act"), None);
        assert!(known("act"));
        // Several words start with "car", so there is nothing to complete to.
        assert_eq!(completion("car"), None);
        // Not a prefix of anything.
        assert_eq!(completion("zzz"), None);
    }

    /// The completion must never invent a word that is not on the list.
    ///
    /// Everything this offers is submitted as part of a seed, and a word that
    /// is not BIP-39 derives nothing — so a completion that could be wrong is
    /// worse than no completion at all.
    #[test]
    fn every_completion_is_a_real_word() {
        for word in Language::English.word_list() {
            if let Some(done) = completion(&word[..3.min(word.len())]) {
                assert!(known(done), "completed to something unknown: {done}");
            }
        }
    }

    /// A real Electrum seed is recognised as one.
    ///
    /// Generated for this test by searching for a phrase whose
    /// `HMAC-SHA512("Seed version", …)` starts with `100`, which is what
    /// Electrum itself does when it makes a segwit seed. It holds nothing and
    /// never has.
    #[test]
    fn an_electrum_seed_is_named_rather_than_called_a_typo() {
        let electrum = "rapid phone kid day save forward gasp cereal nasty fat absorb load";

        // The trap in one assertion: every word is a real BIP-39 word, and the
        // phrase is not a BIP-39 phrase. Without the check below, this is
        // indistinguishable from somebody mistyping.
        assert!(electrum.split_whitespace().all(known));
        assert!(
            bdk_wallet::keys::bip39::Mnemonic::parse_in(
                bdk_wallet::keys::bip39::Language::English,
                electrum
            )
            .is_err(),
            "an Electrum seed must not pass BIP-39 validation"
        );

        assert_eq!(electrum_seed(electrum), Some(Electrum::Segwit));
        assert!(
            electrum_seed(electrum)
                .unwrap()
                .explain()
                .contains("Electrum")
        );

        // Case and spacing are normalised the way Electrum normalises them, so
        // a phrase pasted out of a document still matches.
        assert_eq!(
            electrum_seed("  RAPID  phone kid day save forward gasp cereal nasty fat absorb LOAD "),
            Some(Electrum::Segwit)
        );
    }

    /// An ordinary BIP-39 phrase must never be called an Electrum one.
    ///
    /// This is the direction that would do damage: telling somebody with a
    /// valid BIP-39 phrase that Sieve cannot import it would turn a working
    /// import into a refusal.
    #[test]
    fn a_valid_bip39_phrase_is_not_mistaken_for_another_wallets() {
        let bip39 = "abandon abandon abandon abandon abandon abandon \
                     abandon abandon abandon abandon abandon about";
        let bip39 = bip39.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            bdk_wallet::keys::bip39::Mnemonic::parse_in(
                bdk_wallet::keys::bip39::Language::English,
                &bip39
            )
            .is_ok(),
            "the fixture must be a valid BIP-39 phrase"
        );
        assert_eq!(electrum_seed(&bip39), None);

        // Nothing, and rubbish, are not other wallets' seeds either.
        assert_eq!(electrum_seed(""), None);
        assert_eq!(electrum_seed("   "), None);
        assert_eq!(electrum_seed("not a seed at all"), None);
    }

    #[test]
    fn a_word_is_empty_good_or_bad() {
        let empty = Word(Zeroizing::new(String::new()));
        let good = Word(Zeroizing::new("abandon".into()));
        let bad = Word(Zeroizing::new("abandonn".into()));
        assert_eq!(state_of(&empty), State::Empty);
        assert_eq!(state_of(&good), State::Good);
        assert_eq!(state_of(&bad), State::Bad);
    }

    /// The redaction is the whole reason this type exists.
    #[test]
    fn a_word_does_not_print_itself() {
        let word = Word(Zeroizing::new("abandon".into()));
        assert_eq!(format!("{word:?}"), "<redacted>");
        assert!(!format!("{:?}", PhraseWordMsg::Set(word)).contains("abandon"));
    }
}
