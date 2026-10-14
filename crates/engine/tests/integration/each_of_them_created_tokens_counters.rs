//! "Put ... counters on each of them" after a token-creating clause places the
//! counters on the tokens that clause created when the whole ability is exactly
//! that creation and that placement.
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
//! Verbatim Oracle text (MTGJSON `AtomicCards.json`, as in the card data): Grist,
//! the Plague Swarm (T9), the back face's loyalty abilities.
//!
//! Synthetic text, disclosed: T4 ("Create ... token. Tap up to two target
//! creatures. Put a stun counter on each of them.") and T5a'/T5b ("You may pay
//! {2}. If you do, create two ... tokens named Cadet. Put a +1/+1 counter on each
//! of them.") have no printed witness; T5 is templated on the "you may pay {N}.
//! If you do, create ..." pattern used in `reflexive_body_token_referent.rs`.
//! T7 and T12 are the maintainer's composition ("Create ... Soldier creature
//! token(s). Exile the top two cards of your library. Put a study counter on
//! each of them."), templated on Imbraham, Dean of Theory's "Exile the top X
//! cards of your library and put a study counter on each of them." T13-T25 are
//! synthetic two-sentence shapes and their variations ("Create two 1/1 white
//! Soldier creature tokens. Put a +1/+1 counter on each of them.", with an
//! instruction before, between or after, a condition, an option, one token, a
//! variable count, a trigger, an activation instruction, a modal mode, or a
//! continuation after the anaphor); no printed card has any of them.
//! Staging fixtures are also synthetic: T2 stages a token from an earlier
//! resolution with "Create a 1/1 white Soldier creature token.", and T3 stages
//! life gained this turn with "You gain 3 life.". The card under test in each of
//! those rows is verbatim.
//!
//! Every other shape keeps the base reading (D12); those rows pin base parity,
//! not a CR 608.2c derivation. T7 and T12 are derived: the exiled cards are the
//! nearest plural antecedent (CR 608.2c), and base delivers them.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
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

/// T5a' (synthetic). A payment instruction before the creation is a third
/// sentence, so "each of them" keeps the base reading (D12; the CR 608.2c
/// reading is not asserted): the paid gate creates two Cadets and neither they
/// nor the Soldier left in the created-token ledger get a counter.
#[test]
fn gated_creation_keeps_each_of_them_base_reading() {
    let (mut runner, gated, soldier) = gated_scenario();

    let outcome = runner.cast(gated).accept_optional().resolve();

    let cadets = tokens_named(&runner, "Cadet");
    assert_eq!(cadets.len(), 2, "the paid gate creates two Cadets");
    assert_eq!(outcome.mana_pool_total(P0), 0, "the {{2}} was paid");
    for cadet in cadets {
        assert_eq!(total_counters(&runner, cadet), 0);
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
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![soldier],
        "the ledger still holds the Soldier: the declined gate created nothing"
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
                rider_gap |= def
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

/// Synthetic, the maintainer's composition: one token, then an exile of the top
/// two cards, then the plural anaphor.
const SOLDIER_THEN_EXILE: &str = "Create a 1/1 white Soldier creature token. Exile the top two cards of your library. Put a study counter on each of them.";
/// Synthetic: the two-token variant of the maintainer's composition.
const SOLDIERS_THEN_EXILE: &str = "Create two 1/1 white Soldier creature tokens. Exile the top two cards of your library. Put a study counter on each of them.";
/// Verbatim back face of Grist, Voracious Larva.
const GRIST: &str = "[+1]: Create a 1/1 black and green Insect creature token, then mill two cards. Put a deathtouch counter on the token if a black card was milled this way.\n[\u{2212}2]: Destroy target artifact or enchantment.\n[\u{2212}6]: For each creature card in your graveyard, create a token that's a copy of it, except it's a 1/1 black and green Insect.";
/// Synthetic: an instruction before the creation.
const GAIN_THEN_CREATE: &str = "You gain 2 life. Create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them.";
/// Synthetic: an instruction between the creation and the rider.
const CREATE_TAP_THEN_PUT: &str =
    "Create two 1/1 white Soldier creature tokens. Tap them. Put a +1/+1 counter on each of them.";
/// Synthetic: an instruction after the rider.
const CREATE_PUT_THEN_GAIN: &str = "Create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them. You gain 2 life.";
/// Synthetic: a condition on the rider.
const RIDER_CONDITION: &str = "Create two 1/1 white Soldier creature tokens. If an opponent controls two or more creatures, put a -1/-1 counter on each of them.";
/// Synthetic: an untyped head on the creation, which the parser drops.
const UNTYPED_HEAD: &str = "If two or more creatures an opponent controls are tied for greatest power, create two 1/1 white Soldier creature tokens. Put a -1/-1 counter on each of them.";
/// Synthetic: an optional creation.
const OPTIONAL_CREATION: &str =
    "You may create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them.";
/// Synthetic: one token.
const ONE_TOKEN: &str =
    "Create a 1/1 white Soldier creature token. Put a +1/+1 counter on each of them.";
/// Synthetic: a triggered ability with the two-sentence shape.
const TRIGGERED_SHAPE: &str = "When this creature enters, create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them.";
/// Synthetic: an activated ability with the two-sentence shape.
const ACTIVATED_SHAPE: &str =
    "{T}: Create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them.";
/// Synthetic: a continuation whose trailing guard the token parser drops.
const SWALLOWED_CONTINUATION_GUARD: &str = "Create two 1/1 white Soldier creature tokens. Put a -1/-1 counter on each of them and create a Treasure token if two or more creatures an opponent controls are tied for greatest power.";
/// Synthetic: continuations carrying a condition the parser does not represent.
const CONTINUATION_AS_LONG_AS: &str = "Create two 1/1 white Soldier creature tokens. Put a -1/-1 counter on each of them and create a Treasure token as long as an opponent controls two or more creatures.";
const CONTINUATION_WHILE: &str = "Create two 1/1 white Soldier creature tokens. Put a -1/-1 counter on each of them and create a Treasure token while an opponent controls two or more creatures.";
const CONTINUATION_INSTEAD: &str = "Create two 1/1 white Soldier creature tokens. Put a -1/-1 counter on each of them and draw a card instead.";
/// Synthetic: an activation instruction sentence after the rider.
const ACTIVATION_INSTRUCTION: &str = "{T}: Create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them. Activate only as a sorcery.";
/// Synthetic: a variable token count.
const VARIABLE_COUNT: &str = "Create X 1/1 white Soldier creature tokens, where X is the number of creatures you control. Put a +1/+1 counter on each of them.";
/// Synthetic: the two-sentence shape as a mode of a modal spell.
const MODAL_MODE: &str = "Choose one \u{2014}\n\u{2022} Create two 1/1 white Soldier creature tokens. Put a +1/+1 counter on each of them.\n\u{2022} Draw a card.";

/// A spell `text` in P0's hand, cast for free in the precombat main phase.
fn free_spell(scenario: &mut GameScenario, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Shape Under Test", false, text)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id()
}

/// Library A, B, C with C on top (each added card goes on top).
fn stage_library(scenario: &mut GameScenario) -> [ObjectId; 3] {
    let a = scenario.add_card_to_library_top(P0, "Library A");
    let b = scenario.add_card_to_library_top(P0, "Library B");
    let c = scenario.add_card_to_library_top(P0, "Library C");
    [a, b, c]
}

/// OPP2: P1 controls two 2/2 creatures.
fn stage_two_opposing_creatures(scenario: &mut GameScenario) -> [ObjectId; 2] {
    [
        scenario.add_creature(P1, "Opp A", 2, 2).id(),
        scenario.add_creature(P1, "Opp B", 2, 2).id(),
    ]
}

/// The two Soldiers are alive and carry no counter.
fn assert_two_soldiers_without_counters(runner: &GameRunner, text: &str) {
    let soldiers = tokens_named(runner, "Soldier");
    assert_eq!(soldiers.len(), 2, "{text:?}: two Soldiers exist");
    for soldier in soldiers {
        assert_eq!(
            total_counters(runner, soldier),
            0,
            "{text:?}: base reading, no counter on the Soldiers"
        );
    }
}

fn assert_exiled_cards_get_study_counters(runner: &GameRunner, library: [ObjectId; 3]) {
    let [a, b, c] = library;
    let study = CounterType::Generic("study".to_string());
    for card in [b, c] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert_eq!(
            counter(runner, card, &study),
            1,
            "each exiled card gets one"
        );
    }
    assert_eq!(runner.state().objects[&a].zone, Zone::Library);
    assert_eq!(total_counters(runner, a), 0);
}

/// T7. The maintainer's witness (CR 608.2c, CR 122.1): the exiled cards are the
/// nearest plural antecedent, so each gets a study counter and the Soldier gets
/// none.
#[test]
fn exiled_cards_after_a_created_token_get_each_of_them_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let library = stage_library(&mut scenario);
    let spell = free_spell(&mut scenario, SOLDIER_THEN_EXILE);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let soldiers = tokens_named(&runner, "Soldier");
    assert_eq!(soldiers.len(), 1, "the Soldier is created");
    assert_eq!(total_counters(&runner, soldiers[0]), 0);
    assert_exiled_cards_get_study_counters(&runner, library);
}

/// T12. The two-token variant: still the exiled cards, not the Soldiers.
#[test]
fn exiled_cards_after_created_tokens_get_each_of_them_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let library = stage_library(&mut scenario);
    let spell = free_spell(&mut scenario, SOLDIERS_THEN_EXILE);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_two_soldiers_without_counters(&runner, SOLDIERS_THEN_EXILE);
    assert_exiled_cards_get_study_counters(&runner, library);
}

/// T9 (guard). Other anaphor forms are unchanged: Grist's "+1" puts the
/// deathtouch counter on the Insect it created, after the intervening mill.
#[test]
fn grist_the_plague_swarm_deathtouch_counter_stays_on_the_insect() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let black = scenario
        .add_spell_to_library_top(P0, "Black Card", false)
        .with_color(vec![ManaColor::Black])
        .id();
    let other = scenario
        .add_spell_to_library_top(P0, "Other Card", false)
        .id();
    let grist = scenario
        .add_planeswalker_from_oracle(P0, "Grist, the Plague Swarm", "Grist", 3, GRIST)
        .id();
    let mut runner = scenario.build();
    let plus_one = runner.state().objects[&grist]
        .abilities
        .iter()
        .position(|ability: &AbilityDefinition| {
            matches!(ability.cost, Some(AbilityCost::Loyalty { amount: 1 }))
        })
        .expect("Grist exposes its +1 loyalty ability");

    runner.activate(grist, plus_one).resolve();

    let insects = tokens_named(&runner, "Insect");
    assert_eq!(insects.len(), 1, "one Insect is created");
    assert_eq!(
        counter(
            &runner,
            insects[0],
            &CounterType::Keyword(KeywordKind::Deathtouch)
        ),
        1,
        "the Insect gets the deathtouch counter"
    );
    for card in [black, other] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
        assert_eq!(total_counters(&runner, card), 0);
    }
    assert_eq!(runner.state().objects[&grist].loyalty, Some(4));
}

/// T13 (synthetic). An extra instruction before, between or after keeps the
/// base reading (D12; the CR 608.2c reading is not asserted). Rows 1 and 3 gain
/// the life; row 2's "Tap them" leaves the Soldiers untapped, as at base (a
/// pre-existing gap of the bare "them" in a tap instruction, not this
/// change), so its reach guard is that the spell resolved and made the
/// Soldiers.
#[test]
fn an_extra_instruction_keeps_each_of_them_base_reading() {
    for (text, life_gained) in [
        (GAIN_THEN_CREATE, 2),
        (CREATE_TAP_THEN_PUT, 0),
        (CREATE_PUT_THEN_GAIN, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = free_spell(&mut scenario, text);
        let mut runner = scenario.build();

        let outcome = runner.cast(spell).resolve();

        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        outcome.assert_zone(&[spell], Zone::Graveyard);
        outcome.assert_life_delta(P0, life_gained);
        assert_two_soldiers_without_counters(&runner, text);
    }
}

/// Casts `text` with OPP2 staged and checks that the Soldiers and both
/// opposing creatures survive without counters.
fn assert_opp2_base_reading(text: &str) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opposing = stage_two_opposing_creatures(&mut scenario);
    let spell = free_spell(&mut scenario, text);
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    assert_two_soldiers_without_counters(&runner, text);
    for creature in opposing {
        assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
        assert_eq!(total_counters(&runner, creature), 0, "{text:?}");
    }
}

/// T14 (synthetic). A condition on the rider keeps the base reading.
#[test]
fn a_rider_condition_keeps_each_of_them_base_reading() {
    assert_opp2_base_reading(RIDER_CONDITION);
}

/// T15 (synthetic). The untyped head is unrecognized and dropped (coverage
/// `Swallow:Condition_If`); the row pins only that the tokens get no counters.
#[test]
fn an_unrecognized_head_on_the_creation_keeps_each_of_them_base_reading() {
    assert_opp2_base_reading(UNTYPED_HEAD);
}

/// T16 (synthetic). An optional creation keeps the base reading.
#[test]
fn an_optional_creation_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = free_spell(&mut scenario, OPTIONAL_CREATION);
    let mut runner = scenario.build();

    runner.cast(spell).accept_optional().resolve();

    assert_two_soldiers_without_counters(&runner, OPTIONAL_CREATION);
}

/// T17 (synthetic). One token keeps the base reading.
#[test]
fn a_single_token_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = free_spell(&mut scenario, ONE_TOKEN);
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    let soldiers = tokens_named(&runner, "Soldier");
    assert_eq!(soldiers.len(), 1, "the Soldier exists");
    assert_eq!(total_counters(&runner, soldiers[0]), 0);
}

/// T18 (synthetic). A triggered ability (CR 113.3c) keeps the base reading.
#[test]
fn a_triggered_ability_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, "Src", 1, 1, TRIGGERED_SHAPE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    runner.cast(source).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_two_soldiers_without_counters(&runner, TRIGGERED_SHAPE);
}

/// T19 (guard, synthetic). The positive witness for an activated ability: the
/// cost is not an instruction (CR 602.1), so the two Soldiers get the counter.
#[test]
fn an_activated_two_instruction_ability_binds_its_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Src", 1, 1, ACTIVATED_SHAPE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.activate(source, 0).resolve();

    outcome.assert_tapped(source, true);
    let soldiers = tokens_named(&runner, "Soldier");
    assert_eq!(soldiers.len(), 2, "two Soldiers exist");
    for soldier in soldiers {
        assert_eq!(counter(&runner, soldier, &CounterType::Plus1Plus1), 1);
    }
}

/// T20 (synthetic). The token parser drops the trailing guard, so the Treasure
/// is created unconditionally, as at base; the continuation is not the life
/// gain the rider frame admits, so "each of them" keeps the base reading.
#[test]
fn a_swallowed_continuation_guard_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opposing = stage_two_opposing_creatures(&mut scenario);
    let spell = free_spell(&mut scenario, SWALLOWED_CONTINUATION_GUARD);
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    assert_two_soldiers_without_counters(&runner, SWALLOWED_CONTINUATION_GUARD);
    assert_eq!(
        tokens_named(&runner, "Treasure").len(),
        1,
        "the Treasure exists"
    );
    for creature in opposing {
        assert_eq!(total_counters(&runner, creature), 0);
    }
}

/// T25 (synthetic). A continuation with a condition the parser does not
/// represent fails closed. Rows 1 and 2 create the Treasure (the parser drops
/// the condition, as at base); row 3 draws the card.
#[test]
fn a_swallowed_continuation_condition_keeps_each_of_them_base_reading() {
    for text in [
        CONTINUATION_AS_LONG_AS,
        CONTINUATION_WHILE,
        CONTINUATION_INSTEAD,
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        stage_library(&mut scenario);
        let spell = free_spell(&mut scenario, text);
        let mut runner = scenario.build();

        let outcome = runner.cast(spell).resolve();

        assert_two_soldiers_without_counters(&runner, text);
        if text == CONTINUATION_INSTEAD {
            outcome.assert_hand_drawn(P0, 1);
        } else {
            assert_eq!(
                tokens_named(&runner, "Treasure").len(),
                1,
                "{text:?}: the Treasure exists"
            );
        }
    }
}

/// T21 (synthetic). An activation instruction is a third sentence, so the
/// base reading is kept.
#[test]
fn an_activation_instruction_sentence_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Src", 1, 1, ACTIVATION_INSTRUCTION)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.activate(source, 0).resolve();

    outcome.assert_tapped(source, true);
    assert_two_soldiers_without_counters(&runner, ACTIVATION_INSTRUCTION);
}

/// T23 (synthetic). A variable token count keeps the base reading; the count
/// resolves to two.
#[test]
fn a_variable_token_count_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = [
        scenario.add_creature(P0, "Bear A", 2, 2).id(),
        scenario.add_creature(P0, "Bear B", 2, 2).id(),
    ];
    let spell = free_spell(&mut scenario, VARIABLE_COUNT);
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    assert_two_soldiers_without_counters(&runner, VARIABLE_COUNT);
    for bear in bears {
        assert_eq!(total_counters(&runner, bear), 0);
    }
}

/// T22 (synthetic). A mode of a modal spell (CR 700.2) carries no printed
/// description, so the base reading is kept.
#[test]
fn a_modal_mode_keeps_each_of_them_base_reading() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario);
    let spell = free_spell(&mut scenario, MODAL_MODE);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).modes(&[0]).resolve();

    assert_two_soldiers_without_counters(&runner, MODAL_MODE);
    outcome.assert_hand_drawn(P0, 0);
}
