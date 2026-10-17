//! The deck's mana base as its list counts it: how many cards and how many
//! lands.
//!
//! Not a strategy axis (there is no commitment): the plan reads it to derive
//! the deck's mature land target from the deck's own land density
//! (`plan::curves`), so a 17-land 40-card deck and a 24-land 60-card deck are
//! each measured against themselves.

use engine::game::DeckEntry;
use engine::types::card_type::CoreType;

/// Card and land counts for one deck. `Default` (an empty or unknown deck)
/// is all zeros, which the plan reads as "use the default land target".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManaBaseFeature {
    /// Every card in the analysed deck, counting duplicates.
    pub cards: u32,
    /// Cards whose front face is a land (CR 305.1: a land card is what a land
    /// drop plays). A modal double-faced card with a spell front is not counted.
    pub lands: u32,
}

/// Count the deck's cards and lands.
pub fn detect(deck: &[DeckEntry]) -> ManaBaseFeature {
    deck.iter()
        .fold(ManaBaseFeature::default(), |mut base, entry| {
            base.cards = base.cards.saturating_add(entry.count);
            if entry.card.card_type.core_types.contains(&CoreType::Land) {
                base.lands = base.lands.saturating_add(entry.count);
            }
            base
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;

    fn entry(core_types: Vec<CoreType>, count: u32) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                card_type: CardType {
                    supertypes: Vec::new(),
                    core_types,
                    subtypes: Vec::new(),
                },
                ..Default::default()
            },
            count,
        }
    }

    #[test]
    fn counts_cards_and_lands() {
        let deck = vec![
            entry(vec![CoreType::Land], 17),
            entry(vec![CoreType::Creature], 15),
            entry(vec![CoreType::Artifact, CoreType::Land], 1),
            entry(vec![CoreType::Instant], 7),
        ];
        assert_eq!(
            detect(&deck),
            ManaBaseFeature {
                cards: 40,
                lands: 18
            }
        );
        assert_eq!(detect(&[]), ManaBaseFeature::default());
    }
}
