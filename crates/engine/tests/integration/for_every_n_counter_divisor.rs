//! "… for every N <things>" — a per-group count: one unit for each complete
//! group of N (Recursive Recruitment: "put a +1/+1 counter on each of them for
//! every three cards in your graveyard").
//!
//! CR 107.1 + CR 107.1a: the game uses only integers, so a count "for every
//! three" cards is the number of complete groups of three, rounded down. The
//! parser composes that from the existing `QuantityExpr::DivideRounded`
//! building block over the same quantity a plain "for each <thing>" yields.
//!
//! The runtime tests cast the real card through the pipeline so the counter
//! count is observed on the created tokens, from both the hand (the rider is
//! gated off) and the graveyard via flashback (CR 702.34a).

use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr, RoundingMode, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const RECURSIVE_RECRUITMENT: &str = "Create two 2/2 colorless Wizard Soldier creature \
    tokens named Cadet. If this spell was cast from a graveyard, put a +1/+1 counter on \
    each of them for every three cards in your graveyard.\nFlashback {6}{U}{B} (You may \
    cast this card from your graveyard for its flashback cost. Then exile it.)";

/// The +1/+1 counter count on each Cadet token on the battlefield.
fn cadet_counters(state: &GameState) -> Vec<u32> {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[*id].name == "Cadet")
        .map(|id| {
            state.objects[id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .collect()
}

fn mana(colors: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    colors
        .iter()
        .flat_map(|&(color, n)| {
            (0..n).map(move |_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        })
        .collect()
}

fn recruitment_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
        generic: 2,
    }
}

/// SHAPE: the rider's count is the graveyard count divided by three, rounded
/// down, it lands on the created tokens, and it is no longer `Unimplemented`.
#[test]
fn for_every_three_cards_parses_to_divide_rounded_down() {
    let parsed = parse_oracle_text(
        RECURSIVE_RECRUITMENT,
        "Recursive Recruitment",
        &["Flashback".to_string()],
        &["Sorcery".to_string()],
        &[],
    );
    let spell = parsed.abilities.first().expect("spell ability");
    let rider = spell.sub_ability.as_ref().expect("counter rider");
    let (Effect::PutCounter { count, target, .. } | Effect::PutCounterAll { count, target, .. }) =
        rider.effect.as_ref()
    else {
        panic!("rider must place counters, got {:?}", rider.effect);
    };
    // CR 608.2c: "each of them" after "Create two … tokens" is the created tokens.
    assert_eq!(*target, TargetFilter::LastCreated);
    let QuantityExpr::DivideRounded {
        divisor, rounding, ..
    } = count
    else {
        panic!("count must divide by the group size, got {count:?}");
    };
    assert_eq!(*divisor, 3);
    assert_eq!(*rounding, RoundingMode::Down);
}

/// CR 702.34a + CR 107.1a: cast via flashback with eight other cards in the
/// graveyard, each Cadet gets floor(8 / 3) = 2 counters. The spell itself is on
/// the stack while it resolves, so it is not one of the counted cards.
#[test]
fn flashback_puts_one_counter_per_three_graveyard_cards_on_each_cadet() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..8 {
        scenario.add_creature_to_graveyard(P0, &format!("Dead {i}"), 1, 1);
    }
    let spell = scenario
        .add_spell_to_graveyard(P0, "Recursive Recruitment", false)
        .with_mana_cost(recruitment_cost())
        .from_oracle_text_with_keywords(&["Flashback"], RECURSIVE_RECRUITMENT)
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Blue, 7), (ManaType::Black, 1)]));
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .resolve();
    assert_eq!(
        cadet_counters(outcome.state()),
        vec![2, 2],
        "two Cadets, floor(8 / 3) = 2 counters on each"
    );
}

/// Reach guard for the gate: cast from hand, the "cast from a graveyard" rider
/// does not apply, so the Cadets have no counters even with a full graveyard.
#[test]
fn cast_from_hand_puts_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..9 {
        scenario.add_creature_to_graveyard(P0, &format!("Dead {i}"), 1, 1);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Recursive Recruitment", false, RECURSIVE_RECRUITMENT)
        .with_mana_cost(recruitment_cost())
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Blue, 3), (ManaType::Black, 1)]));
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        cadet_counters(outcome.state()),
        vec![0, 0],
        "two Cadets; the rider only applies when cast from a graveyard"
    );
}

/// CR 608.2c + CR 111.1: "Create two … tokens. Put two +1/+1 counters on each of
/// them." — "each of them" names the tokens the previous instruction created.
/// With 0/0 tokens the counters are what keep them alive (CR 704.5f), so this
/// fails loudly if the anaphor binds to nothing.
#[test]
fn each_of_them_after_create_tokens_places_counters_on_the_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Fractal Sorcery",
            false,
            "Create two 0/0 green and blue Fractal creature tokens named Cadet. \
             Put two +1/+1 counters on each of them.",
        )
        .with_mana_cost(recruitment_cost())
        .id();
    scenario.with_mana_pool(P0, mana(&[(ManaType::Blue, 3), (ManaType::Black, 1)]));
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        cadet_counters(outcome.state()),
        vec![2, 2],
        "both 0/0 tokens survive with two counters each"
    );
}
