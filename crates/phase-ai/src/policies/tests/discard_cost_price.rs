//! End-to-end pins for pricing a discard cost by the card it gives up.
//!
//! Report (FRA sealed signals, 2026-10-17): with Murmuring Volume on the
//! battlefield and one castable card in hand, the AI activated the loot and
//! discarded that card (Break Under Pressure, Divining Duelist, Extended
//! Absence, Archive Arbiter in the seed 1008 audit). The same shape recurs with
//! Twinned Vision's flashback. `SelfCostValue` priced any one-card discard at a
//! flat 1.0 against a draw of 1.0, so the loot was always "covered", whatever
//! card it threw away.
//!
//! Both cards are real; their Oracle text is the MTGJSON text, parsed through
//! the shipped parser:
//!
//! ```text
//! Murmuring Volume  {3}  Artifact — Book
//!   {T}: Add one mana of any color.
//!   {2}, {T}, Discard a card: Draw a card.
//! Twinned Vision    {1}{U/R}  Instant
//!   Draw a card. If this spell wasn't cast from your hand, draw two cards
//!   instead.
//!   Flashback—{1}{U/R}{U/R}, Discard a card.
//! ```
//!
//! Each arm has a positive control: the same board with a card in hand worth
//! less than the expected draw, where the loot or the flashback must still be
//! available. A blanket "never loot" would pass the negative arm alone.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

use crate::config::{create_config, AiDifficulty, Platform};
use crate::score_candidates;

const VOLUME_TEXT: &str = "{T}: Add one mana of any color.\n{2}, {T}, Discard a card: Draw a card.";
const TWINNED_VISION_TEXT: &str = "Draw a card. If this spell wasn't cast from your hand, draw \
    two cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from \
    your graveyard for its flashback cost. Then exile it.)";

/// What the AI holds: a card worth more than the expected draw, or less.
#[derive(Clone, Copy)]
enum Held {
    /// A 4/4 for {3}{U}: `intrinsic_value` 1.5 * 4 + 4 + 0.5 * 4 = 12.0.
    GoodCreature,
    /// A {1}{U} instant: `intrinsic_value` 0.5 * 2 = 1.0.
    CheapInstant,
    /// Last Gasp ({1}{B}, "Target creature gets -3/-3 until end of turn.")
    /// with an opposing 3/3 it kills: removal, worth more than a draw.
    Removal,
    /// The Theorist, Jace Beleren ({2}{U}{U}, loyalty 3): a planeswalker,
    /// worth more than two draws.
    Planeswalker,
}

const LAST_GASP_TEXT: &str = "Target creature gets -3/-3 until end of turn.";
const THEORIST_TEXT: &str = "At the beginning of each opponent's draw step, you draw a card.\n\
    [+1]: Create a 1/1 blue Illusion creature token.\n\
    [−2]: For each opponent, return up to one target artifact or creature that player controls \
    to its owner's hand.\n\
    [−6]: Draw three cards. Then put X +1/+1 counters on each creature you control, where X is \
    the number of cards in your hand.";

struct Board {
    state: GameState,
    source: ObjectId,
    held: ObjectId,
}

/// Main phase, AI to act, five untapped Islands, a library of six Islands
/// (expected draw: `intrinsic_value` 3.0), and one card in hand; the opponent
/// has ten Plains in library and nothing else.
fn board(held: Held, with_volume: bool, with_twinned_vision: bool) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..6 {
        scenario.add_land_to_library_top(P0, "Island");
    }
    // The opponent has a library too, so no line wins by decking them and the
    // candidate scores stay in the ordinary range.
    for _ in 0..10 {
        scenario.add_land_to_library_top(P1, "Plains");
    }
    let held = match held {
        Held::GoodCreature => scenario
            .add_creature_to_hand(P0, "Good Creature", 4, 4)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 3,
            })
            .id(),
        Held::CheapInstant => scenario
            .add_spell_to_hand_from_oracle(P0, "Cheap Instant", true, "You gain 1 life.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            })
            .id(),
        Held::Removal => {
            scenario
                .add_creature(P1, "Opposing Bear", 3, 3)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 2,
                });
            scenario
                .add_spell_to_hand_from_oracle(P0, "Last Gasp", true, LAST_GASP_TEXT)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Black],
                    generic: 1,
                })
                .id()
        }
        Held::Planeswalker => scenario
            .add_spell_to_hand(P0, "The Theorist, Jace Beleren", false)
            .from_oracle_text(THEORIST_TEXT)
            .as_planeswalker_with_loyalty("Jace", 3)
            .as_legendary()
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 2,
            })
            .id(),
    };
    let volume = with_volume.then(|| {
        scenario
            .add_artifact_from_oracle(P0, "Murmuring Volume", VOLUME_TEXT)
            .id()
    });
    let vision = with_twinned_vision.then(|| {
        scenario
            .add_spell_to_graveyard(P0, "Twinned Vision", true)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::BlueRed],
                generic: 1,
            })
            .from_oracle_text_with_keywords(&["Flashback"], TWINNED_VISION_TEXT)
            .id()
    });
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    Board {
        state: runner.state().clone(),
        source: volume.or(vision).expect("one source"),
        held,
    }
}

fn is_loot(action: &GameAction, source: ObjectId) -> bool {
    matches!(action, GameAction::ActivateAbility { source_id, ability_index: 1 } if *source_id == source)
}

fn is_flashback(action: &GameAction, source: ObjectId) -> bool {
    matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == source)
}

/// The candidate's production score (`None` = dropped by a `Reject`) and the
/// engine's count of how often it was offered. The offered count is the
/// non-vacuity probe: "not chosen" means nothing if it was never legal.
fn measure(board: &Board, matches: fn(&GameAction, ObjectId) -> bool) -> (usize, Option<f64>) {
    let offered = engine::ai_support::legal_actions(&board.state)
        .iter()
        .filter(|a| matches(a, board.source))
        .count();
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let score = score_candidates(&board.state, P0, &config)
        .into_iter()
        .find(|(a, _)| matches(a, board.source))
        .map(|(_, s)| s);
    (offered, score)
}

fn assert_rejected(score: Option<f64>, what: &str) {
    if let Some(score) = score {
        assert!(
            score.is_infinite() && score.is_sign_negative(),
            "{what} must be REJECTED, got {score}"
        );
    }
}

fn assert_available(score: Option<f64>, what: &str) {
    let score = score.unwrap_or_else(|| panic!("{what} must still be scored"));
    assert!(score.is_finite(), "{what} must not be vetoed, got {score}");
}

/// CR 118.1 + CR 701.9b: the loot's cost is the card it gives up. With one
/// card in hand worth 12.0 against an expected draw of 3.0, the loot trades
/// four expected cards for one and must not be chosen.
#[test]
fn volume_loot_does_not_throw_away_a_lone_card_worth_more_than_a_draw() {
    let board = board(Held::GoodCreature, true, false);
    let (offered, score) = measure(&board, is_loot);
    assert!(offered > 0, "fixture must offer the Volume loot");
    assert_rejected(score, "looting away the only card, worth four draws,");
    assert!(board.state.objects.contains_key(&board.held));
}

/// Positive control: the same board holding a card worth less than a draw.
#[test]
fn volume_loot_stays_available_when_the_lone_card_is_worth_less_than_a_draw() {
    let board = board(Held::CheapInstant, true, false);
    let (offered, score) = measure(&board, is_loot);
    assert!(offered > 0, "fixture must offer the Volume loot");
    assert_available(score, "looting away a card worth a third of a draw");
}

/// CR 702.34a + CR 601.2f: Twinned Vision's flashback cost includes the
/// discard. Cast from the graveyard it draws two, so the card given up is
/// weighed against two expected draws (6.0); a lone card worth 12.0 is a loss.
#[test]
fn twinned_vision_flashback_does_not_throw_away_a_lone_card_worth_more_than_two_draws() {
    let board = board(Held::GoodCreature, false, true);
    let (offered, score) = measure(&board, is_flashback);
    assert!(offered > 0, "fixture must offer the flashback cast");
    assert_rejected(score, "flashing back by discarding a card worth four draws");
}

/// Positive control for the flashback arm.
#[test]
fn twinned_vision_flashback_stays_available_when_the_lone_card_is_cheap() {
    let board = board(Held::CheapInstant, false, true);
    let (offered, score) = measure(&board, is_flashback);
    assert!(offered > 0, "fixture must offer the flashback cast");
    assert_available(
        score,
        "flashing back by discarding a card worth a third of a draw",
    );
}

/// The target shape of the noncreature-value round (`fork-checks/9f72018f4/
/// OPEN.md`): a lone removal spell, priced 1.0 at 0.5 a pip, was looted away
/// against an expected draw of 3.0. Removal with a creature to answer is worth
/// more than one draw, so the loot must not be chosen.
#[test]
fn volume_loot_does_not_throw_away_a_lone_removal_spell() {
    let board = board(Held::Removal, true, false);
    let (offered, score) = measure(&board, is_loot);
    assert!(offered > 0, "fixture must offer the Volume loot");
    assert_rejected(
        score,
        "looting away the only card, a removal spell with a target,",
    );
}

/// Twinned Vision's flashback draws two; a lone planeswalker (The Theorist,
/// priced 2.0 at 0.5 a pip) is worth more than two expected draws.
#[test]
fn twinned_vision_flashback_does_not_throw_away_a_lone_planeswalker() {
    let board = board(Held::Planeswalker, false, true);
    let (offered, score) = measure(&board, is_flashback);
    assert!(offered > 0, "fixture must offer the flashback cast");
    assert_rejected(score, "flashing back by discarding The Theorist");
}
