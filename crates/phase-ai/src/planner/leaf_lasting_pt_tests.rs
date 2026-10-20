//! The leaf's board sum reads each creature's power and toughness as they
//! will be after this turn's cleanup, with power floored at 0
//! (`simforge/docs/spec-ai-leaf-eot-pt-2026-10-20.md`).
//!
//! CR 514.2: "all 'until end of turn' and 'this turn' effects end" in the
//! cleanup step, so a creature under an until-end-of-turn -3/-3 that survived
//! is, from the next turn on, the creature it was. The leaf priced it as the
//! shrunken body for good, so a Last Gasp on a 3/4 (it survives at 0/1)
//! scored above killing a 1/1 (judge row L4 on 60ae8209b: 8.212 against
//! 6.122 at the target prompt, about 6.05 of the gap the -3/-3 read as
//! lasting and 2.03 a 1/5's negative power credited). These rows pin the leaf
//! values the search's scores are made of; combat that the search resolves
//! itself is untouched (guards).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

use super::PlannerServices;
use crate::config::{create_config, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::eval::{evaluate_features, evaluate_state, EvalWeightSet};

const GASP: &str = "Target creature gets -3/-3 until end of turn.";
const PUMP: &str = "Target creature gets +3/+3 until end of turn.";

fn libraries(scenario: &mut GameScenario) {
    for _ in 0..20 {
        scenario.add_land_to_library_top(P0, "Island");
    }
    for _ in 0..25 {
        scenario.add_land_to_library_top(P1, "Plains");
    }
}

fn instant_in_hand(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    shard: ManaCostShard,
    generic: u32,
) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, name, true)
        .from_oracle_text(oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![shard],
            generic,
        })
        .id()
}

/// The search's two leaf readings of `state` for the AI (P0): the strategy
/// leaf (`evaluate_with_strategy`) and the tactical leaf at the learned
/// weights of the state's turn (`evaluate_state`).
fn leaf_values(state: &GameState) -> (f64, f64) {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let policies = crate::policies::PolicyRegistry::shared();
    let context = AiContext::empty(&config.weights);
    let services = PlannerServices::new(P0, &config, policies, context);
    let strategy = services.evaluate_with_strategy(state);
    let tactical = evaluate_state(
        state,
        P0,
        EvalWeightSet::learned().for_turn(state.turn_number),
    );
    (strategy, tactical)
}

/// The L4 table: P0's precombat main, two Swamps and Last Gasp in hand, no
/// creatures; P1 a 3/4 and a 1/1, nothing open. Gasp is cast at `target` and
/// resolved through the real pipeline (CR 601.2, CR 608.2).
fn gasp_resolved_at(pick_big: bool) -> (GameState, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    let gasp = instant_in_hand(&mut scenario, "Gasp", GASP, ManaCostShard::Black, 1);
    let big = scenario.add_creature(P1, "Big Arbiter", 3, 4).id();
    let small = scenario.add_creature(P1, "Small Illusion", 1, 1).id();
    let mut runner = scenario.build();
    let target = if pick_big { big } else { small };
    runner.cast(gasp).target_objects(&[target]).resolve();
    let state = runner.state().clone();
    (state, big, small)
}

/// Done-when 1 (judge row L4 on 60ae8209b, with the target prune out of the
/// way: the two resolved positions scored directly by the leaf). Gasp on the
/// 3/4 leaves it alive at 0/1 until cleanup (CR 514.2), then a 3/4 again;
/// Gasp on the 1/1 kills it (CR 704.5f). The kill line SHALL score above the
/// survivor line at both leaves.
#[test]
fn l4_the_kill_line_outscores_the_survivor_line_at_the_leaf() {
    let (survivor, big, _) = gasp_resolved_at(true);
    let (killed, _, small) = gasp_resolved_at(false);
    assert_eq!(
        survivor.objects[&big].toughness,
        Some(1),
        "reach-guard: the 3/4 survived at 0/1"
    );
    assert!(
        !survivor.battlefield.contains(&big) || survivor.objects[&big].power == Some(0),
        "reach-guard: the 3/4 is 0/1 on the battlefield"
    );
    assert!(
        !killed.battlefield.contains(&small),
        "reach-guard: the 1/1 died"
    );
    let (survivor_strategy, survivor_tactical) = leaf_values(&survivor);
    let (kill_strategy, kill_tactical) = leaf_values(&killed);
    eprintln!("L4 leaf: survivor {survivor_strategy:.3}/{survivor_tactical:.3} kill {kill_strategy:.3}/{kill_tactical:.3}");
    assert!(
        kill_strategy > survivor_strategy,
        "strategy leaf: kill {kill_strategy:.3} <= survivor {survivor_strategy:.3}"
    );
    assert!(
        kill_tactical > survivor_tactical,
        "tactical leaf: kill {kill_tactical:.3} <= survivor {survivor_tactical:.3}"
    );
}

/// The opposing side of L4 in features: the 3/4 under -3/-3 until end of
/// turn is read as the 3/4 it is after cleanup (CR 514.2), not a 0/1.
#[test]
fn an_opposing_shrink_until_end_of_turn_is_read_after_cleanup() {
    let (survivor, _, _) = gasp_resolved_at(true);
    let features = evaluate_features(&survivor, P0).expect("non-terminal");
    // P1: the 3/4 and the 1/1, as they are after cleanup.
    assert_eq!(features.board_power, -4.0, "power after cleanup");
    assert_eq!(features.board_toughness, -5.0, "toughness after cleanup");
    assert_eq!(features.board_presence, -2.0, "both bodies are still there");
}

/// P0's postcombat main (no combat ahead this turn): its 2/2 Bear, a Forest
/// and an instant +3/+3 until end of turn, cast at the Bear and resolved.
fn pump_resolved_postcombat() -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let pump = instant_in_hand(&mut scenario, "Pump", PUMP, ManaCostShard::Green, 0);
    let mut runner = scenario.build();
    runner.cast(pump).target_objects(&[bear]).resolve();
    (runner.state().clone(), bear)
}

/// Done-when 2, first clause: a +3/+3 until end of turn on the AI's own
/// creature outside combat is worth the creature after cleanup (CR 514.2):
/// the Bear reads 2/2, not 5/5.
#[test]
fn an_own_pump_outside_combat_is_read_after_cleanup() {
    let (state, bear) = pump_resolved_postcombat();
    assert_eq!(
        state.objects[&bear].power,
        Some(5),
        "reach-guard: the pump resolved (5/5 now)"
    );
    let features = evaluate_features(&state, P0).expect("non-terminal");
    assert_eq!(features.board_power, 2.0, "the Bear's power after cleanup");
    assert_eq!(
        features.board_toughness, 2.0,
        "the Bear's toughness after cleanup"
    );
}

/// P0 attacks with its 2/2 Bear; P1 blocks with a 3/3. P0 holds an instant
/// +3/+3 until end of turn and a Forest. Returns the runner at P0's priority
/// in the declare-blockers step.
fn bear_blocked_by_a_three_three() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let wall = scenario.add_creature(P1, "Ogre", 3, 3).id();
    let pump = instant_in_hand(&mut scenario, "Pump", PUMP, ManaCostShard::Green, 0);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("declare the Bear");
    let mut guard = 0;
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::PassPriority)
            .expect("pass to blockers");
        guard += 1;
        assert!(guard < 8, "reach-guard: declare blockers");
    }
    runner
        .declare_blockers(&[(wall, bear)])
        .expect("Ogre blocks");
    let mut guard = 0;
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ) {
        runner
            .act(GameAction::PassPriority)
            .expect("to P0's priority");
        guard += 1;
        assert!(guard < 8, "reach-guard: P0's priority after blocks");
    }
    assert_eq!(
        runner.state().phase,
        Phase::DeclareBlockers,
        "reach-guard: P0 holds priority before damage"
    );
    (runner, bear, wall, pump)
}

/// Pass priority until combat damage has been dealt (CR 510.2) and the
/// game is past the combat damage step.
fn through_combat_damage(runner: &mut GameRunner) {
    let mut guard = 0;
    while matches!(
        runner.state().phase,
        Phase::DeclareBlockers | Phase::CombatDamage
    ) || !runner.state().stack.is_empty()
    {
        runner.act(GameAction::PassPriority).expect("pass");
        guard += 1;
        assert!(guard < 12, "reach-guard: past combat damage");
    }
}

/// Done-when 2, second clause (guard): the same pump during combat before
/// damage. Combat the engine resolves still reflects it (CR 510.1a: the 5/5
/// Bear deals 5 to the Ogre and takes 3): after damage the Ogre is dead and
/// the Bear alive, and that position SHALL score above the unpumped one
/// (the Bear dead, the Ogre alive) at both leaves.
#[test]
fn a_pump_in_combat_before_damage_keeps_the_outcome_combat_resolves() {
    let (mut pumped, bear, wall, pump) = bear_blocked_by_a_three_three();
    pumped.cast(pump).target_objects(&[bear]).resolve();
    through_combat_damage(&mut pumped);
    let pumped = pumped.state().clone();
    assert!(
        pumped.battlefield.contains(&bear) && !pumped.battlefield.contains(&wall),
        "reach-guard: the pumped Bear killed the Ogre and lived"
    );

    let (mut held, bear, wall, _) = bear_blocked_by_a_three_three();
    through_combat_damage(&mut held);
    let held = held.state().clone();
    assert!(
        !held.battlefield.contains(&bear) && held.battlefield.contains(&wall),
        "reach-guard: the unpumped Bear died to the Ogre"
    );

    let (pumped_strategy, pumped_tactical) = leaf_values(&pumped);
    let (held_strategy, held_tactical) = leaf_values(&held);
    assert!(
        pumped_strategy > held_strategy,
        "strategy leaf: pumped {pumped_strategy:.3} <= held {held_strategy:.3}"
    );
    assert!(
        pumped_tactical > held_tactical,
        "tactical leaf: pumped {pumped_tactical:.3} <= held {held_tactical:.3}"
    );
}

/// The same table through the production search: at P0's priority after
/// blocks, casting the pump SHALL score above passing (the search resolves
/// the combat damage itself).
#[test]
fn the_search_still_casts_the_pump_before_damage() {
    let (runner, _, _, pump) = bear_blocked_by_a_three_three();
    let state = runner.state().clone();
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let scored = crate::search::score_candidates(&state, P0, &config);
    let cast = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == pump))
        .map(|(_, s)| *s);
    let pass = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::PassPriority))
        .map(|(_, s)| *s);
    eprintln!("pump before damage: cast {cast:?} pass {pass:?} all {scored:?}");
    let (cast, pass) = (cast.expect("cast scored"), pass.expect("pass scored"));
    assert!(cast > pass, "cast {cast:.3} <= pass {pass:.3}");
}

/// S4 on 0ad7f7864 (guard): a shrink at the beginning of combat, before
/// attackers. P1's 2/4 under -3/-3 until end of turn is a -1/1; it blocks
/// P0's attacking 3/3, which kills it and takes no damage (CR 510.1a: 0 or
/// less power assigns no combat damage). The engine resolves that combat;
/// the position after damage SHALL score above the unshrunk line, where the
/// 2/4 blocks the 3/3 and both live.
#[test]
fn a_shrink_before_attackers_keeps_the_kill_combat_resolves() {
    fn line(shrink: bool) -> (GameState, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        libraries(&mut scenario);
        scenario.add_basic_land(P0, ManaColor::Black);
        scenario.add_basic_land(P0, ManaColor::Black);
        let gasp = instant_in_hand(&mut scenario, "Gasp", GASP, ManaCostShard::Black, 1);
        let attacker = scenario.add_creature(P0, "Centaur", 3, 3).id();
        let wall = scenario.add_creature(P1, "Wall", 2, 4).id();
        let mut runner = scenario.build();
        runner.advance_to_phase(Phase::BeginCombat);
        if shrink {
            runner.cast(gasp).target_objects(&[wall]).resolve();
        }
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("declare the Centaur");
        let mut guard = 0;
        while !matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner.act(GameAction::PassPriority).expect("to blockers");
            guard += 1;
            assert!(guard < 8, "reach-guard: declare blockers");
        }
        runner
            .declare_blockers(&[(wall, attacker)])
            .expect("Wall blocks");
        through_combat_damage(&mut runner);
        (runner.state().clone(), attacker, wall)
    }
    let (shrunk, attacker, wall) = line(true);
    assert!(
        shrunk.battlefield.contains(&attacker) && !shrunk.battlefield.contains(&wall),
        "reach-guard: the shrunken Wall died blocking and dealt nothing"
    );
    assert_eq!(shrunk.objects[&attacker].damage_marked, 0, "reach-guard");
    let (plain, attacker, wall) = line(false);
    assert!(
        plain.battlefield.contains(&attacker) && plain.battlefield.contains(&wall),
        "reach-guard: both live without the shrink"
    );
    let (shrunk_strategy, shrunk_tactical) = leaf_values(&shrunk);
    let (plain_strategy, plain_tactical) = leaf_values(&plain);
    assert!(
        shrunk_strategy > plain_strategy,
        "strategy leaf: shrunk {shrunk_strategy:.3} <= plain {plain_strategy:.3}"
    );
    assert!(
        shrunk_tactical > plain_tactical,
        "tactical leaf: shrunk {shrunk_tactical:.3} <= plain {plain_tactical:.3}"
    );
}

/// Install an until-end-of-turn "gains flying" on `id` (no P/T change) so
/// the after-cleanup reading is taken, then settle the layers (CR 613.1).
fn flying_until_end_of_turn(state: &mut GameState, id: ObjectId) {
    state.add_transient_continuous_effect(
        id,
        P1,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id },
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Flying,
        }],
        None,
    );
    engine::game::layers::mark_layers_full(state);
    engine::game::layers::flush_layers(state);
}

/// A lasting -1/-1 (no duration: CR 611.2a, until the end of the game) on
/// `id`, then settle the layers.
fn permanent_minus_one(state: &mut GameState, id: ObjectId) {
    state.add_transient_continuous_effect(
        id,
        P0,
        Duration::Permanent,
        TargetFilter::SpecificObject { id },
        vec![
            ContinuousModification::AddPower { value: -1 },
            ContinuousModification::AddToughness { value: -1 },
        ],
        None,
    );
    engine::game::layers::mark_layers_full(state);
    engine::game::layers::flush_layers(state);
}

/// Done-when 3 (guards): lasting P/T changes are read as they are, with an
/// until-end-of-turn effect on the table so the after-cleanup reading runs.
/// P0: a 2/2 with a +1/+1 counter (CR 122.1a, 3/3), a 2/2 under its own
/// anthem enchantment "Creatures you control get +1/+1." (CR 613.4c, so
/// both creatures get +1/+1), a 3/3 under a lasting -1/-1 (2/2). P1: a 1/1
/// that gains flying until end of turn.
#[test]
fn lasting_pt_changes_are_unchanged_by_the_after_cleanup_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    let countered = scenario.add_creature(P0, "Countered", 2, 2).id();
    scenario.with_counter(countered, CounterType::Plus1Plus1, 1);
    scenario.add_enchantment_from_oracle(P0, "Anthem", "Creatures you control get +1/+1.");
    let anthemed = scenario.add_creature(P0, "Anthemed", 2, 2).id();
    let weakened = scenario.add_creature(P0, "Weakened", 3, 3).id();
    let flier = scenario.add_creature(P1, "Flier", 1, 1).id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    permanent_minus_one(state, weakened);
    let mut state = state.clone();
    engine::game::layers::mark_layers_full(&mut state);
    engine::game::layers::flush_layers(&mut state);
    // Lasting: countered 2/2 + counter + anthem = 4/4; anthemed 3/3;
    // weakened 3/3 - 1/1 + anthem = 3/3.
    let lasting = [(countered, 4), (anthemed, 3), (weakened, 3)];
    for (id, pt) in lasting {
        assert_eq!(
            (state.objects[&id].power, state.objects[&id].toughness),
            (Some(pt), Some(pt)),
            "reach-guard: {id:?} reads {pt}/{pt}"
        );
    }
    let before = evaluate_features(&state, P0).expect("non-terminal");
    flying_until_end_of_turn(&mut state, flier);
    assert!(
        state.objects[&flier].has_keyword(&Keyword::Flying),
        "reach-guard: the 1/1 flies until end of turn"
    );
    let after = evaluate_features(&state, P0).expect("non-terminal");
    assert_eq!(after.board_power, 10.0 - 1.0, "power: 4 + 3 + 3 - 1");
    assert_eq!(
        after.board_toughness,
        10.0 - 1.0,
        "toughness: 4 + 3 + 3 - 1"
    );
    assert_eq!(after.board_power, before.board_power);
    assert_eq!(after.board_toughness, before.board_toughness);
    assert_eq!(after.board_presence, before.board_presence);
}

/// A lasting change and an until-end-of-turn one on the same creature: only
/// the lasting one is read. A 2/2 with a +1/+1 counter, then +3/+3 until end
/// of turn (6/6 now): the leaf reads 3/3.
#[test]
fn a_counter_under_a_pump_reads_only_the_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, 1);
    let pump = instant_in_hand(&mut scenario, "Pump", PUMP, ManaCostShard::Green, 0);
    let mut runner = scenario.build();
    runner.cast(pump).target_objects(&[bear]).resolve();
    let state = runner.state().clone();
    assert_eq!(state.objects[&bear].power, Some(6), "reach-guard: 6/6 now");
    let features = evaluate_features(&state, P0).expect("non-terminal");
    assert_eq!(features.board_power, 3.0);
    assert_eq!(features.board_toughness, 3.0);
}

/// Done-when 4: power below 0 counts as 0 in the board sum, from any source
/// (CR 107.1b allows a negative power; CR 510.1a: it assigns no combat
/// damage). A lasting -1/-1 times three on P1's 1/5 leaves a -2/2 for good;
/// P0's own 1/5 under the same is a -2/2 too. Each side's board power is 0.
#[test]
fn negative_power_counts_as_zero_in_the_board_sum() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    let theirs = scenario.add_creature(P1, "Their Traxos", 1, 5).id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    for _ in 0..3 {
        permanent_minus_one(state, theirs);
    }
    let opposing_only = state.clone();
    assert_eq!(
        opposing_only.objects[&theirs].power,
        Some(-2),
        "reach-guard: a lasting -2/2"
    );
    let features = evaluate_features(&opposing_only, P0).expect("non-terminal");
    assert_eq!(
        features.board_power, 0.0,
        "an opposing -2 power is not credited to the AI"
    );
    assert_eq!(features.board_toughness, -2.0);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    let mine = scenario.add_creature(P0, "My Traxos", 1, 5).id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    for _ in 0..3 {
        permanent_minus_one(state, mine);
    }
    let features = evaluate_features(state, P0).expect("non-terminal");
    assert_eq!(features.board_power, 0.0, "an own -2 power costs nothing");
}

/// The L4 survivor on a 1/5 (the seed 1011 T10 shape): Gasp leaves a -2/2
/// until cleanup; the leaf reads the 1/5 it will be, and neither the -2 nor
/// the -3/-3 is credited.
#[test]
fn a_shrink_to_negative_power_reads_the_creature_after_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    let gasp = instant_in_hand(&mut scenario, "Gasp", GASP, ManaCostShard::Black, 1);
    let traxos = scenario.add_creature(P1, "Traxos", 1, 5).id();
    let mut runner = scenario.build();
    runner.cast(gasp).target_objects(&[traxos]).resolve();
    let state = runner.state().clone();
    assert_eq!(
        state.objects[&traxos].power,
        Some(-2),
        "reach-guard: -2/2 now"
    );
    let features = evaluate_features(&state, P0).expect("non-terminal");
    assert_eq!(features.board_power, -1.0);
    assert_eq!(features.board_toughness, -5.0);
}

/// CR 514.2 + CR 514.3a + CR 704.5f: a creature that an until-end-of-turn
/// pump is keeping alive dies in the cleanup step once the pump ends. P0's
/// 2/2 under a lasting -1/-1 three times (dead) is first given +3/+3 until
/// end of turn: a 2/2 now, a -1/-1 after cleanup. The leaf counts nothing
/// for it.
#[test]
fn a_creature_kept_alive_by_a_pump_is_not_on_the_board_after_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let pump = instant_in_hand(&mut scenario, "Pump", PUMP, ManaCostShard::Green, 0);
    let mut runner = scenario.build();
    runner.cast(pump).target_objects(&[bear]).resolve();
    let state = runner.state_mut();
    for _ in 0..3 {
        permanent_minus_one(state, bear);
    }
    let state = state.clone();
    assert!(
        state.battlefield.contains(&bear) && state.objects[&bear].toughness == Some(2),
        "reach-guard: the pumped Bear is a 2/2 now"
    );
    let features = evaluate_features(&state, P0).expect("non-terminal");
    assert_eq!(features.board_presence, 0.0);
    assert_eq!(features.board_power, 0.0);
    assert_eq!(features.board_toughness, 0.0);
}
