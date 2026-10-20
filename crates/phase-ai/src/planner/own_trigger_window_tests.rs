//! The opponent's priority over the AI's own triggered ability on top of the
//! stack (`simforge/docs/spec-ai-opponent-window-over-own-trigger-2026-10-20.md`;
//! judge rows JR1 to JR6 on 3b773d68b, `fork-checks/3b773d68b/JUDGE.md`).
//!
//! Since 3b773d68b the AI's own trigger on top resolves with every player
//! passing and no depth spent. That removed the prowess penalty but made the
//! opponent's response window over the trigger a forced pass (CR 117.3d: the
//! opponent receives priority after the AI passes; CR 117.1a: it may cast an
//! instant then; CR 117.4: the trigger resolves only when all players pass in
//! succession). These rows hold the
//! no-depth resolution where the opponent can only pass, and the window as a
//! searched response where it can act: a -3/-3 instant over a prowess trigger
//! (JR5), Break Under Pressure over Mind Meanderer's fight trigger, and
//! Multiply by Zero over Traxos's prowess trigger.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::{BeamContinuationPlanner, PlannerServices, SearchBudget};
use crate::config::{create_config, AiDifficulty, Platform};
use crate::context::AiContext;

/// Verbatim from `client/public/card-data.json` (Mind Meanderer, FRA, 4/4,
/// {3}{G}{U}{U}).
const MIND_MEANDERER: &str = "Flying\nThis creature has vigilance as long as you control a \
    Jace planeswalker.\nWhen this creature enters, it fights up to one target creature an \
    opponent controls. (Each deals damage equal to its power to the other.)";

/// Verbatim from `client/public/card-data.json` (Break Under Pressure, FRA,
/// instant, {2}{B}).
const BREAK_UNDER_PRESSURE: &str = "Target opponent sacrifices a creature or planeswalker with \
    the greatest mana value among creatures and planeswalkers they control. You gain 2 life.";

/// Verbatim from `client/public/card-data.json` (Multiply by Zero, FRA,
/// instant, {1}{B}).
const MULTIPLY_BY_ZERO: &str =
    "Target creature has base power and toughness 0/0 until end of turn.";

const GASP: &str = "Target creature gets -3/-3 until end of turn.";
const PEEK: &str = "Draw a card.";

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn u() -> ManaCost {
    cost(vec![ManaCostShard::Blue], 0)
}

fn libraries(scenario: &mut GameScenario, own: usize) {
    for _ in 0..own {
        scenario.add_land_to_library_top(P0, "Island");
    }
    for _ in 0..25 {
        scenario.add_land_to_library_top(P1, "Plains");
    }
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

/// The production root scores (VeryHard, measurement mode) of casting
/// `spell` and of passing.
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

fn top_trigger_controller(state: &GameState) -> Option<PlayerId> {
    state.stack.back().and_then(|top| {
        matches!(top.kind, StackEntryKind::TriggeredAbility { .. }).then_some(top.controller)
    })
}

/// Pass while a player has priority and P0's trigger is on top (CR 117.4:
/// the trigger resolves when all players pass in succession).
fn pass_over_own_triggers(state: &GameState) -> GameState {
    let mut runner = GameRunner::from_state(state.clone());
    for _ in 0..12 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || top_trigger_controller(runner.state()) != Some(P0)
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    runner.state().clone()
}

/// Pass priority until the stack is empty (or a non-priority prompt).
fn drain_stack(state: &GameState) -> GameState {
    let mut runner = GameRunner::from_state(state.clone());
    for _ in 0..12 {
        if runner.state().stack.is_empty()
            || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    runner.state().clone()
}

fn on_battlefield(state: &GameState, object: ObjectId) -> bool {
    state.battlefield.contains(&object)
}

/// P0 precombat main, one Island, a P0 creature "Engine" with `oracle` (and
/// a vanilla 5/5 when `extra_big`), sorcery `spell_text` for {U} in hand, P0
/// library `own_lib` Islands, P1 library 25.
fn table(oracle: &str, extra_big: bool, spell_text: &str, own_lib: usize) -> (GameState, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    libraries(&mut s, own_lib);
    s.add_basic_land(P0, ManaColor::Blue);
    s.add_creature(P0, "Engine", 1, 1).from_oracle_text(oracle);
    if extra_big {
        s.add_creature(P0, "Big", 5, 5);
    }
    let spell = s
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text(spell_text)
        .with_mana_cost(u())
        .id();
    let runner = s.build();
    (runner.state().clone(), spell)
}

/// `state` after P0 casts `spell`: P0's trigger on top, P0 holding priority
/// (CR 117.3c).
fn on_top(state: &GameState, spell: ObjectId) -> GameState {
    let mut r = GameRunner::from_state(state.clone());
    r.cast(spell).commit();
    let st = r.state().clone();
    assert_eq!(
        top_trigger_controller(&st),
        Some(P0),
        "reach-guard: own trigger on top"
    );
    st
}

/// `top` after P0 passes priority over its own trigger: P1 SHALL hold
/// priority (reach-guard), and the returned runner stands there.
fn opponent_window(top: &GameState) -> GameRunner {
    let mut r = GameRunner::from_state(top.clone());
    r.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "reach-guard: P1 holds priority over P0's trigger"
    );
    assert_eq!(
        top_trigger_controller(r.state()),
        Some(P0),
        "reach-guard: P0's trigger is still on top"
    );
    r
}

/// JR1 loop: an own trigger whose resolution makes another own trigger
/// (lifegain feeding lifegain) SHALL terminate the search with a finite
/// value in bounded time.
#[test]
fn jr1_own_trigger_loop_terminates() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    libraries(&mut s, 20);
    s.add_basic_land(P0, ManaColor::Blue);
    s.add_creature(P0, "Caster", 1, 1)
        .from_oracle_text("Whenever you cast a noncreature spell, you gain 1 life.");
    s.add_creature(P0, "Looper", 1, 1)
        .from_oracle_text("Whenever you gain life, you gain 1 life.");
    let spell = s
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text(PEEK)
        .with_mana_cost(u())
        .id();
    let st = s.build().state().clone();
    let top = on_top(&st, spell);
    let after = pass_over_own_triggers(&top);
    eprintln!(
        "JR1 life {} -> {} after passes; top own trigger {:?}",
        top.players[0].life,
        after.players[0].life,
        top_trigger_controller(&after)
    );
    let t = std::time::Instant::now();
    let v1 = searched(&top, 1);
    let v2 = searched(&top, 2);
    let (c, p) = cast_and_pass(&st, spell);
    let el = t.elapsed();
    eprintln!("JR1 depth1 {v1:.3} depth2 {v2:.3} root cast {c:.3} pass {p:.3} in {el:?}");
    assert!(v1.is_finite() && v2.is_finite() && c.is_finite());
    assert!(el.as_secs() < 60, "terminates in bounded time ({el:?})");
}

/// JR1 with an opposing response: the same lifegain loop while P1 holds a
/// -3/-3 instant and {B}, so every pass over the loop's trigger is an
/// opponent window searched at no depth. The search SHALL terminate with a
/// finite value in bounded time (the windows on a path are capped at
/// `MAX_FREE_OPPONENT_WINDOWS`; the rollout charges no node budget and
/// measurement mode sets no deadline).
#[test]
fn own_trigger_loop_with_an_opposing_response_terminates() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    libraries(&mut s, 20);
    s.add_basic_land(P0, ManaColor::Blue);
    s.add_basic_land(P1, ManaColor::Black);
    s.add_creature(P0, "Caster", 1, 1)
        .from_oracle_text("Whenever you cast a noncreature spell, you gain 1 life.");
    s.add_creature(P0, "Looper", 1, 1)
        .from_oracle_text("Whenever you gain life, you gain 1 life.");
    let spell = s
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text(PEEK)
        .with_mana_cost(u())
        .id();
    s.add_spell_to_hand(P1, "Gasp", true)
        .from_oracle_text(GASP)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 0));
    let st = s.build().state().clone();
    let top = on_top(&st, spell);
    let window = opponent_window(&top);
    assert!(
        engine::ai_support::legal_actions(window.state())
            .iter()
            .any(|action| !matches!(action, GameAction::PassPriority)),
        "reach-guard: P1 can respond over the loop's trigger"
    );
    let t = std::time::Instant::now();
    let v1 = searched(&top, 1);
    let v2 = searched(&top, 2);
    let (c, p) = cast_and_pass(&st, spell);
    let el = t.elapsed();
    eprintln!(
        "loop with response: depth1 {v1:.3} depth2 {v2:.3} root cast {c:.3} pass {p:.3} in {el:?}"
    );
    assert!(v1.is_finite() && v2.is_finite() && c.is_finite() && p.is_finite());
    assert!(el.as_secs() < 60, "terminates in bounded time ({el:?})");
}

/// JR2 may-trigger: "you may draw" on top SHALL search as the position at
/// its optional prompt (the choice stays a node; no depth spent on the
/// passes).
#[test]
fn jr2_may_trigger_searches_as_its_prompt() {
    let (st, spell) = table(
        "Whenever you cast a noncreature spell, you may draw a card.",
        false,
        "You gain 1 life.",
        20,
    );
    let top = on_top(&st, spell);
    let prompt = pass_over_own_triggers(&top);
    eprintln!("JR2 prompt waiting_for {:?}", prompt.waiting_for);
    assert!(
        !matches!(prompt.waiting_for, WaitingFor::Priority { .. }),
        "reach-guard: a resolution prompt"
    );
    for d in [1, 2] {
        let a = searched(&top, d);
        let b = searched(&prompt, d);
        eprintln!("JR2 depth {d}: on top {a:.6} prompt {b:.6}");
        assert!((a - b).abs() < 1e-9, "depth {d}: {a} vs {b}");
    }
}

/// JR3 draw two at library 1: the own trigger on top SHALL search as the
/// loss it resolves to, and the cast SHALL score below passing.
#[test]
fn jr3_draw_trigger_at_short_library_reaches_the_score() {
    let (st, spell) = table(
        "Whenever you cast a noncreature spell, draw two cards.",
        false,
        "You gain 1 life.",
        1,
    );
    let top = on_top(&st, spell);
    let resolved = pass_over_own_triggers(&top);
    eprintln!("JR3 resolved waiting_for {:?}", resolved.waiting_for);
    let a = searched(&top, 1);
    let b = searched(&resolved, 1);
    let (c, p) = cast_and_pass(&st, spell);
    eprintln!("JR3 on top {a:.3} resolved {b:.3}; root cast {c:.3} pass {p:.3}");
    assert!((a - b).abs() < 1e-9, "on top {a} vs resolved {b}");
    assert!(c < p, "cast {c} SHALL score below pass {p}");
}

/// JR4 sacrifice with a choice: "sacrifice a creature" with a 1/1 and a 5/5
/// SHALL search as the position at the sacrifice prompt.
#[test]
fn jr4_sacrifice_trigger_choice_stays_a_node() {
    let (st, spell) = table(
        "Whenever you cast a noncreature spell, sacrifice a creature.",
        true,
        PEEK,
        20,
    );
    let top = on_top(&st, spell);
    let prompt = pass_over_own_triggers(&top);
    eprintln!("JR4 prompt waiting_for {:?}", prompt.waiting_for);
    for d in [1, 2] {
        let a = searched(&top, d);
        let b = searched(&prompt, d);
        eprintln!("JR4 depth {d}: on top {a:.6} prompt {b:.6}");
        assert!((a - b).abs() < 1e-9, "depth {d}: {a} vs {b}");
    }
    let (c, p) = cast_and_pass(&st, spell);
    eprintln!("JR4 root cast {c:.3} pass {p:.3}");
}

/// JR6 self-mill to 0: "mill two cards" with library 2 SHALL score the cast
/// below passing.
#[test]
fn jr6_self_mill_trigger_into_empty_library_reaches_the_score() {
    let (st, spell) = table(
        "Whenever you cast a noncreature spell, mill two cards.",
        false,
        "You gain 1 life.",
        2,
    );
    let (c, p) = cast_and_pass(&st, spell);
    let top = on_top(&st, spell);
    let resolved = pass_over_own_triggers(&top);
    eprintln!(
        "JR6 library after {} ; root cast {c:.3} pass {p:.3}",
        resolved.players[0].library.len()
    );
    assert!(c < p, "cast {c} SHALL score below pass {p}");
}

/// P0's 2/3 prowess "Chaser" and "Draw a card." for {U}; P1 holds the instant
/// `response` for `response_cost` with the lands to pay it. Returns the table,
/// the spell, the Chaser and P1's instant.
fn prowess_against_an_instant(
    p1_lands: usize,
    response: &str,
    response_cost: ManaCost,
    chaser: (i32, i32),
    p1_instant: bool,
) -> (GameState, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    libraries(&mut s, 20);
    s.add_basic_land(P0, ManaColor::Blue);
    for _ in 0..p1_lands {
        s.add_basic_land(P1, ManaColor::Black);
    }
    let chaser = {
        let mut b = s.add_creature(P0, "Chaser", chaser.0, chaser.1);
        b.with_keyword(Keyword::Prowess);
        b.id()
    };
    let spell = s
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text(PEEK)
        .with_mana_cost(u())
        .id();
    let held = s
        .add_spell_to_hand(P1, "Held", p1_instant)
        .from_oracle_text(response)
        .with_mana_cost(response_cost)
        .id();
    let st = s.build().state().clone();
    (st, spell, chaser, held)
}

/// JR5 (the opponent's window over an own trigger): P0's 2/3 prowess with its
/// trigger on top; P1 holds a -3/-3 instant and {B}. The response with the
/// trigger on top kills the 2/3; after the trigger it survives as 0/1. The
/// on-top value SHALL NOT exceed the value after that response (red at
/// 3b773d68b: 6.533 against 1.304).
#[test]
fn jr5_opponent_response_over_own_prowess_trigger() {
    let (st, spell, chaser, gasp) =
        prowess_against_an_instant(1, GASP, cost(vec![ManaCostShard::Black], 0), (2, 3), true);
    let top = on_top(&st, spell);
    let mut r = opponent_window(&top);
    r.cast(gasp).target_object(chaser).commit();
    let resp = r.state().clone();
    let dead = !on_battlefield(&drain_stack(&resp), chaser);
    let a = searched(&top, 2);
    let b = searched(&resp, 2);
    let (c, p) = cast_and_pass(&st, spell);
    eprintln!(
        "JR5 chaser dead after response {dead}; on top {a:.3} after response {b:.3}; root cast {c:.3} pass {p:.3}"
    );
    assert!(dead, "reach-guard: response kills");
    assert!(
        a <= b + 1e-9,
        "on-top value {a} SHALL NOT exceed the value after the response {b}"
    );
}

/// Multiply by Zero over Traxos, Academy Guardian's prowess trigger: P0's
/// 1/5 flying prowess, trigger on top; P1 holds Multiply by Zero and {1}{B}.
/// Cast over the trigger, the 0/0 base dies (CR 613.4b, CR 704.5f); cast
/// after it, the prowess +1/+1 applies on top (CR 613.4c) and the 1/1 lives.
/// The on-top value SHALL NOT exceed the value after the response over the
/// trigger.
#[test]
fn multiply_by_zero_over_a_prowess_trigger_is_searched() {
    let (st, spell, traxos, zero) = prowess_against_an_instant(
        2,
        MULTIPLY_BY_ZERO,
        cost(vec![ManaCostShard::Black], 1),
        (1, 5),
        true,
    );
    let top = on_top(&st, spell);
    // Reach-guard: after the trigger resolves the same instant leaves a 1/1.
    {
        let resolved = pass_over_own_triggers(&top);
        let mut r = GameRunner::from_state(resolved);
        r.act(GameAction::PassPriority).expect("P0 passes");
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1),
            "reach-guard: P1 holds priority over P0's spell"
        );
        r.cast(zero).target_object(traxos).commit();
        let after = drain_stack(r.state());
        assert!(
            on_battlefield(&after, traxos),
            "reach-guard: after the trigger, Multiply by Zero leaves a 1/1"
        );
    }
    let mut r = opponent_window(&top);
    r.cast(zero).target_object(traxos).commit();
    let resp = r.state().clone();
    let dead = !on_battlefield(&drain_stack(&resp), traxos);
    let a = searched(&top, 2);
    let b = searched(&resp, 2);
    eprintln!("MbZ traxos dead after response {dead}; on top {a:.3} after response {b:.3}");
    assert!(dead, "reach-guard: over the trigger, the 0/0 dies");
    assert!(
        a <= b + 1e-9,
        "on-top value {a} SHALL NOT exceed the value after the response {b}"
    );
}

/// Guard (no instant-speed action): P1 has an untapped Swamp and a sorcery
/// -3/-3 in hand, so only PassPriority is issued at its priority over P0's
/// prowess trigger. The position with the trigger on top SHALL search to the
/// value of the position after it resolves, at the same depth, as at
/// 3b773d68b.
#[test]
fn own_trigger_with_no_opposing_instant_speed_action_takes_no_ply() {
    let (st, spell, _, _) =
        prowess_against_an_instant(1, GASP, cost(vec![ManaCostShard::Black], 0), (2, 3), false);
    let top = on_top(&st, spell);
    let window = opponent_window(&top);
    let issued = engine::ai_support::legal_actions(window.state());
    assert_eq!(
        issued,
        vec![GameAction::PassPriority],
        "reach-guard: P1 can only pass"
    );
    let resolved = pass_over_own_triggers(&top);
    assert_eq!(
        resolved.stack.len(),
        1,
        "reach-guard: only the spell is left"
    );
    for depth in [1, 2] {
        let a = searched(&top, depth);
        let b = searched(&resolved, depth);
        eprintln!("no-instant depth {depth}: trigger on top {a:.6}, after it resolves {b:.6}");
        assert!(
            (a - b).abs() < 1e-9,
            "depth {depth}: the own trigger SHALL take no ply ({a:.6} against {b:.6})"
        );
    }
}

/// P0 casts Mind Meanderer (4/4, {3}{G}{U}{U}) with its enters trigger aimed
/// at P1's 3/3; P1 holds Break Under Pressure and {2}{B}. Returns the position
/// with the fight trigger on top and P0 holding priority, Meanderer, the 3/3
/// and P1's instant.
fn meanderer_fight_on_top() -> (GameState, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    libraries(&mut s, 20);
    for color in [
        ManaColor::Green,
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::Blue,
    ] {
        s.add_basic_land(P0, color);
    }
    for _ in 0..3 {
        s.add_basic_land(P1, ManaColor::Black);
    }
    let rival = s.add_creature(P1, "Rival", 3, 3).id();
    let meanderer = s
        .add_creature_to_hand_from_oracle(P0, "Mind Meanderer", 4, 4, MIND_MEANDERER)
        .with_mana_cost(cost(
            vec![
                ManaCostShard::Green,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            3,
        ))
        .id();
    let pressure = s
        .add_spell_to_hand(P1, "Break Under Pressure", true)
        .from_oracle_text(BREAK_UNDER_PRESSURE)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 2))
        .id();
    let mut runner = s.build();
    runner.cast(meanderer).commit();
    let mut staged = false;
    for _ in 0..40 {
        let state = runner.state();
        match &state.waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(rival)),
                    })
                    .expect("the fight trigger targets the 3/3");
            }
            WaitingFor::Priority { player } => {
                if *player == P0
                    && state.objects[&meanderer].zone == Zone::Battlefield
                    && top_trigger_controller(state) == Some(P0)
                {
                    staged = true;
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt while staging the fight trigger: {other:?}"),
        }
    }
    assert!(staged, "reach-guard: the fight trigger is on the stack");
    (runner.state().clone(), meanderer, rival, pressure)
}

/// Break Under Pressure over Mind Meanderer's fight trigger: with the trigger
/// on top, P1 can make P0 sacrifice Meanderer (its greatest mana value) before
/// the fight, and then nothing fights (CR 701.14b) and the 3/3 lives. The
/// search SHALL NOT count the fight's kill as certain: the on-top value SHALL
/// be below the value after the trigger resolves with the kill (equal at
/// 3b773d68b). The value after the response is printed, not bounded: the
/// opponent's node adds its move-ordering prior to each line (`+ score *
/// 0.05`), here 0.025 on a response line 0.8 below the kill line.
#[test]
fn break_under_pressure_over_a_fight_trigger_is_searched() {
    let (top, meanderer, rival, pressure) = meanderer_fight_on_top();
    let killed = pass_over_own_triggers(&top);
    assert!(
        !on_battlefield(&killed, rival) && on_battlefield(&killed, meanderer),
        "reach-guard: unanswered, the fight kills the 3/3 and Meanderer lives"
    );
    let mut r = opponent_window(&top);
    r.cast(pressure).target_player(P0).commit();
    let resp = r.state().clone();
    let answered = drain_stack(&resp);
    assert!(
        !on_battlefield(&answered, meanderer) && on_battlefield(&answered, rival),
        "reach-guard: answered, Meanderer is sacrificed and the 3/3 lives"
    );
    let a = searched(&top, 2);
    let kill = searched(&killed, 2);
    let b = searched(&resp, 2);
    eprintln!("BUP on top {a:.3}; kill line {kill:.3}; after the response {b:.3}");
    assert!(
        a < kill - 1e-9,
        "the fight's kill SHALL NOT be counted as certain (on top {a:.3}, kill line {kill:.3})"
    );
}
