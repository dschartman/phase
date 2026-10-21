//! `card_value::intrinsic_value` for two ability shapes the creature ability
//! pricing (be87f9b3c) newly reached and priced wrong
//! (`fork-checks/be87f9b3c/JUDGE.md`, F1 and F2; what the price counted at
//! be87f9b3c is the step 0 table in
//! `deck-builder/sims/fra-phase/2026-10-20-trigger-shapes/NOTE.md`):
//!
//! - **F1, a symmetric refill.** "Each player may discard their hand and
//!   draw seven cards" was priced on its controller's refill alone. Each
//!   player takes it only when it gains them cards and their library holds
//!   the draws (CR 121.4 + CR 704.5b), every opponent's refill is the
//!   controller's loss, and the controller's hand is the hand at the cast,
//!   without the card itself.
//! - **F2, a trigger its controller does not decide.** "When one or more of
//!   your opponents are dealt combat damage during your turn, draw two
//!   cards. ... Fblthp's owner shuffles him into their library." was priced
//!   as two certain draws on top of the body the shuffle removes.
//!
//! The cards are real; their Oracle text is the MTGJSON text
//! (`client/public/card-data.json`), parsed through the shipped parser:
//!
//! ```text
//! Variable Chaser          {2}{U}  Creature — Human Wizard 2/3
//!   Flying, prowess
//!   This creature enters prepared.
//!   Prepare spell: Arc of Fortune {2}{U} Sorcery
//!   Each player may discard their hand and draw seven cards.
//! Fblthp, Impossibly Lost  {1}{U}  Legendary Creature — Homunculus 1/1
//!   When one or more of your opponents are dealt combat damage during your
//!   turn, draw two cards. If your library has no cards in it, you win the
//!   game. Fblthp's owner shuffles him into their library.
//! Memory Trap              {2}{W}  Enchantment
//!   When this enchantment enters, exile target nonland permanent an
//!   opponent controls until this enchantment leaves the battlefield.
//! ```
//!
//! Rows JA1 to JA6 are the judge's rows (appendix of the judge file), kept
//! as written. The other creatures are made-up shapes of the same classes.

use engine::game::effects::prepare::prepare_object;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter, TriggerConstraint};
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::card_value::{given_up_cards, intrinsic_value};

const CHASER_TEXT: &str = "Flying, prowess\nThis creature enters prepared. (While it's prepared, \
    you may cast a copy of its spell. Doing so unprepares it.)";
const ARC_TEXT: &str = "Each player may discard their hand and draw seven cards.";
const FBLTHP_TEXT: &str = "When one or more of your opponents are dealt combat damage during \
    your turn, draw two cards. If your library has no cards in it, you win the game. Fblthp's \
    owner shuffles him into their library. (If you draw from an empty library this way, you \
    still win the game.)";
const MEMORY_TRAP_TEXT: &str = "When this enchantment enters, exile target nonland permanent an \
    opponent controls until this enchantment leaves the battlefield.";

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// `count` vanilla creatures `power`/`toughness` for `{generic}` in
/// `player`'s library.
fn add_library_creatures(
    state: &mut GameState,
    player: PlayerId,
    count: usize,
    (power, toughness, generic): (i32, i32, u32),
) {
    for _ in 0..count {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            state,
            card_id,
            player,
            "Library Beast".to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.mana_cost = cost(&[], generic);
    }
}

/// `count` lands (3.0 each) in `player`'s library.
fn add_library_lands(state: &mut GameState, player: PlayerId, count: usize) {
    for _ in 0..count {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, player, "Plains".to_string(), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.mana_cost = ManaCost::NoCost;
    }
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{what}: {actual}, expected {expected}"
    );
}

// --- F1: Arc of Fortune, each player's refill --------------------------------

/// Variable Chaser (2/3 for {2}{U}: 7.5 on its body and pips) prepared on
/// P0's battlefield with Arc of Fortune as its prepare spell; P0 holds
/// `own_hand` Islands, P1 holds `opp_hand` Plains; libraries of 4/4s for {4}
/// (`own_library` for P0, eight for P1), so one expected draw is 12.0 for
/// either player.
fn chaser_board(
    own_hand: usize,
    opp_hand: usize,
    prepared_on_battlefield: bool,
    own_library: usize,
) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.add_basic_land(P0, ManaColor::Blue);
    for i in 0..own_hand {
        scenario
            .add_land_to_hand(P0, &format!("Island {i}"))
            .with_subtypes(vec!["Island"]);
    }
    for i in 0..opp_hand {
        scenario
            .add_land_to_hand(P1, &format!("Plains {i}"))
            .with_subtypes(vec!["Plains"]);
    }
    let chaser = if prepared_on_battlefield {
        let mut b = scenario.add_creature_from_oracle(P0, "Variable Chaser", 2, 3, CHASER_TEXT);
        b.with_mana_cost(cost(&[ManaCostShard::Blue], 2));
        b.id()
    } else {
        let mut b =
            scenario.add_creature_to_hand_from_oracle(P0, "Variable Chaser", 2, 3, CHASER_TEXT);
        b.with_mana_cost(cost(&[ManaCostShard::Blue], 2));
        b.id()
    };
    let mut state = scenario.build().state().clone();
    add_library_creatures(&mut state, P0, own_library, (4, 4, 4));
    add_library_creatures(&mut state, P1, 8, (4, 4, 4));
    let parsed = parse_oracle_text(
        ARC_TEXT,
        "Arc of Fortune",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        !parsed.abilities.is_empty(),
        "Arc of Fortune parses a spell ability"
    );
    state.objects.get_mut(&chaser).unwrap().back_face = Some(BackFaceData {
        name: "Arc of Fortune".to_string(),
        card_types: CardType {
            core_types: vec![CoreType::Sorcery],
            ..CardType::default()
        },
        mana_cost: cost(&[ManaCostShard::Blue], 2),
        abilities: parsed.abilities,
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });
    if prepared_on_battlefield {
        let mut events = Vec::new();
        prepare_object(&mut state, chaser, &mut events);
        assert!(state.objects[&chaser].prepared.is_some());
    }
    (state, chaser)
}

/// JA1: equal hands, 3 and 3. Each player refills four; the net is 0.
#[test]
fn ja1_arc_equal_hands_nets_nothing() {
    let (state, chaser) = chaser_board(3, 3, true, 8);
    assert_close(intrinsic_value(&state, chaser), 7.5, "Chaser, 3 v 3");
}

/// JA2 (guard): the opponent holds nine and declines (it would lose two
/// cards); P0 refills four: 7.5 + 48. (Read literally, opponent's hand minus
/// own is 6.)
#[test]
fn ja2_arc_opponent_above_seven_declines() {
    let (state, chaser) = chaser_board(3, 9, true, 8);
    assert_close(intrinsic_value(&state, chaser), 55.5, "Chaser, 3 v 9");
}

/// JA3: own hand 5, opponent's 1: P0 gains 2, P1 gains 6; net -4, so the
/// copy is not cast (CR 722.3c: it "may" be cast): the body, 7.5.
#[test]
fn ja3_arc_that_feeds_the_opponent_is_not_cast() {
    let (state, chaser) = chaser_board(5, 1, true, 8);
    assert_close(intrinsic_value(&state, chaser), 7.5, "Chaser, 5 v 1");
}

/// JA4: own hand 1, opponent's 4: P0 gains 6, P1 gains 3; net 3: 7.5 + 36.
#[test]
fn ja4_arc_nets_the_difference_not_zero() {
    let (state, chaser) = chaser_board(1, 4, true, 8);
    assert_close(intrinsic_value(&state, chaser), 43.5, "Chaser, 1 v 4");
}

/// JA5 (in hand): Chaser plus two Islands in P0's hand, three Plains in
/// P1's. Chaser must be cast before its copy can be: P0 holds 2 at the cast
/// at most, so P0 gains 5, P1 gains 4; net 1: 7.5 + 12.
#[test]
fn ja5_arc_in_hand_counts_the_hand_without_the_chaser() {
    let (state, chaser) = chaser_board(2, 3, false, 8);
    assert_close(
        intrinsic_value(&state, chaser),
        19.5,
        "Chaser in hand, 2+1 v 3",
    );
}

/// JA6: as JA4 (own hand 1, opponent's 4), but P0's library holds five:
/// drawing seven from it loses the game (CR 121.4 + CR 704.5b), so P0 does
/// not take the refill, P1 still gains 3, and the copy is not cast: 7.5.
#[test]
fn ja6_arc_that_would_draw_out_the_library_is_not_cast() {
    let (state, chaser) = chaser_board(1, 4, true, 5);
    assert_close(
        intrinsic_value(&state, chaser),
        7.5,
        "Chaser, 1 v 4, library 5",
    );
}

// --- F2: a trigger its controller does not decide alone ----------------------

/// P0's hand holds the given creature from Oracle text, a 2/2 or 1/1 shape;
/// P0 has an Island in play.
fn creature_in_hand(
    scenario: &mut GameScenario,
    name: &str,
    (power, toughness): (i32, i32),
    mana: ManaCost,
    text: &str,
) -> ObjectId {
    let mut builder = scenario.add_creature_to_hand_from_oracle(P0, name, power, toughness, text);
    builder.with_mana_cost(mana);
    builder.id()
}

fn fblthp(scenario: &mut GameScenario) -> ObjectId {
    creature_in_hand(
        scenario,
        "Fblthp, Impossibly Lost",
        (1, 1),
        cost(&[ManaCostShard::Blue], 1),
        FBLTHP_TEXT,
    )
}

/// The parse the F2 rows rest on: Fblthp's trigger is the combat damage
/// mode, and its chain moves Fblthp itself to a library.
fn assert_fblthp_parse(state: &GameState, id: ObjectId) {
    let triggers: Vec<_> = state.objects[&id]
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .collect();
    assert_eq!(triggers.len(), 1, "one trigger");
    assert_eq!(triggers[0].mode, TriggerMode::DamageReceived);
    let execute = triggers[0].execute.as_deref().expect("an execute");
    assert!(
        collect_chain_effects(execute)
            .into_iter()
            .any(|effect| matches!(
                effect,
                Effect::ChangeZone {
                    target: TargetFilter::SelfRef,
                    destination: Zone::Library,
                    ..
                }
            )),
        "the chain shuffles Fblthp into its owner's library"
    );
}

/// FB1: Fblthp in hand, library mean 12.0 (five 4/4s for {4}). Its body and
/// pips are 3.5. The trigger needs combat damage to an opponent on its
/// controller's turn, which the defender's blocks decide as much as the
/// attack, so it is priced at 0.5 of its payoff; the shuffle takes the body,
/// so the payoff is the two draws less the body: 3.5 + 0.5 * (24.0 - 3.5)
/// = 13.75. (be87f9b3c: 3.5 + 24.0 = 27.5.)
#[test]
fn fb1_fblthp_is_half_his_draws_less_the_body_he_shuffles_away() {
    let mut scenario = GameScenario::new();
    scenario.add_basic_land(P0, ManaColor::Blue);
    let fblthp = fblthp(&mut scenario);
    let mut state = scenario.build().state().clone();
    add_library_creatures(&mut state, P0, 5, (4, 4, 4));
    assert_fblthp_parse(&state, fblthp);
    assert_close(
        intrinsic_value(&state, fblthp),
        3.5 + 0.5 * (24.0 - 3.5),
        "Fblthp",
    );
}

/// FB2 (the spec's row; 1014 T12 and 1015 T14): Memory Trap and Fblthp in
/// hand at a discard, an opposing 2/3 flier for {2}{U} (7.5) on the
/// battlefield. Memory Trap exiles it: 1.5 + 7.5 = 9.0. P0's library is
/// four Plains (3.0) and four 3/3s for {3} (9.0), a mean of 6.0 (the FRA
/// pool 2 library reads lower: 17 of 40 cards are lands at 3.0). Fblthp is
/// 3.5 + 0.5 * (12.0 - 3.5) = 7.75 and is given up first. (be87f9b3c:
/// 3.5 + 12.0 = 15.5, so Memory Trap went first.) At this pricing Fblthp
/// sits under a 9.0 Memory Trap while the library mean is under 7.25; FB1's
/// 12.0 keeps him.
#[test]
fn fb2_memory_trap_with_a_target_is_kept_over_fblthp() {
    let mut scenario = GameScenario::new();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let fblthp = fblthp(&mut scenario);
    let trap = {
        let mut builder = scenario.add_spell_to_hand(P0, "Memory Trap", false);
        builder.as_enchantment();
        builder
            .from_oracle_text(MEMORY_TRAP_TEXT)
            .with_mana_cost(cost(&[ManaCostShard::White], 2))
            .id()
    };
    scenario
        .add_creature(P1, "Opposing Flier", 2, 3)
        .with_keyword(Keyword::Flying)
        .with_mana_cost(cost(&[ManaCostShard::Blue], 2));
    let mut state = scenario.build().state().clone();
    add_library_lands(&mut state, P0, 4);
    add_library_creatures(&mut state, P0, 4, (3, 3, 3));
    assert_fblthp_parse(&state, fblthp);
    assert_eq!(
        given_up_cards(&state, &[trap, fblthp], 1, None),
        vec![fblthp],
        "Fblthp is given up, Memory Trap kept"
    );
    assert_close(intrinsic_value(&state, trap), 9.0, "Memory Trap");
    assert_close(
        intrinsic_value(&state, fblthp),
        3.5 + 0.5 * (12.0 - 3.5),
        "Fblthp",
    );
}

/// A 2/2 for {1}{U} (6.0) in hand with `text`, library mean 12.0.
fn two_drop_value(text: &str) -> f64 {
    let (state, id) = two_drop(text);
    intrinsic_value(&state, id)
}

fn two_drop(text: &str) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.add_basic_land(P0, ManaColor::Blue);
    let id = creature_in_hand(
        &mut scenario,
        "Shape Probe",
        (2, 2),
        cost(&[ManaCostShard::Blue], 1),
        text,
    );
    let mut state = scenario.build().state().clone();
    add_library_creatures(&mut state, P0, 5, (4, 4, 4));
    assert!(
        !state.objects[&id].trigger_definitions.is_empty(),
        "{text}: the trigger parses"
    );
    (state, id)
}

/// The one trigger of the probe in `state`.
fn only_trigger(state: &GameState, id: ObjectId) -> &engine::types::ability::TriggerDefinition {
    let triggers: Vec<_> = state.objects[&id]
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .collect();
    assert_eq!(triggers.len(), 1, "one trigger");
    triggers[0]
}

/// FB3: an opponent's action ("whenever an opponent casts a spell"): 0.5 of
/// one draw, 6.0 + 6.0. (be87f9b3c: 18.0.)
#[test]
fn fb3_an_opponents_action_is_half_its_payoff() {
    assert_close(
        two_drop_value("Whenever an opponent casts a spell, you draw a card."),
        12.0,
        "opponent casts",
    );
}

/// FB4: its own combat damage to a player ("whenever this creature deals
/// combat damage to a player"): the defender's blocks decide it as much as
/// the attack, 6.0 + 6.0. (be87f9b3c: 18.0.)
#[test]
fn fb4_combat_damage_to_a_player_is_half_its_payoff() {
    assert_close(
        two_drop_value("Whenever this creature deals combat damage to a player, draw a card."),
        12.0,
        "combat damage",
    );
}

/// FB5: an event on the opponent's turn ("whenever you cast a spell during
/// an opponent's turn"): 6.0 + 6.0. (be87f9b3c: 18.0.)
#[test]
fn fb5_an_event_on_the_opponents_turn_is_half_its_payoff() {
    let (state, id) = two_drop("Whenever you cast a spell during an opponent's turn, draw a card.");
    assert_eq!(
        only_trigger(&state, id).constraint,
        Some(TriggerConstraint::OnlyDuringOpponentsTurn)
    );
    assert_close(intrinsic_value(&state, id), 12.0, "opponent's turn");
}

/// FG1 (guard, green at both): its own attack is its controller's decision,
/// one certain draw: 6.0 + 12.0.
#[test]
fn fg1_its_own_attack_is_certain() {
    assert_close(
        two_drop_value("Whenever this creature attacks, draw a card."),
        18.0,
        "attacks",
    );
}

/// FG2 (guard, green at both): another creature of its controller's
/// entering (Liliana's mill shape) is its controller's cast: 6.0 + 12.0.
#[test]
fn fg2_its_controllers_creature_entering_is_certain() {
    assert_close(
        two_drop_value("Whenever another creature you control enters, draw a card."),
        18.0,
        "another creature enters",
    );
}

/// FG3 (guard, green at both): its controller's own cast, 6.0 + 12.0.
#[test]
fn fg3_its_controllers_cast_is_certain() {
    assert_close(
        two_drop_value("Whenever you cast a noncreature spell, draw a card."),
        18.0,
        "you cast",
    );
}

/// FG4 (guard, green at both): a phase trigger on the opponent's turn
/// happens every turn ("at the beginning of each opponent's upkeep"): one
/// certain draw, 6.0 + 12.0.
#[test]
fn fg4_a_phase_on_the_opponents_turn_is_certain() {
    let (state, id) = two_drop("At the beginning of each opponent's upkeep, draw a card.");
    let trigger = only_trigger(&state, id);
    assert_eq!(trigger.mode, TriggerMode::Phase);
    assert_eq!(
        trigger.constraint,
        Some(TriggerConstraint::OnlyDuringOpponentsTurn)
    );
    assert_close(intrinsic_value(&state, id), 18.0, "opponent's upkeep");
}

/// FG5 (guard, green at both): a self-sacrifice with nothing priced beside
/// it ("at the beginning of the end step, sacrifice this creature", the
/// one-shot attacker class whose haste the scalar does not price) leaves the
/// body as it was: 6.0.
#[test]
fn fg5_a_bare_self_sacrifice_keeps_the_body() {
    let (state, id) = two_drop("At the beginning of the end step, sacrifice this creature.");
    let execute = only_trigger(&state, id)
        .execute
        .as_deref()
        .expect("an execute");
    assert!(
        collect_chain_effects(execute)
            .into_iter()
            .any(|effect| matches!(
                effect,
                Effect::Sacrifice {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )),
        "the trigger sacrifices its source"
    );
    assert_close(intrinsic_value(&state, id), 6.0, "end step sacrifice");
}
