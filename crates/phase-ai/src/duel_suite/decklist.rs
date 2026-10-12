//! Plain-text decklist files for `ai-duel --p0-deck PATH --p1-deck PATH`.
//!
//! The registered matchups (`spec::MATCHUPS`) are the pinned measurement set the
//! AI gate runs; a decklist file is the way to put any other two decks — a
//! sealed or draft pool, a brew — into a duel without registering them there.
//! Deck size is whatever the list says: a 40-card limited deck is as valid an
//! input as a 60-card constructed one.
//!
//! Accepted line shapes (case-insensitive headers, one card entry per line):
//!
//! ```text
//! // comment            # comment
//! Deck / Main / Mainboard / Sideboard     (section headers)
//! 4 Lightning Bolt
//! 4x Lightning Bolt
//! 1 Fblthp, Impossibly Lost (FRA) 57      (Arena export: set/collector suffix ignored)
//! ```
//!
//! Every name must resolve against the card database. An unresolved name is an
//! error that lists every such name, never a silent drop: a deck that quietly
//! loses a card is a different deck than the one asked for.

use std::fmt;
use std::path::{Path, PathBuf};

use engine::database::CardDatabase;
use engine::game::deck_loading::PlayerDeckList;

/// A parsed decklist, expanded to one entry per copy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decklist {
    pub main: Vec<String>,
    pub sideboard: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Main,
    Sideboard,
}

#[derive(Debug)]
pub enum DecklistError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A line that is not a comment, header, or `<count> <name>` entry.
    Line { line: usize, text: String },
    /// The list has no main-deck cards.
    Empty,
    /// Names the card database does not know, in list order, deduplicated.
    Unresolved(Vec<String>),
}

impl fmt::Display for DecklistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecklistError::Io { path, source } => {
                write!(f, "cannot read decklist {}: {source}", path.display())
            }
            DecklistError::Line { line, text } => write!(
                f,
                "line {line}: expected '<count> <card name>', a section header, or a comment, got '{text}'"
            ),
            DecklistError::Empty => write!(f, "decklist has no main-deck cards"),
            DecklistError::Unresolved(names) => write!(
                f,
                "card names not found in card-data.json: {}",
                names.join("; ")
            ),
        }
    }
}

impl std::error::Error for DecklistError {}

/// Parse decklist text. See the module docs for the accepted shapes.
pub fn parse_decklist(text: &str) -> Result<Decklist, DecklistError> {
    let mut deck = Decklist::default();
    let mut section = Section::Main;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        if let Some(header) = section_header(line) {
            section = header;
            continue;
        }
        let Some((count, name)) = parse_entry(line) else {
            return Err(DecklistError::Line {
                line: index + 1,
                text: line.to_string(),
            });
        };
        let bucket = match section {
            Section::Main => &mut deck.main,
            Section::Sideboard => &mut deck.sideboard,
        };
        bucket.extend(std::iter::repeat_n(name.to_string(), count));
    }
    if deck.main.is_empty() {
        return Err(DecklistError::Empty);
    }
    Ok(deck)
}

/// Read and parse a decklist file.
pub fn load_decklist(path: &Path) -> Result<Decklist, DecklistError> {
    let text = std::fs::read_to_string(path).map_err(|source| DecklistError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_decklist(&text)
}

/// Check every name against `db` and build the engine's name-only deck list.
/// Fails with every unresolved name rather than dropping them, because
/// `resolve_deck_list` skips unknown names silently.
pub fn to_player_deck_list(
    db: &CardDatabase,
    deck: &Decklist,
) -> Result<PlayerDeckList, DecklistError> {
    let mut unresolved: Vec<String> = Vec::new();
    for name in deck.main.iter().chain(&deck.sideboard) {
        if db.get_face_by_name(name).is_none() && !unresolved.contains(name) {
            unresolved.push(name.clone());
        }
    }
    if !unresolved.is_empty() {
        return Err(DecklistError::Unresolved(unresolved));
    }
    Ok(PlayerDeckList {
        main_deck: deck.main.clone(),
        sideboard: deck.sideboard.clone(),
        ..Default::default()
    })
}

fn section_header(line: &str) -> Option<Section> {
    let header = line.trim_end_matches(':');
    ["deck", "main", "mainboard", "maindeck"]
        .iter()
        .any(|h| header.eq_ignore_ascii_case(h))
        .then_some(Section::Main)
        .or_else(|| {
            header
                .eq_ignore_ascii_case("sideboard")
                .then_some(Section::Sideboard)
        })
}

/// `<count>[x] <name>[ (SET) <collector>]` → `(count, name)`.
fn parse_entry(line: &str) -> Option<(usize, &str)> {
    let (count, rest) = line.split_once(char::is_whitespace)?;
    let count = count.strip_suffix(['x', 'X']).unwrap_or(count);
    let count: usize = count.parse().ok().filter(|&n| n > 0)?;
    let name = strip_set_suffix(rest.trim());
    (!name.is_empty()).then_some((count, name))
}

/// Drop an Arena-style ` (SET) 123` / ` (SET)` suffix. Card names never end in a
/// parenthesized group, so the last ` (` that is followed by a closing `)` marks
/// the suffix.
fn strip_set_suffix(name: &str) -> &str {
    match name.rsplit_once(" (") {
        Some((head, tail)) if tail.contains(')') => head.trim_end(),
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_expand_comments_and_headers_are_skipped() {
        let deck = parse_decklist(
            "// FRA sealed pool\n# note\n\nDeck\n2 Last Gasp\n1x Fblthp, Impossibly Lost\n\
             3X Island\nSideboard\n1 Forest\n",
        )
        .expect("parses");
        assert_eq!(
            deck.main,
            vec![
                "Last Gasp",
                "Last Gasp",
                "Fblthp, Impossibly Lost",
                "Island",
                "Island",
                "Island"
            ]
        );
        assert_eq!(deck.sideboard, vec!["Forest"]);
    }

    #[test]
    fn a_forty_card_list_stays_forty_cards() {
        let deck = parse_decklist("23 Spell Card\n17 Island\n").expect("parses");
        assert_eq!(deck.main.len(), 40);
    }

    #[test]
    fn arena_set_suffix_is_dropped_but_names_with_commas_survive() {
        let deck =
            parse_decklist("1 Fblthp, Impossibly Lost (FRA) 57\n1 Opt (XLN)\n").expect("parses");
        assert_eq!(deck.main, vec!["Fblthp, Impossibly Lost", "Opt"]);
    }

    #[test]
    fn a_line_without_a_count_is_rejected_with_its_line_number() {
        let err = parse_decklist("2 Island\nLightning Bolt\n").expect_err("rejects");
        assert!(
            matches!(err, DecklistError::Line { line: 2, ref text } if text == "Lightning Bolt"),
            "{err}"
        );
        assert!(
            parse_decklist("0 Island\n").is_err(),
            "a zero count is not an entry"
        );
    }

    #[test]
    fn a_list_with_no_main_deck_is_rejected() {
        assert!(matches!(
            parse_decklist("// nothing\nSideboard\n1 Forest\n"),
            Err(DecklistError::Empty)
        ));
    }

    #[test]
    fn unresolved_names_are_all_reported_not_dropped() {
        let mut cards = serde_json::Map::new();
        for name in ["Island", "Opt"] {
            cards.insert(
                name.to_lowercase(),
                serde_json::json!({
                    "name": name,
                    "mana_cost": { "type": "NoCost" },
                    "card_type": { "supertypes": [], "core_types": ["Instant"], "subtypes": [] },
                    "power": null, "toughness": null, "loyalty": null, "defense": null,
                    "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                    "keywords": [], "abilities": [], "triggers": [], "static_abilities": [],
                    "replacements": [], "color_override": null, "scryfall_oracle_id": null
                }),
            );
        }
        let db = CardDatabase::from_json_str(&serde_json::Value::Object(cards).to_string())
            .expect("fixture db");

        let ok = parse_decklist("2 Island\n1 Opt\n").unwrap();
        let list = to_player_deck_list(&db, &ok).expect("resolves");
        assert_eq!(list.main_deck.len(), 3);

        let bad = parse_decklist("1 Opt\n2 Not A Card\n1 Also Missing\n").unwrap();
        match to_player_deck_list(&db, &bad) {
            Err(DecklistError::Unresolved(names)) => {
                assert_eq!(names, vec!["Not A Card", "Also Missing"]);
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }
}
