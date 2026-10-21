//! The keep order for a card with flashback: a discard loses the hand cast
//! alone, so the card keeps its hand price.
//!
//! The rule (`fork-checks/96d0bfd2a/JUDGE.md`, blocker B1): kept in hand, a
//! flashback card gives two casts, from hand and then from the graveyard (CR
//! 702.34a). Discarded, it still gives the graveyard cast. The graveyard cast
//! is in both futures and cancels, so what a discard loses is the hand cast,
//! which `intrinsic_value` already prices (it reads the spell as cast from
//! hand). The keep value of a flashback card is its `intrinsic_value`.
//!
//! These rows are permanent guards on that behavior. JF1, JF2, JF3 and JF5
//! fail against the withdrawn rule (hand value less the graveyard cast; tag
//! `archive/flashback-keep-wrong-rule-2026-10-20`, 96d0bfd2a), which dropped
//! Twinned Vision to 1.5 and Recursive Recruitment to 4.0. The last two
//! guards, from that pair's red commit (a library of two, no blue or red
//! source, one land), pin the same hand price on the boards where a reading
//! of the graveyard cast's reach would differ.
//!
//! The cards are real; their Oracle text is the MTGJSON text
//! (`client/public/card-data.json`), parsed through the shipped parser:
//!
//! ```text
//! Twinned Vision        {1}{U/R}   Instant  Draw a card. If this spell wasn't cast
//!                                           from your hand, draw two cards instead.
//!                                           Flashback—{1}{U/R}{U/R}, Discard a card.
//! Liliana the Repentant {1}{B}     Legendary Creature — Human Warlock 2/2
//!                                           Whenever another creature or planeswalker
//!                                           you control enters, mill two cards.
//!                                           Exhaust — {5}{B}: Return target creature or
//!                                           planeswalker card from your graveyard to the
//!                                           battlefield. Put a +1/+1 counter on Liliana.
//!                                           Activate only as a sorcery.
//! Recursive Recruitment {2}{U}{B}  Sorcery  Create two 2/2 colorless Wizard Soldier
//!                                           creature tokens named Cadet. If this spell
//!                                           was cast from a graveyard, put a +1/+1
//!                                           counter on each of them for every three
//!                                           cards in your graveyard.
//!                                           Flashback {6}{U}{B}
//! ```
//!
//! The draw-two with a cheaper flashback and the one-pip pump are made-up
//! cards, the draw-two's "Flashback {cost}" set as a keyword.
//!
//! Every row reads the keep order the discard seams share: the CR 514.1
//! cleanup discard and the discard after a loot (`keep_key` / `cmp_keep`) and
//! the discard cost (`given_up_cards`, `keep_prices`).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{FlashbackCost, Keyword};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::zones::Zone;

use crate::card_value::{given_up_cards, intrinsic_value, keep_key};

const TWINNED_VISION_TEXT: &str = "Draw a card. If this spell wasn't cast from your hand, draw \
    two cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from \
    your graveyard for its flashback cost. Then exile it.)";
const LILIANA_TEXT: &str = "Whenever another creature or planeswalker you control enters, mill \
    two cards.\nExhaust — {5}{B}: Return target creature or planeswalker card from your graveyard \
    to the battlefield. Put a +1/+1 counter on Liliana. Activate only as a sorcery. (Activate \
    each exhaust ability only once.)";
const DRAW_TWO_TEXT: &str = "Draw two cards.";
const ONE_PIP_PUMP_TEXT: &str = "Target creature gets +1/+1 until end of turn.";
const RECRUITMENT_TEXT: &str = "Create two 2/2 colorless Wizard Soldier creature tokens named \
    Cadet. If this spell was cast from a graveyard, put a +1/+1 counter on each of them for \
    every three cards in your graveyard.";

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// P0's board for a discard: `lands` basics of `colour` on the battlefield and
/// a library of `library` vanilla 4/4s for {4} (`intrinsic_value` 12.0 each:
/// 1.5 * 4 + 4 + 0.5 * 4), so the expected draw is 12.0.
struct Board {
    scenario: GameScenario,
    library: usize,
}

impl Board {
    fn new(colour: ManaColor, lands: usize, library: usize) -> Self {
        let mut scenario = GameScenario::new();
        for _ in 0..lands {
            scenario.add_basic_land(P0, colour);
        }
        for _ in 0..10 {
            scenario.add_land_to_library_top(P1, "Plains");
        }
        Self { scenario, library }
    }

    fn twinned_vision(&mut self) -> ObjectId {
        self.scenario
            .add_spell_to_hand_from_oracle(P0, "Twinned Vision", true, "")
            .with_mana_cost(cost(&[ManaCostShard::BlueRed], 1))
            .from_oracle_text_with_keywords(&["Flashback"], TWINNED_VISION_TEXT)
            .id()
    }

    fn liliana(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_hand_from_oracle(P0, "Liliana the Repentant", 2, 2, LILIANA_TEXT)
            .with_mana_cost(cost(&[ManaCostShard::Black], 1))
            .id()
    }

    /// A made-up instant or sorcery with "Flashback {`flashback`}" (CR
    /// 702.34a), the keyword set as the card data's keyword list sets it.
    fn spell(
        &mut self,
        name: &str,
        instant: bool,
        text: &str,
        mana: ManaCost,
        flashback: ManaCost,
    ) -> ObjectId {
        self.scenario
            .add_spell_to_hand_from_oracle(P0, name, instant, text)
            .with_mana_cost(mana)
            .with_keyword(Keyword::Flashback(FlashbackCost::Mana(flashback)))
            .id()
    }

    fn plain_spell(&mut self, name: &str, text: &str, mana: ManaCost) -> ObjectId {
        self.scenario
            .add_spell_to_hand_from_oracle(P0, name, true, text)
            .with_mana_cost(mana)
            .id()
    }

    /// A vanilla 2/2 for {1}{B} in hand (6.0).
    fn bear(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_hand(P0, "Bear", 2, 2)
            .with_mana_cost(cost(&[ManaCostShard::Black], 1))
            .id()
    }

    fn state(self) -> GameState {
        let mut state = self.scenario.build().state().clone();
        for _ in 0..self.library {
            let card_id = CardId(state.next_object_id);
            let id = create_object(
                &mut state,
                card_id,
                P0,
                "Library Beast".to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.mana_cost = cost(&[], 4);
        }
        assert_eq!(state.players[0].library.len(), self.library);
        state
    }
}

fn first_given_up(state: &GameState, pool: &[ObjectId]) -> ObjectId {
    given_up_cards(state, pool, 1, None)[0]
}

fn keep_value(state: &GameState, id: ObjectId) -> f64 {
    keep_key(state, id, None).1
}

/// JF1 (the 1029 T21 and 1018 T14 board): Twinned Vision and Liliana the
/// Repentant in hand, three Islands, a library mean of 12.0. The discard
/// loses Twinned Vision's hand cast, one draw plus its pips, 13.0; the
/// graveyard cast survives either way. Liliana (6.0) is given up.
#[test]
fn jf1_twinned_vision_keeps_its_hand_cast_value() {
    let mut board = Board::new(ManaColor::Blue, 3, 5);
    let vision = board.twinned_vision();
    let liliana = board.liliana();
    let state = board.state();
    assert!(
        state.objects[&vision]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Flashback(FlashbackCost::NonMana(_)))),
        "the fixture parses the flashback cost"
    );
    assert_eq!(intrinsic_value(&state, vision), 13.0);
    assert_eq!(keep_value(&state, vision), intrinsic_value(&state, vision));
    assert_eq!(first_given_up(&state, &[liliana, vision]), liliana);
}

/// JF2 (1020 T8): four lands in play, four lands and Twinned Vision in hand.
/// A spare land is given up, not the only spell.
#[test]
fn jf2_a_spare_land_goes_before_twinned_vision() {
    let mut board = Board::new(ManaColor::Blue, 4, 5);
    let vision = board.twinned_vision();
    let lands: Vec<ObjectId> = (0..4)
        .map(|i| {
            board
                .scenario
                .add_land_to_hand(P0, &format!("Spare {i}"))
                .id()
        })
        .collect();
    let state = board.state();
    let mut pool = lands.clone();
    pool.push(vision);
    let first = first_given_up(&state, &pool);
    assert!(
        lands.contains(&first),
        "a land goes first; Twinned Vision keeps {} against a land's {}",
        keep_value(&state, vision),
        keep_value(&state, lands[0])
    );
}

/// JF3 (1023 T16): nine lands, Recursive Recruitment ({2}{U}{B}, Flashback
/// {6}{U}{B}) and a 2/2 for two. Pitching Recruitment loses its hand cast (two
/// 2/2s now); the graveyard cast happens either way (it did, at T20, in both
/// runs). The 2/2 is given up.
#[test]
fn jf3_recursive_recruitment_keeps_its_hand_cast() {
    let mut board = Board::new(ManaColor::Blue, 5, 5);
    for _ in 0..4 {
        board.scenario.add_basic_land(P0, ManaColor::Black);
    }
    let recruitment = board.spell(
        "Recursive Recruitment",
        false,
        RECRUITMENT_TEXT,
        cost(&[ManaCostShard::Blue, ManaCostShard::Black], 2),
        cost(&[ManaCostShard::Blue, ManaCostShard::Black], 6),
    );
    let bear = board.bear();
    let state = board.state();
    assert_eq!(
        keep_value(&state, recruitment),
        intrinsic_value(&state, recruitment),
        "hand price {}",
        intrinsic_value(&state, recruitment)
    );
    assert_eq!(first_given_up(&state, &[recruitment, bear]), bear);
}

/// JF5: a draw-two whose flashback is cheaper than its cast. Pitched, it
/// loses its hand cast (two draws); a one-pip pump (0.5) goes first.
#[test]
fn jf5_a_cheap_flashback_draw_two_is_not_free_to_pitch() {
    let mut board = Board::new(ManaColor::Blue, 3, 5);
    let draw_two = board.spell(
        "Cheap Recall",
        false,
        DRAW_TWO_TEXT,
        cost(&[ManaCostShard::Blue], 2),
        cost(&[ManaCostShard::Blue], 1),
    );
    let pump = board.plain_spell(
        "Small Pump",
        ONE_PIP_PUMP_TEXT,
        cost(&[ManaCostShard::White], 0),
    );
    let state = board.state();
    assert_eq!(
        keep_value(&state, draw_two),
        intrinsic_value(&state, draw_two)
    );
    assert_eq!(first_given_up(&state, &[pump, draw_two]), pump);
}

/// Guard (CR 121.4 + CR 704.5b): with two cards left the flashback's two draws
/// would empty the library, a cast the draw-to-zero gate rejects. The hand
/// cast is still what a discard loses, so Twinned Vision keeps its hand price
/// and is kept over Liliana.
#[test]
fn a_flashback_that_draws_the_library_out_keeps_the_hand_price() {
    let mut board = Board::new(ManaColor::Blue, 3, 2);
    let vision = board.twinned_vision();
    let liliana = board.liliana();
    let state = board.state();
    assert_eq!(keep_value(&state, vision), intrinsic_value(&state, vision));
    assert_eq!(first_given_up(&state, &[liliana, vision]), liliana);
}

/// Guard: no source of blue or red in play or in hand, or too few lands for
/// its three mana: the flashback is out of reach. The reach of the graveyard
/// cast does not move the keep value; the hand price stands.
#[test]
fn a_flashback_out_of_reach_keeps_the_hand_price() {
    for (colour, lands) in [(ManaColor::Black, 3), (ManaColor::Blue, 1)] {
        let mut board = Board::new(colour, lands, 5);
        let vision = board.twinned_vision();
        let liliana = board.liliana();
        let state = board.state();
        assert_eq!(
            keep_value(&state, vision),
            intrinsic_value(&state, vision),
            "{lands} {colour:?} lands: out of reach"
        );
        assert_eq!(first_given_up(&state, &[liliana, vision]), liliana);
    }
}
