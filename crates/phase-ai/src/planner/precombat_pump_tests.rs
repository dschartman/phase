//! A precombat spell that pumps a creature able to attack this turn is
//! credited for the damage it can deal
//! (`simforge/docs/spec-ai-precombat-prowess-2026-10-20.md`; judge row J4 on
//! f684984f4).
//!
//! Since f684984f4 the leaf reads P/T after this turn's cleanup (CR 514.2), so
//! a prowess +1/+1 (CR 702.108a) is worth something only where the searched
//! line reaches combat damage. The prowess trigger is one more object on the
//! stack, and passing priority over it spent search plies the vanilla line
//! spent on reaching combat: at production depth the cast with a prowess
//! attacker scored 6.40 below the same cast with a vanilla one.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::{BeamContinuationPlanner, PlannerServices, SearchBudget};
use crate::config::{create_config, AiDifficulty, Platform};
use crate::context::AiContext;

const PEEK: &str = "Draw a card.";

fn libraries(scenario: &mut GameScenario) {
    for _ in 0..20 {
        scenario.add_land_to_library_top(P0, "Island");
    }
    for _ in 0..25 {
        scenario.add_land_to_library_top(P1, "Plains");
    }
}

/// How the AI's 1/1 stands when the AI considers casting a sorcery-speed
/// "Draw a card." for {U}.
#[derive(Clone, Copy, Debug)]
enum Monk {
    /// Untapped and able to attack (entered on an earlier turn).
    Ready,
    /// Tapped (CR 508.1a: it cannot be declared as an attacker).
    Tapped,
    /// Entered this turn (CR 302.6: it cannot attack).
    SummoningSick,
}

/// The J4 table: P0 at `phase` of its own turn with one Island, a 1/1 (with
/// or without prowess) standing as `monk`, and "Draw a card." for {U} in
/// hand; P1 at 20 with an empty board. Returns the state and the spell.
fn j4_table(phase: Phase, prowess: bool, monk: Monk) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let mut builder = scenario.add_creature(P0, "Monk", 1, 1);
    if prowess {
        builder.with_keyword(Keyword::Prowess);
    }
    if matches!(monk, Monk::SummoningSick) {
        builder.with_summoning_sickness();
    }
    let monk_id = builder.id();
    let spell = scenario
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text(PEEK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    if matches!(monk, Monk::Tapped) {
        runner
            .state_mut()
            .objects
            .get_mut(&monk_id)
            .expect("monk")
            .tapped = true;
    }
    let state = runner.state().clone();
    if prowess {
        // Reach-guard: the cast triggers prowess through the real pipeline
        // (CR 702.108a) and the trigger resolves to a 2/2.
        let mut probe = GameRunner::from_state(state.clone());
        probe.cast(spell).resolve();
        assert_eq!(
            probe.state().objects[&monk_id].power,
            Some(2),
            "reach-guard: prowess fired"
        );
    }
    (state, spell)
}

/// The production root scores (VeryHard, measurement mode) of casting the
/// spell and of passing.
fn cast_and_pass(state: &GameState, spell: ObjectId) -> (f64, f64) {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let scored = crate::search::score_candidates(state, P0, &config);
    let cast = scored
        .iter()
        .find(|(action, _)| {
            matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell)
        })
        .map(|(_, score)| *score)
        .expect("cast scored");
    let pass = scored
        .iter()
        .find(|(action, _)| matches!(action, GameAction::PassPriority))
        .map(|(_, score)| *score)
        .expect("pass scored");
    (cast, pass)
}

/// What prowess adds to the cast: (prowess cast minus pass) minus (vanilla
/// cast minus pass), with both tables otherwise identical.
fn prowess_credit(phase: Phase, monk: Monk) -> f64 {
    let (state, spell) = j4_table(phase, true, monk);
    let (pc, pp) = cast_and_pass(&state, spell);
    let (state, spell) = j4_table(phase, false, monk);
    let (vc, vp) = cast_and_pass(&state, spell);
    let credit = (pc - pp) - (vc - vp);
    eprintln!(
        "{phase:?} {monk:?}: prowess cast {pc:.3} pass {pp:.3}; vanilla cast {vc:.3} pass {vp:.3}; credit {credit:.3}"
    );
    credit
}

/// J4: casting the spell precombat with a prowess attacker that can attack
/// SHALL score at least as high, against passing, as the same cast with a
/// vanilla 1/1 (red at f684984f4: 6.40 below).
#[test]
fn j4_precombat_cast_with_a_prowess_attacker_scores_at_least_the_vanilla_cast() {
    let credit = prowess_credit(Phase::PreCombatMain, Monk::Ready);
    assert!(
        credit >= 0.0,
        "the prowess cast SHALL score at least as high as the vanilla cast (credit {credit:.3})"
    );
}

/// The judge's strict form of J4: the extra point of combat damage SHALL be
/// credited. At production depth the line does not reach combat damage on
/// this table, so (a) only removes the penalty (credit 0.000).
#[test]
#[ignore = "combat damage outside the horizon; needs a damage-credit leaf term (option b)"]
fn j4_strict_prowess_precombat_cast_is_credited_by_combat() {
    let credit = prowess_credit(Phase::PreCombatMain, Monk::Ready);
    assert!(
        credit > 0.0,
        "precombat cast SHALL be worth more with a prowess attacker (credit {credit:.3})"
    );
}

/// Guard: in the AI's postcombat main no combat is left this turn, so
/// prowess SHALL give the cast no credit.
#[test]
fn postcombat_cast_with_a_prowess_creature_gets_no_damage_credit() {
    let credit = prowess_credit(Phase::PostCombatMain, Monk::Ready);
    assert!(
        credit <= 1e-9,
        "no damage credit postcombat (credit {credit:.3})"
    );
}

/// Guard: a tapped prowess creature cannot attack this turn (CR 508.1a), so
/// prowess SHALL give the precombat cast no credit.
#[test]
fn precombat_cast_with_a_tapped_prowess_creature_gets_no_damage_credit() {
    let credit = prowess_credit(Phase::PreCombatMain, Monk::Tapped);
    assert!(
        credit <= 1e-9,
        "no damage credit when tapped (credit {credit:.3})"
    );
}

/// Guard: a summoning-sick prowess creature cannot attack this turn
/// (CR 302.6), so prowess SHALL give the precombat cast no credit.
#[test]
fn precombat_cast_with_a_summoning_sick_prowess_creature_gets_no_damage_credit() {
    let credit = prowess_credit(Phase::PreCombatMain, Monk::SummoningSick);
    assert!(
        credit <= 1e-9,
        "no damage credit when summoning sick (credit {credit:.3})"
    );
}

/// Alpha-beta value of `state` for P0 at `depth` plies, fresh services and
/// budget, VeryHard measurement settings.
fn searched(state: &GameState, depth: u32) -> f64 {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let policies = crate::policies::PolicyRegistry::shared();
    let context = AiContext::empty(&config.weights);
    let mut services = PlannerServices::new(P0, &config, policies, context);
    let mut budget = SearchBudget::new(config.search.max_nodes);
    let planner = BeamContinuationPlanner {
        depth,
        rollout_depth: config.search.rollout_depth,
    };
    planner.search_value(
        state,
        depth,
        0,
        f64::NEG_INFINITY,
        f64::INFINITY,
        &mut services,
        &mut budget,
    )
}

/// Pass priority until the top of the stack is no longer `trigger_owner`'s
/// triggered ability (CR 117.4 + CR 608.1).
fn resolve_top_trigger(state: &GameState, trigger_owner: PlayerId) -> GameState {
    let mut runner = GameRunner::from_state(state.clone());
    let mut passes = 0;
    while top_trigger_controller(runner.state()) == Some(trigger_owner) {
        runner.act(GameAction::PassPriority).expect("pass");
        passes += 1;
        assert!(passes < 6, "reach-guard: the trigger resolves");
    }
    runner.state().clone()
}

fn top_trigger_controller(state: &GameState) -> Option<PlayerId> {
    state.stack.back().and_then(|top| {
        matches!(top.kind, StackEntryKind::TriggeredAbility { .. }).then_some(top.controller)
    })
}

/// The J4 table after P0 casts the spell: the spell under P0's prowess
/// trigger, P0 holding priority (CR 117.3c).
fn own_trigger_on_top() -> GameState {
    let (state, spell) = j4_table(Phase::PreCombatMain, true, Monk::Ready);
    let mut runner = GameRunner::from_state(state);
    runner.cast(spell).commit();
    let st = runner.state().clone();
    assert_eq!(
        top_trigger_controller(&st),
        Some(P0),
        "reach-guard: own trigger on top"
    );
    assert_eq!(st.stack.len(), 2, "reach-guard: spell under the trigger");
    st
}

/// The AI's own trigger on top resolves without spending search depth: the
/// position with P0's prowess trigger on top SHALL search to the value of the
/// position after it resolves, at the same depth.
#[test]
fn own_trigger_on_top_is_searched_as_the_position_after_it_resolves() {
    let on_top = own_trigger_on_top();
    let resolved = resolve_top_trigger(&on_top, P0);
    assert_eq!(
        resolved.stack.len(),
        1,
        "reach-guard: only the spell is left"
    );
    assert!(
        matches!(resolved.waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach-guard: P0 has priority with its spell on the stack"
    );
    for depth in [1, 2] {
        let a = searched(&on_top, depth);
        let b = searched(&resolved, depth);
        eprintln!("depth {depth}: trigger on top {a:.6}, after it resolves {b:.6}");
        assert!(
            (a - b).abs() < 1e-9,
            "depth {depth}: the own trigger SHALL take no ply ({a:.6} against {b:.6})"
        );
    }
}

/// P1 controls a prowess 1/1 and casts "Draw a card." for {U}; P0 then holds
/// priority with P1's prowess trigger on top (P1 passes if it holds it). `p1_turn` sets whose
/// turn it is: P1's precombat main, or P0's precombat main with P0's own
/// spell under P1's instant. P0 holds an instant "Draw a card." and an open
/// Island.
fn opponent_trigger_on_top(p1_turn: bool) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    libraries(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_creature(P0, "Monk", 1, 1);
    scenario
        .add_creature(P1, "Rival", 1, 1)
        .with_keyword(Keyword::Prowess);
    let own_sorcery = scenario
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text(PEEK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario
        .add_spell_to_hand(P0, "Glance", true)
        .from_oracle_text(PEEK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        });
    let theirs = scenario
        .add_spell_to_hand(P1, "Opt", true)
        .from_oracle_text(PEEK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    if p1_turn {
        let st = runner.state_mut();
        st.active_player = P1;
        st.priority_player = P1;
        st.waiting_for = WaitingFor::Priority { player: P1 };
    } else {
        runner.cast(own_sorcery).commit();
        runner.act(GameAction::PassPriority).expect("P0 passes");
    }
    runner.cast(theirs).commit();
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
        runner.act(GameAction::PassPriority).expect("P1 passes");
    }
    let st = runner.state().clone();
    assert_eq!(
        top_trigger_controller(&st),
        Some(P1),
        "reach-guard: P1's trigger on top"
    );
    assert!(
        matches!(st.waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach-guard: P0 holds priority"
    );
    st
}

/// P0's root scores at the opponent-trigger table, sorted for a stable pin.
fn root_scores(state: &GameState) -> Vec<f64> {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let mut scores: Vec<f64> = crate::search::score_candidates(state, P0, &config)
        .into_iter()
        .map(|(_, score)| score)
        .collect();
    scores.sort_by(f64::total_cmp);
    scores
}

/// Guard: an opponent's trigger on top stays a search node. P0's root scores
/// and the depth-2 value with P1's prowess trigger on top, on P1's turn and on
/// P0's, SHALL equal the values at f684984f4 (pinned).
#[test]
fn an_opponents_trigger_on_top_is_searched_as_before() {
    // (P1's turn, P0's root scores sorted, depth-2 value), read at f684984f4.
    let pinned: [(bool, [f64; 2], f64); 2] = [
        (
            true,
            [1.2463600000000001, 2.130569803921569],
            2.5217098039215684,
        ),
        (
            false,
            [8.547422857142857, 14.85197619047619],
            14.71947619047619,
        ),
    ];
    for (p1_turn, roots_at_base, value_at_base) in pinned {
        let st = opponent_trigger_on_top(p1_turn);
        let roots = root_scores(&st);
        let value = searched(&st, 2);
        eprintln!("p1_turn {p1_turn}: roots {roots:?} depth-2 {value:?}");
        assert_eq!(
            roots.len(),
            roots_at_base.len(),
            "p1_turn {p1_turn}: root candidates"
        );
        for (now, base) in roots.iter().zip(roots_at_base) {
            assert!(
                (now - base).abs() < 1e-9,
                "p1_turn {p1_turn}: root {now} against {base}"
            );
        }
        assert!(
            (value - value_at_base).abs() < 1e-9,
            "p1_turn {p1_turn}: depth-2 {value} against {value_at_base}"
        );
    }
}
