//! Leaf credit for a cast noncreature permanent and for a spell still on the
//! stack (`simforge/docs/spec-ai-eval-permanent-credit-2026-10-19.md`).
//!
//! The search scores a cast at its leaves. Before this credit, a card in hand
//! was worth the hand-size weight (0.75 for an Aggro deck late) and the same
//! card resolved as a noncreature permanent only the `card_advantage` weight
//! (0.1945), with planeswalker loyalty never read, so casting The Theorist or
//! Infinite Coursework lost about 0.55 at every leaf; a spell still on the
//! stack at a rollout leaf was a card lost for nothing. These rows pin the
//! leaf values that the casts' search scores are made of.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::PlannerServices;
use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::deck_profile::{DeckArchetype, DeckProfile};
use crate::eval::{evaluate_state, EvalWeightSet, EvalWeights};

const THE_THEORIST_ORACLE: &str =
    "At the beginning of each opponent's draw step, you draw a card.\n\
     [+1]: Create a 1/1 blue Illusion creature token.\n\
     [\u{2212}2]: For each opponent, return up to one target artifact or creature that player \
     controls to its owner's hand.\n\
     [\u{2212}6]: Draw three cards. Then put X +1/+1 counters on each creature you control, \
     where X is the number of cards in your hand.";

const INFINITE_COURSEWORK_ORACLE: &str =
    "Enchant creature\nWhen this Aura enters, tap enchanted creature. It becomes \
     unprepared.\nEnchanted creature loses all abilities and doesn't untap during its \
     controller's untap step.";

const PACIFISM_ORACLE: &str = "Enchant creature\nEnchanted creature can't attack or block.";

const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

/// The turn every row is evaluated on: the late weight set, where the FRA
/// holds were (T12 to T31).
const LATE_TURN: u32 = 12;

const ARCHETYPES: [DeckArchetype; 5] = [
    DeckArchetype::Aggro,
    DeckArchetype::Midrange,
    DeckArchetype::Control,
    DeckArchetype::Combo,
    DeckArchetype::Ramp,
];

/// Serve-time weights for `archetype` at `turn`: the learned set scaled by the
/// archetype's multipliers, as `AiContext::analyze_for_player` builds them.
fn served_weights(archetype: DeckArchetype, turn: u32) -> EvalWeights {
    let profile = DeckProfile {
        archetype,
        ..DeckProfile::default()
    };
    profile.adjust_weights(EvalWeightSet::learned().for_turn(turn))
}

/// P0 (the AI) on its own precombat main: four Islands and three Plains,
/// `library` Islands in library, an empty hand. P1: two Plains, 25 Plains in
/// library.
fn table(library: usize) -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for color in [
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::White,
        ManaColor::White,
        ManaColor::White,
    ] {
        scenario.add_basic_land(P0, color);
    }
    for _ in 0..library {
        scenario.add_land_to_library_top(P0, "Island");
    }
    for _ in 0..25 {
        scenario.add_land_to_library_top(P1, "Plains");
    }
    scenario.add_basic_land(P1, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::White);
    scenario
}

fn the_theorist_in_hand(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_spell_to_hand(P0, "The Theorist, Jace Beleren", false);
    builder
        .from_oracle_text(THE_THEORIST_ORACLE)
        .as_planeswalker_with_loyalty("Jace", 3)
        .as_legendary()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        })
        .id()
}

fn aura_in_hand(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    let mut builder = scenario.add_spell_to_hand(P0, name, false);
    builder.as_enchantment();
    builder.with_subtypes(vec!["Aura"]);
    builder
        .from_oracle_text_with_keywords(&["Enchant"], oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id()
}

fn at_turn(mut state: GameState, turn: u32) -> GameState {
    state.turn_number = turn;
    state
}

/// Cast `spell` through the real pipeline (CR 601.2a-i) and resolve it and
/// anything it puts on the stack (CR 608.3: a permanent spell enters the
/// battlefield).
fn cast_and_resolve(scenario: GameScenario, spell: ObjectId, targets: &[ObjectId]) -> GameState {
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(targets).resolve();
    runner.state().clone()
}

/// The Theorist resolved and then at `loyalty` (CR 306.5c: a planeswalker's
/// loyalty is the number of loyalty counters on it).
fn the_theorist_resolved_at(library: usize, loyalty: u32) -> (GameState, ObjectId) {
    let mut scenario = table(library);
    let theorist = the_theorist_in_hand(&mut scenario);
    let mut state = cast_and_resolve(scenario, theorist, &[]);
    let obj = state
        .objects
        .get_mut(&theorist)
        .expect("The Theorist exists");
    assert_eq!(obj.zone, Zone::Battlefield, "The Theorist resolved");
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.loyalty = Some(loyalty);
    (at_turn(state, LATE_TURN), theorist)
}

fn the_theorist_held(library: usize) -> GameState {
    let mut scenario = table(library);
    the_theorist_in_hand(&mut scenario);
    at_turn(scenario.build().state().clone(), LATE_TURN)
}

/// A small creature deck: `AiContext::analyze_for_player` classifies it as a
/// pure Aggro deck, the FRA decks' archetype (hand 0.75, card_advantage 0.1945
/// late).
fn aggro_context(config: &AiConfig) -> AiContext {
    use engine::game::DeckEntry;
    let deck: Vec<DeckEntry> = (0..8)
        .map(|i| DeckEntry {
            card: CardFace {
                name: format!("Goblin {i}"),
                card_type: CardType {
                    core_types: vec![CoreType::Creature],
                    subtypes: vec!["Goblin".to_string()],
                    ..Default::default()
                },
                ..Default::default()
            },
            count: 4,
        })
        .collect();
    let context =
        AiContext::analyze_for_player(&deck, &config.weights, &config.archetype_multipliers, P0);
    assert_eq!(context.deck_profile.archetype, DeckArchetype::Aggro);
    context
}

fn with_services<R>(f: impl FnOnce(&mut PlannerServices<'_>) -> R) -> R {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let policies = crate::policies::PolicyRegistry::shared();
    let context = aggro_context(&config);
    let mut services = PlannerServices::new(P0, &config, policies, context);
    f(&mut services)
}

// --- The Theorist: a resolved planeswalker is worth at least its card --------

/// CR 306.5b + CR 306.5c + CR 606.3: on the battlefield The Theorist is her
/// loyalty and the loyalty abilities it can pay for; in hand she is one card.
/// At 4 loyalty the battlefield state must score at least the hand state for
/// her controller, at every archetype's served weights.
#[test]
fn the_theorist_at_four_loyalty_is_worth_at_least_her_card_in_hand() {
    let held = the_theorist_held(25);
    let (resolved, _) = the_theorist_resolved_at(25, 4);
    for archetype in ARCHETYPES {
        let weights = served_weights(archetype, LATE_TURN);
        let hand = evaluate_state(&held, P0, &weights);
        let battlefield = evaluate_state(&resolved, P0, &weights);
        assert!(
            battlefield >= hand,
            "{archetype:?}: The Theorist at 4 loyalty on the battlefield ({battlefield:.4}) \
             scores below her card in hand ({hand:.4})"
        );
    }
}

/// The same at both planner leaves for an Aggro deck: the rollout leaf
/// (`quiesced_leaf_eval`) and the strategy leaf (`evaluate_with_strategy`,
/// which adds zone quality and the card-advantage differential).
#[test]
fn the_theorist_at_four_loyalty_outscores_her_card_at_both_planner_leaves() {
    let held = the_theorist_held(25);
    let (resolved, _) = the_theorist_resolved_at(25, 4);
    with_services(|services| {
        let rollout_hand = services.quiesced_leaf_eval(&held);
        let rollout_battlefield = services.quiesced_leaf_eval(&resolved);
        assert!(
            rollout_battlefield >= rollout_hand,
            "rollout leaf: battlefield {rollout_battlefield:.4} < hand {rollout_hand:.4}"
        );
        let strategy_hand = services.evaluate_with_strategy(&held);
        let strategy_battlefield = services.evaluate_with_strategy(&resolved);
        assert!(
            strategy_battlefield >= strategy_hand,
            "strategy leaf: battlefield {strategy_battlefield:.4} < hand {strategy_hand:.4}"
        );
    });
}

/// CR 306.5c + CR 606.6: her credit reads her loyalty, so a planeswalker at 5
/// is worth more than the same one at 2 (where her [-6] and more of her
/// loyalty are gone).
#[test]
fn a_planeswalkers_leaf_credit_rises_with_its_loyalty() {
    let (low, _) = the_theorist_resolved_at(25, 2);
    let (high, _) = the_theorist_resolved_at(25, 5);
    let weights = served_weights(DeckArchetype::Aggro, LATE_TURN);
    assert!(
        evaluate_state(&high, P0, &weights) > evaluate_state(&low, P0, &weights),
        "loyalty 5 must outscore loyalty 2"
    );
}

// --- The Theorist: loyalty is the budget that pays for one line ------------

/// The two readings of a resolved Theorist at `loyalty` (25 Islands in
/// library, so one expected draw is an Island's 3.0): her leaf credit in
/// cards (`permanent_card_credit`) and her `intrinsic_value`.
fn the_theorist_prices_at(loyalty: u32) -> (f64, f64) {
    let (state, theorist) = the_theorist_resolved_at(25, loyalty);
    let obj = &state.objects[&theorist];
    (
        crate::card_value::permanent_card_credit(&state, obj),
        crate::card_value::intrinsic_value(&state, theorist),
    )
}

/// CR 606.3: a planeswalker's loyalty abilities are activated one a turn, and
/// CR 606.4 + CR 606.6: a negative cost is paid out of its loyalty. Her loyalty is what
/// pays for the line she takes, not value on top of it. At 1 loyalty only her
/// [+1] (a 1/1 Illusion, 2.5) is payable: her leaf credit is her card, the
/// larger of her loyalty (1) and that line, and her recurring draw once
/// (3.0), so 1 + (2.5 + 3.0) / 3.0 = 2.83 cards, under two cards above her
/// card (it was 3.17: her loyalty added on top of the line it pays for).
#[test]
fn the_theorists_leaf_credit_at_one_loyalty_is_her_card_plus_her_one_payable_line() {
    let (credit, _) = the_theorist_prices_at(1);
    assert!(
        credit - 1.0 < 2.0,
        "at 1 loyalty her leaf credit must sit under two cards above her card, got {credit:.4}"
    );
    assert!(
        (credit - (1.0 + 5.5 / 3.0)).abs() < 1e-9,
        "card + max(loyalty 1, [+1] 2.5) + draw 3.0, in draws; got {credit:.4}"
    );
}

/// The same rule in `intrinsic_value`: her pips (2.0) plus the larger of her
/// loyalty (1) and her [+1] (2.5), plus her draw (3.0): 7.5 (it was 8.5).
#[test]
fn the_theorists_intrinsic_value_at_one_loyalty_is_her_one_payable_line() {
    let (_, intrinsic) = the_theorist_prices_at(1);
    assert!(
        (intrinsic - 7.5).abs() < 1e-9,
        "pips 2.0 + max(loyalty 1, [+1] 2.5) + draw 3.0; got {intrinsic:.4}"
    );
}

/// At 6 loyalty every ability is payable ([+1] 2.5, [-2] 0 on an empty
/// opposing board, [-6] draw three 9.0), but she activates one a turn
/// (CR 606.3): her best line, the [-6], against her loyalty 6, so
/// 1 + (9.0 + 3.0) / 3.0 = 5.0 cards, not every payable ability summed on top
/// of her loyalty (7.83).
#[test]
fn the_theorists_leaf_credit_at_six_loyalty_is_her_best_line_not_the_sum() {
    let (credit, _) = the_theorist_prices_at(6);
    assert!(
        (credit - 5.0).abs() < 1e-9,
        "card + max(loyalty 6, [-6] 9.0) + draw 3.0, in draws; got {credit:.4}"
    );
}

/// `intrinsic_value` at 6 loyalty: 2.0 + max(6, 9.0) + 3.0 = 14.0, not the
/// sum's 22.5.
#[test]
fn the_theorists_intrinsic_value_at_six_loyalty_is_her_best_line_not_the_sum() {
    let (_, intrinsic) = the_theorist_prices_at(6);
    assert!(
        (intrinsic - 14.0).abs() < 1e-9,
        "pips 2.0 + max(loyalty 6, [-6] 9.0) + draw 3.0; got {intrinsic:.4}"
    );
}

// --- The Theorist's draw on a short library -------------------------------

/// The Theorist with her recurring draw trigger removed: the credit of
/// everything but the draw.
fn without_triggers(mut state: GameState, id: ObjectId) -> GameState {
    let obj = state.objects.get_mut(&id).expect("object exists");
    obj.trigger_definitions = Vec::<engine::types::TriggerDefinition>::new().into();
    state
}

/// CR 121.4 + CR 704.5b: her draw is mandatory. With 25 cards (12.5 rounds of
/// her draw plus the AI's own) the draw is credited; with 11 cards
/// (`self_draw_exposure` reads 5.5 rounds, under six) it is credited at zero,
/// so a higher permanent credit does not make a short-library cast attractive
/// (seed 1018 T23 cast her at 11 and decked at T31).
#[test]
fn the_theorists_draw_earns_leaf_credit_only_off_a_short_library() {
    let weights = served_weights(DeckArchetype::Aggro, LATE_TURN);
    let draw_credit = |library: usize| {
        let (state, theorist) = the_theorist_resolved_at(library, 4);
        let without = without_triggers(state.clone(), theorist);
        evaluate_state(&state, P0, &weights) - evaluate_state(&without, P0, &weights)
    };
    let long = draw_credit(25);
    let short = draw_credit(11);
    assert!(
        long > 0.0,
        "at 25 cards her recurring draw must earn leaf credit, got {long:.4}"
    );
    assert!(
        short.abs() < 1e-9,
        "at 11 cards (under six rounds) her draw must earn no leaf credit, got {short:.4}"
    );
}

// --- Auras: the host's combat value -----------------------------------------

/// Leaf values for P0 of an Aura in hand against an opposing 3/3, the same
/// Aura cast onto that 3/3 and resolved, and the hand state without the 3/3.
fn aura_states(name: &str, oracle: &str) -> (GameState, GameState, GameState) {
    let mut held = table(25);
    held.add_vanilla(P1, 3, 3);
    aura_in_hand(&mut held, name, oracle);
    let held = at_turn(held.build().state().clone(), LATE_TURN);

    let mut cast = table(25);
    let host = cast.add_vanilla(P1, 3, 3);
    let aura = aura_in_hand(&mut cast, name, oracle);
    let resolved = cast_and_resolve(cast, aura, &[host]);
    let attached = &resolved.objects[&aura];
    assert_eq!(attached.zone, Zone::Battlefield, "{name} resolved");

    let mut empty = table(25);
    aura_in_hand(&mut empty, name, oracle);
    let empty = at_turn(empty.build().state().clone(), LATE_TURN);

    (held, at_turn(resolved, LATE_TURN), empty)
}

/// CR 303.4b + CR 502.3 + CR 508.1a + CR 509.1a: Infinite Coursework taps the
/// creature it enchants and keeps it from untapping, so that creature can no
/// longer attack or block. Attached, it must score above the Aura in hand by
/// at least the creature's combat value (what the board loses without it).
#[test]
fn infinite_coursework_on_an_opposing_creature_gains_at_least_the_creatures_combat_value() {
    let (held, attached, empty) = aura_states("Infinite Coursework", INFINITE_COURSEWORK_ORACLE);
    let host = attached
        .battlefield
        .iter()
        .find_map(|id| {
            let obj = &attached.objects[id];
            (obj.controller == P1 && obj.card_types.core_types.contains(&CoreType::Creature))
                .then_some(obj)
        })
        .expect("the 3/3 is still there");
    assert!(host.tapped, "the ETB tapped the enchanted creature");
    for archetype in ARCHETYPES {
        let weights = served_weights(archetype, LATE_TURN);
        let hand = evaluate_state(&held, P0, &weights);
        let creature_value = evaluate_state(&empty, P0, &weights) - hand;
        let gain = evaluate_state(&attached, P0, &weights) - hand;
        assert!(
            creature_value > 0.0,
            "{archetype:?}: the 3/3 is worth something"
        );
        assert!(
            gain >= creature_value,
            "{archetype:?}: Coursework attached gains {gain:.4} over the hand state, under the \
             3/3's combat value {creature_value:.4}"
        );
    }
}

/// The class, not the card: an Aura whose host can't attack or block
/// (Pacifism) earns the same host credit.
#[test]
fn an_aura_whose_host_cant_attack_or_block_gains_the_hosts_combat_value() {
    let (held, attached, empty) = aura_states("Pacifism", PACIFISM_ORACLE);
    let weights = served_weights(DeckArchetype::Aggro, LATE_TURN);
    let hand = evaluate_state(&held, P0, &weights);
    let creature_value = evaluate_state(&empty, P0, &weights) - hand;
    let gain = evaluate_state(&attached, P0, &weights) - hand;
    assert!(
        gain >= creature_value,
        "Pacifism attached gains {gain:.4}, under the 3/3's combat value {creature_value:.4}"
    );
}

/// Guard: Coursework's "doesn't untap" holds a creature only while it is
/// tapped. A host that untapped anyway (another effect) still fights, so the
/// Aura earns its card and no host credit.
#[test]
fn an_untapped_host_under_a_doesnt_untap_aura_keeps_its_combat_value() {
    let (held, mut attached, empty) =
        aura_states("Infinite Coursework", INFINITE_COURSEWORK_ORACLE);
    let host = attached
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let obj = &attached.objects[id];
            obj.controller == P1 && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .expect("the 3/3 is still there");
    attached.objects.get_mut(&host).unwrap().tapped = false;
    let weights = served_weights(DeckArchetype::Aggro, LATE_TURN);
    let hand = evaluate_state(&held, P0, &weights);
    let creature_value = evaluate_state(&empty, P0, &weights) - hand;
    let gain = evaluate_state(&attached, P0, &weights) - hand;
    assert!(
        gain < creature_value,
        "an untapped host still fights: gain {gain:.4} must stay under its combat value \
         {creature_value:.4}"
    );
}

// --- Guards: creatures and tokens are priced as before ---------------------

/// Creature evaluation is unchanged: a resolved 2/2 creature spell moves the
/// leaf by exactly its board terms less the card it was in hand (equal life,
/// so no aggression term).
#[test]
fn a_resolved_creature_spell_is_priced_by_its_board_terms_as_before() {
    let bear_in_hand = |scenario: &mut GameScenario| {
        scenario
            .add_creature_to_hand(P0, "Bear", 2, 2)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .id()
    };
    let mut held = table(25);
    bear_in_hand(&mut held);
    let held = at_turn(held.build().state().clone(), LATE_TURN);
    let mut cast = table(25);
    let cast_bear = bear_in_hand(&mut cast);
    let resolved = at_turn(cast_and_resolve(cast, cast_bear, &[]), LATE_TURN);
    assert_eq!(resolved.objects[&cast_bear].zone, Zone::Battlefield);
    for archetype in ARCHETYPES {
        let w = served_weights(archetype, LATE_TURN);
        let delta = evaluate_state(&resolved, P0, &w) - evaluate_state(&held, P0, &w);
        let expected =
            w.board_presence + 2.0 * w.board_power + 2.0 * w.board_toughness - w.hand_size;
        assert!(
            (delta - expected).abs() < 1e-9,
            "{archetype:?}: a resolved 2/2 moved the leaf by {delta:.6}, expected {expected:.6}"
        );
    }
}

/// CR 111.1: a token is not represented by a card, so a noncreature token
/// (a Treasure, a Clue) earns no card credit: it stays at the
/// `card_advantage` weight alone.
#[test]
fn a_noncreature_token_earns_no_card_credit() {
    let base = at_turn(table(25).build().state().clone(), LATE_TURN);
    let mut with_token = base.clone();
    let card_id = CardId(with_token.next_object_id);
    let id = create_object(
        &mut with_token,
        card_id,
        P0,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    let token = with_token.objects.get_mut(&id).unwrap();
    token.card_types.core_types.push(CoreType::Artifact);
    token.is_token = true;
    for archetype in ARCHETYPES {
        let w = served_weights(archetype, LATE_TURN);
        let delta = evaluate_state(&with_token, P0, &w) - evaluate_state(&base, P0, &w);
        assert!(
            (delta - w.card_advantage).abs() < 1e-9,
            "{archetype:?}: a Treasure token moved the leaf by {delta:.6}, expected the \
             card_advantage weight {:.6}",
            w.card_advantage
        );
    }
}

// --- A spell on the stack at a rollout leaf -------------------------------

/// Commit `spell` to the stack and pass P0's priority (CR 117.3d), so P1
/// holds priority with the spell unresolved (CR 405.1).
fn on_the_stack_after_pass(scenario: GameScenario, spell: ObjectId) -> GameState {
    let mut runner: GameRunner = scenario.build();
    {
        let mut commit = runner.cast(spell).commit();
        commit
            .act(GameAction::PassPriority)
            .expect("P0 passes priority with the spell on the stack");
    }
    let state = runner.state().clone();
    assert_eq!(state.stack.len(), 1, "the spell is on the stack");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority, got {:?}",
        state.waiting_for
    );
    at_turn(state, LATE_TURN)
}

/// CR 117.4 + CR 608.3: when no opponent can respond, the spell resolves at
/// the leaf: the rollout leaf with The Theorist on the stack equals the leaf
/// with her resolved.
#[test]
fn a_spell_no_one_can_answer_is_valued_as_resolved_at_the_rollout_leaf() {
    let mut on_stack = table(25);
    let theorist = the_theorist_in_hand(&mut on_stack);
    let stacked = on_the_stack_after_pass(on_stack, theorist);

    let mut cast = table(25);
    let resolved_id = the_theorist_in_hand(&mut cast);
    let resolved = at_turn(cast_and_resolve(cast, resolved_id, &[]), LATE_TURN);

    with_services(|services| {
        let stack_leaf = services.quiesced_leaf_eval(&stacked);
        let resolved_leaf = services.quiesced_leaf_eval(&resolved);
        assert!(
            (stack_leaf - resolved_leaf).abs() < 1e-9,
            "stack leaf {stack_leaf:.6} vs resolved leaf {resolved_leaf:.6}"
        );
    });
}

/// P1 holds Shock with a Mountain untapped, so the rollout leaf stops with
/// The Theorist on the stack (P1's priority is a real decision). The spell is
/// not a card lost: the leaf must be at least the value of holding her
/// (her hand value times a resolve chance of at most one).
#[test]
fn a_spell_facing_an_open_instant_is_not_scored_as_a_card_lost() {
    let open_instant = |scenario: &mut GameScenario| {
        scenario.add_basic_land(P1, ManaColor::Red);
        let mut shock = scenario.add_spell_to_hand(P1, "Shock", true);
        shock
            .from_oracle_text(SHOCK_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            });
    };
    let mut on_stack = table(25);
    open_instant(&mut on_stack);
    let theorist = the_theorist_in_hand(&mut on_stack);
    let stacked = on_the_stack_after_pass(on_stack, theorist);

    let mut held = table(25);
    open_instant(&mut held);
    the_theorist_in_hand(&mut held);
    let held = at_turn(held.build().state().clone(), LATE_TURN);

    with_services(|services| {
        let stack_leaf = services.quiesced_leaf_eval(&stacked);
        assert_eq!(
            services.quiesce(&stacked).stack.len(),
            1,
            "P1's open Shock stops quiescence with the spell on the stack"
        );
        let hand_leaf = services.quiesced_leaf_eval(&held);
        assert!(
            stack_leaf >= hand_leaf,
            "The Theorist on the stack ({stack_leaf:.4}) scores below holding her ({hand_leaf:.4})"
        );
    });
}

/// The stack credit is the spell's controller's: an opposing spell on the
/// stack counts against the AI as that opponent's card, not for it.
#[test]
fn an_opposing_spell_on_the_stack_counts_for_its_controller() {
    let mut scenario = table(25);
    let theorist = the_theorist_in_hand(&mut scenario);
    let stacked = on_the_stack_after_pass(scenario, theorist);
    let weights = served_weights(DeckArchetype::Aggro, LATE_TURN);
    let p1 = evaluate_state(&stacked, P1, &weights);
    let mut gone = stacked.clone();
    gone.stack.clear();
    let p1_gone = evaluate_state(&gone, P1, &weights);
    assert!(
        p1 < p1_gone,
        "P0's spell on the stack must count against P1 ({p1:.4} vs {p1_gone:.4} without it)"
    );
}
