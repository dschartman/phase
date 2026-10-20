//! The judge's constructed rows on f684984f4 (spec-ai-leaf-eot-pt), kept as
//! permanent rows by `simforge/docs/spec-ai-precombat-prowess-2026-10-20.md`:
//! J1 (without its comparative assert), J2, J5, J6, J7. Each was green at
//! f684984f4 (J1, J5, J6 and J7 red at fe23db171, the leaf before it read P/T
//! after this turn's cleanup).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

use super::PlannerServices;
use crate::config::{create_config, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::eval::{evaluate_features, threat_level};

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

fn spell(
    scenario: &mut GameScenario,
    name: &str,
    text: &str,
    instant: bool,
    shard: ManaCostShard,
    generic: u32,
) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, name, instant)
        .from_oracle_text(text)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![shard],
            generic,
        })
        .id()
}

/// The rollout (utility) leaf for P0: VeryHard measurement settings.
fn rollout_leaf(state: &GameState) -> f64 {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let policies = crate::policies::PolicyRegistry::shared();
    let context = AiContext::empty(&config.weights);
    let mut services = PlannerServices::new(P0, &config, policies, context);
    services.quiesced_leaf_eval(state)
}

fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool, cap: usize, what: &str) {
    let mut passes = 0;
    while !done(runner.state()) {
        runner
            .act(GameAction::PassPriority)
            .unwrap_or_else(|e| panic!("{what}: {e:?}"));
        passes += 1;
        assert!(passes < cap, "reach-guard: {what}");
    }
}

/// J1: the AI's unblocked 2/2 attacker pumped +3/+3 before damage; the leaf
/// after damage keeps the 5 damage dealt (life) and reads the Bear as a 2/2
/// (CR 510.2: combat damage is dealt with the pumped power; CR 514.2: the pump
/// ends at cleanup).
fn j1_line(pump: bool) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let pump_id = spell(&mut scenario, "Pump", PUMP, true, ManaCostShard::Green, 0);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("attack");
    if pump {
        runner.cast(pump_id).target_objects(&[bear]).resolve();
    }
    let mut steps = 0;
    while matches!(
        runner.state().phase,
        Phase::DeclareAttackers | Phase::DeclareBlockers | Phase::CombatDamage
    ) || !runner.state().stack.is_empty()
    {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner.declare_blockers(&[]).expect("no blocks");
        } else {
            runner.act(GameAction::PassPriority).expect("pass");
        }
        steps += 1;
        assert!(steps < 16, "reach-guard: past damage");
    }
    runner.state().clone()
}

#[test]
fn j1_pump_on_attacker_after_damage_keeps_the_damage() {
    let pumped = j1_line(true);
    let plain = j1_line(false);
    assert_eq!(pumped.players[1].life, 15, "reach-guard: 5 dealt");
    assert_eq!(plain.players[1].life, 18, "reach-guard: 2 dealt");
    let features = evaluate_features(&pumped, P0).expect("non-terminal");
    assert_eq!(features.life, 5.0, "damage dealt is kept");
    assert_eq!(features.board_power, 2.0, "the Bear reads as a 2/2");
}

/// J2: CR 514.2 removes damage and ends until-end-of-turn effects
/// simultaneously. P1's 2/2 under +3/+3 with 4 damage marked, and P1's 4/4
/// with 2 damage under -1/-1: both survive cleanup (2/2 and 4/4). The leaf
/// SHALL read them alive at 2/2 and 4/4, and the engine SHALL agree.
#[test]
fn j2_marked_damage_and_transient_pt_end_together() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Black);
    let small = scenario.add_creature(P1, "Small", 2, 2).id();
    let big = scenario.add_creature(P1, "Big", 4, 4).id();
    let pump_id = spell(&mut scenario, "Pump", PUMP, true, ManaCostShard::Green, 0);
    let wither = spell(
        &mut scenario,
        "Wither",
        "Target creature gets -1/-1 until end of turn.",
        true,
        ManaCostShard::Black,
        0,
    );
    let mut runner = scenario.build();
    runner.cast(pump_id).target_objects(&[small]).resolve();
    runner.cast(wither).target_objects(&[big]).resolve();
    runner
        .state_mut()
        .objects
        .get_mut(&small)
        .expect("small")
        .damage_marked = 4;
    runner
        .state_mut()
        .objects
        .get_mut(&big)
        .expect("big")
        .damage_marked = 2;
    let st = runner.state().clone();
    assert_eq!(st.objects[&small].toughness, Some(5), "reach-guard");
    assert_eq!(st.objects[&big].toughness, Some(3), "reach-guard");
    let features = evaluate_features(&st, P0).expect("non-terminal");
    let turn = st.turn_number;
    pass_until(&mut runner, |x| x.turn_number != turn, 40, "next turn");
    let after = runner.state();
    assert!(
        after.battlefield.contains(&small) && after.battlefield.contains(&big),
        "engine: both survive cleanup (CR 514.2)"
    );
    assert_eq!(
        (
            features.board_presence,
            features.board_power,
            features.board_toughness
        ),
        (-2.0, -6.0, -6.0)
    );
}

/// J5: an until-end-of-turn anthem on the AI's side cast in the opponent's
/// upkeep. After that turn's cleanup the 2/2s are 2/2s (CR 514.2).
#[test]
fn j5_anthem_on_opponents_turn_reads_after_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_creature(P0, "A", 2, 2);
    scenario.add_creature(P0, "B", 2, 2);
    let rally = spell(
        &mut scenario,
        "Rally",
        "Creatures you control get +2/+2 until end of turn.",
        true,
        ManaCostShard::White,
        0,
    );
    let mut runner = scenario.build();
    pass_until(
        &mut runner,
        |x| {
            x.active_player == P1
                && matches!(x.waiting_for, WaitingFor::Priority { player } if player == P0)
        },
        40,
        "P0 priority in P1's turn",
    );
    runner.cast(rally).resolve();
    let features = evaluate_features(runner.state(), P0).expect("non-terminal");
    assert_eq!((features.board_power, features.board_toughness), (4.0, 4.0));
}

/// J6: the rollout (utility) leaf on L4 (P0's Last Gasp on P1's 3/4, which
/// survives at 0/1 until cleanup, or on P1's 1/1, which dies): the kill SHALL
/// score above the survivor. The threat_level residual of the -3/-3 is
/// printed (judge S2 on f684984f4).
#[test]
fn j6_l4_at_the_rollout_leaf_and_threat_residual() {
    fn l4(target_big: Option<bool>) -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        libraries(&mut scenario);
        scenario.add_basic_land(P0, ManaColor::Black);
        scenario.add_basic_land(P0, ManaColor::Black);
        let gasp = spell(&mut scenario, "Gasp", GASP, true, ManaCostShard::Black, 1);
        let big = scenario.add_creature(P1, "Big", 3, 4).id();
        let small = scenario.add_creature(P1, "Small", 1, 1).id();
        let mut runner = scenario.build();
        if let Some(pick_big) = target_big {
            runner
                .cast(gasp)
                .target_objects(&[if pick_big { big } else { small }])
                .resolve();
        }
        runner.state().clone()
    }
    let survivor = l4(Some(true));
    let kill = l4(Some(false));
    let untouched = l4(None);
    let survivor_value = rollout_leaf(&survivor);
    let kill_value = rollout_leaf(&kill);
    let threat_survivor = threat_level(&survivor, P0, P1);
    let threat_untouched = threat_level(&untouched, P0, P1);
    eprintln!(
        "J6 utility survivor {survivor_value:.3} kill {kill_value:.3}; threat residual {:.4}",
        threat_untouched - threat_survivor
    );
    assert!(
        kill_value > survivor_value,
        "kill SHALL score above the survivor at the rollout leaf"
    );
}

/// J7: a land animated until end of turn (type-changing, CR 613.1d) on the
/// AI's side in its postcombat main: after cleanup it is a land again, so it
/// is no creature on the board and still a mana source.
#[test]
fn j7_animated_land_reads_as_a_land_after_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Green);
    let target = scenario.add_basic_land(P0, ManaColor::Green);
    let animate = spell(
        &mut scenario,
        "Animate",
        "Target land you control becomes a 3/3 Elemental creature until end of turn. It's still a land.",
        true,
        ManaCostShard::Green,
        0,
    );
    let mut runner = scenario.build();
    let before = evaluate_features(runner.state(), P0).expect("non-terminal");
    runner.cast(animate).target_objects(&[target]).resolve();
    let st = runner.state().clone();
    assert!(
        st.objects[&target]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach-guard: the land is a creature now"
    );
    let features = evaluate_features(&st, P0).expect("non-terminal");
    assert_eq!(
        (
            features.board_presence,
            features.board_power,
            features.board_toughness
        ),
        (0.0, 0.0, 0.0)
    );
    assert_eq!(
        features.mana_development_offset, before.mana_development_offset,
        "still a mana source"
    );
}
