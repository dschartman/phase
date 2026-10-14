//! "Put ... counters on each of them" after a token-creating clause places the
//! counters on the tokens that clause created.
//!
//! CR 608.2c: an instruction is read with the rules of English, so the plural
//! anaphor "each of them" names the objects the previous instruction produced.
//! CR 111.1: those objects are the tokens the effect just put onto the
//! battlefield. CR 122.1a / CR 122.1b: +1/+1 counters and keyword counters (here
//! reach) are then put on each of those tokens. Without them Snarl Song's 0/0
//! Fractals have 0 toughness and die to the state-based action in CR 704.5f.
//!
//! Every row drives the real pipeline (`GameRunner::cast(..).resolve()` or
//! `GameRunner::activate(..).resolve()`) and asserts resolved board state,
//! except T6, which pins coverage honesty on the parse.
//!
//! Verbatim Oracle text (byte-verbatim from MTGJSON `AtomicCards.json`): Snarl
//! Song (T1, T2), Assemble the Entmoot (T3), Recursive Recruitment (T6).
//!
//! Synthetic text, disclosed: T4 ("Create ... token. Tap up to two target
//! creatures. Put a stun counter on each of them.") and T5a/T5b ("You may pay
//! {2}. If you do, create two ... tokens named Cadet. Put a +1/+1 counter on each
//! of them.") have no printed witness; T5 is templated on the "you may pay {N}.
//! If you do, create ..." pattern used in `reflexive_body_token_referent.rs`.
//! Staging fixtures are also synthetic: T2 stages a token from an earlier
//! resolution with "Create a 1/1 white Soldier creature token.", and T3 stages
//! life gained this turn with "You gain 3 life.". The card under test in each of
//! those rows is verbatim.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SNARL_SONG: &str = "Converge \u{2014} Create two 0/0 green and blue Fractal creature tokens. Put X +1/+1 counters on each of them and you gain X life, where X is the number of colors of mana spent to cast this spell.";
const ASSEMBLE_THE_ENTMOOT: &str = "Creatures you control have reach.\nSacrifice this enchantment: Create three tapped X/X green Treefolk creature tokens, where X is the amount of life you gained this turn. Put a reach counter on each of them.";
const RECURSIVE_RECRUITMENT: &str = "Create two 2/2 colorless Wizard Soldier creature tokens named Cadet. If this spell was cast from a graveyard, put a +1/+1 counter on each of them for every three cards in your graveyard.\nFlashback {6}{U}{B} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

/// Synthetic staging spell: a token from an earlier resolution.
const STAGE_SOLDIER: &str = "Create a 1/1 white Soldier creature token.";
/// Synthetic staging spell: life gained this turn.
const STAGE_GAIN_THREE: &str = "You gain 3 life.";
/// Synthetic hostile: chosen targets sit between the creator and the anaphor.
const CHOSEN_AFTER_CREATOR: &str = "Create a 1/1 white Soldier creature token. Tap up to two target creatures. Put a stun counter on each of them.";
/// Synthetic hostile: the creator is under an affirmative optional-cost gate.
const GATED_CREATOR: &str = "You may pay {2}. If you do, create two 1/1 white Soldier creature tokens named Cadet. Put a +1/+1 counter on each of them.";

fn mana(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|kind| ManaUnit::new(*kind, ObjectId(9_999), false, vec![]))
        .collect()
}

/// Battlefield tokens with the given name, in battlefield order.
fn tokens_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| obj.is_token && obj.name == name)
        })
        .collect()
}

fn counter(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

fn total_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].counters.values().sum()
}

/// Cast the synthetic Soldier staging spell and return the Soldier token.
fn stage_prior_soldier(runner: &mut GameRunner, spell: ObjectId) -> ObjectId {
    runner.cast(spell).resolve();
    let soldiers = tokens_named(runner, "Soldier");
    assert_eq!(
        soldiers.len(),
        1,
        "the staging spell must create one Soldier"
    );
    soldiers[0]
}

fn snarl_song_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 5,
    }
}

/// {G}{U} plus four colorless: two colors spent on Snarl Song's {5}{G}.
fn two_color_pool() -> Vec<ManaUnit> {
    mana(&[
        ManaType::Green,
        ManaType::Blue,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
        ManaType::Colorless,
    ])
}

fn assert_two_fractals_with_two_counters(runner: &GameRunner) {
    let fractals = tokens_named(runner, "Fractal");
    assert_eq!(
        fractals.len(),
        2,
        "both 0/0 Fractals must survive with their counters (CR 704.5f)"
    );
    for fractal in fractals {
        assert_eq!(
            counter(runner, fractal, &CounterType::Plus1Plus1),
            2,
            "each Fractal gets X = 2 +1/+1 counters"
        );
        let obj = &runner.state().objects[&fractal];
        assert_eq!((obj.power, obj.toughness), (Some(2), Some(2)));
    }
}

/// T1. CR 608.2c + CR 122.1a: with two colors spent, Snarl Song makes two 2/2
/// Fractals and gains 2 life.
#[test]
fn snarl_song_two_colors_spent_makes_two_2_2_fractals_and_gains_two_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, two_color_pool());
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Snarl Song", false, SNARL_SONG)
        .with_mana_cost(snarl_song_cost())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    // Positive reach: resolution got to the GainLife sibling.
    outcome.assert_life_delta(P0, 2);
    assert_two_fractals_with_two_counters(&runner);
}

/// T2. Multi-authority: a token created by an earlier resolution is not one of
/// "them"; only this resolution's Fractals get counters.
#[test]
fn snarl_song_counters_skip_a_token_from_an_earlier_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, two_color_pool());
    let stage = scenario
        .add_spell_to_hand_from_oracle(P0, "Stage Soldier", false, STAGE_SOLDIER)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Snarl Song", false, SNARL_SONG)
        .with_mana_cost(snarl_song_cost())
        .id();
    let mut runner = scenario.build();
    let soldier = stage_prior_soldier(&mut runner, stage);

    runner.cast(spell).resolve();

    assert_two_fractals_with_two_counters(&runner);
    assert_eq!(
        counter(&runner, soldier, &CounterType::Plus1Plus1),
        0,
        "the earlier Soldier is not one of \"them\""
    );
}

/// T3. CR 122.1b + CR 613.1f: each Treefolk gets a reach counter and has reach.
/// Assemble the Entmoot is in the graveyard afterward, so reach cannot come
/// from its "Creatures you control have reach." static.
#[test]
fn assemble_the_entmoot_treefolk_each_get_a_reach_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stage = scenario
        .add_spell_to_hand_from_oracle(P0, "Stage Gain", false, STAGE_GAIN_THREE)
        .id();
    let entmoot = scenario
        .add_enchantment_from_oracle(P0, "Assemble the Entmoot", ASSEMBLE_THE_ENTMOOT)
        .id();
    let mut runner = scenario.build();
    runner.cast(stage).resolve();
    let index = runner.state().objects[&entmoot]
        .abilities
        .iter()
        .position(|ability: &AbilityDefinition| {
            matches!(ability.kind, AbilityKind::Activated)
                && matches!(ability.cost, Some(AbilityCost::Sacrifice(_)))
        })
        .expect("Assemble the Entmoot exposes its sacrifice ability");

    let outcome = runner.activate(entmoot, index).resolve();

    outcome.assert_zone(&[entmoot], Zone::Graveyard);
    let treefolk = tokens_named(&runner, "Treefolk");
    assert_eq!(treefolk.len(), 3, "three Treefolk tokens are created");
    for tree in treefolk {
        outcome.assert_tapped(tree, true);
        // X read 3 from life gained this turn: the ability resolved fully.
        outcome.assert_power_toughness(tree, 3, 3);
        assert_eq!(
            counter(&runner, tree, &CounterType::Keyword(KeywordKind::Reach)),
            1,
            "each Treefolk gets one reach counter"
        );
        assert!(
            runner.state().objects[&tree].has_keyword_kind(KeywordKind::Reach),
            "the reach counter grants reach (CR 122.1b)"
        );
    }
}

/// T4 (guard, synthetic). Chosen targets between the creator and the anaphor
/// are the antecedent of "each of them" (CR 115.1 + CR 608.2c): the stun
/// counters go on the chosen creatures, not on the Soldier token.
#[test]
fn chosen_targets_after_token_creation_keep_parent_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chosen After Creator", false, CHOSEN_AFTER_CREATOR)
        .id();
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_objects(&[bear_a, bear_b])
        .resolve();

    for bear in [bear_a, bear_b] {
        outcome.assert_tapped(bear, true);
        assert_eq!(counter(&runner, bear, &CounterType::Stun), 1);
    }
    let soldiers = tokens_named(&runner, "Soldier");
    assert_eq!(soldiers.len(), 1, "the Soldier token exists");
    assert_eq!(total_counters(&runner, soldiers[0]), 0);
}

fn gated_scenario() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless, ManaType::Colorless]));
    let stage = scenario
        .add_spell_to_hand_from_oracle(P0, "Stage Soldier", false, STAGE_SOLDIER)
        .id();
    let gated = scenario
        .add_spell_to_hand_from_oracle(P0, "Gated Creator", false, GATED_CREATOR)
        .id();
    let mut runner = scenario.build();
    let soldier = stage_prior_soldier(&mut runner, stage);
    (runner, gated, soldier)
}

/// T5a (synthetic). An accepted gate creates the Cadets and the counters land on
/// them, not on the Soldier left in the created-token ledger by an earlier
/// resolution.
#[test]
fn gated_created_tokens_get_counters_when_paid() {
    let (mut runner, gated, soldier) = gated_scenario();

    runner.cast(gated).accept_optional().resolve();

    let cadets = tokens_named(&runner, "Cadet");
    assert_eq!(cadets.len(), 2, "the paid gate creates two Cadets");
    for cadet in cadets {
        assert_eq!(counter(&runner, cadet, &CounterType::Plus1Plus1), 1);
    }
    assert_eq!(total_counters(&runner, soldier), 0);
}

/// T5b (guard, synthetic). CR 608.2c + CR 608.2d: a declined gate creates nothing, so nothing
/// receives counters; the stale ledger entry (the Soldier) is not read.
#[test]
fn gated_created_tokens_decline_touches_no_stale_token() {
    let (mut runner, gated, soldier) = gated_scenario();

    let outcome = runner.cast(gated).decline_optional().resolve();

    outcome.assert_zone(&[gated], Zone::Graveyard);
    assert!(tokens_named(&runner, "Cadet").is_empty(), "no Cadet exists");
    assert_eq!(
        outcome.mana_pool_total(P0),
        2,
        "the declined {{2}} stays in the pool"
    );
    assert!(
        runner.state().objects.contains_key(&soldier),
        "the stale ledger entry is present"
    );
    assert_eq!(total_counters(&runner, soldier), 0);
}

fn walk(def: &AbilityDefinition, visit: &mut impl FnMut(&AbilityDefinition)) {
    visit(def);
    if let Some(sub) = def.sub_ability.as_deref() {
        walk(sub, visit);
    }
    if let Some(other) = def.else_ability.as_deref() {
        walk(other, visit);
    }
}

/// T6 (guard). Coverage honesty: Recursive Recruitment's "for every three cards
/// in your graveyard" multiplier is out of scope, so its counter rider stays an
/// `Unimplemented` from the counter remainder guard and the card stays red.
#[test]
fn recursive_recruitment_rider_stays_unimplemented() {
    let parsed = parse_oracle_text(
        RECURSIVE_RECRUITMENT,
        "Recursive Recruitment",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let mut cadet_token = false;
    let mut rider_gap = false;
    for ability in &parsed.abilities {
        walk(ability, &mut |def| match &*def.effect {
            Effect::Token { name, .. } if name == "Cadet" => cadet_token = true,
            Effect::Unimplemented { name, .. } if name == "put_counter_tail" => {
                rider_gap = def
                    .effect
                    .unimplemented_description()
                    .is_some_and(|text| text.contains("for every three cards"));
            }
            _ => {}
        });
    }
    assert!(
        cadet_token,
        "the Cadet token creation parses (positive reach)"
    );
    assert!(
        rider_gap,
        "the counter rider stays Unimplemented(put_counter_tail) on the multiplier"
    );
}
