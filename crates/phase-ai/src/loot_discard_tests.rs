//! The discard after a loot follows the keep order when the search cannot
//! tell the candidates apart
//! (`simforge/docs/spec-ai-loot-discard-keep-order-2026-10-20.md`; judge F2
//! and row JE6 on e6602dad2, judge S7 on 9666a59d6).
//!
//! At a `DiscardChoice` that follows a loot, every candidate discard
//! searches to the same value on boards the leaf cannot separate (no mana
//! open, the spells not castable inside the horizon): in the e6602dad2 run
//! 34 of the 36 discards after a Divining Duelist loot were exact top-two
//! ties, and the root took the last candidate in action order. The keep order
//! (`card_value::keep_key` / `cmp_keep`) already says which card is worth
//! least, and the loot's price (`card_value::expected_loot_gain`) assumes it.
//! These rows read the production chooser (`choose_action_with_session`) at
//! the prompt.
//!
//! Card text verified against MTGJSON AtomicCards (`data/mtgjson`).

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
use crate::session::AiSession;
use crate::test_support::default_deck_plan;

/// Divining Duelist ({2}{U}, 3/2), MTGJSON AtomicCards.
const DUELIST: &str = "Flash\nWhen this creature enters, choose one —\n• Tap target creature.\n• Untap target creature.\n• Draw a card, then discard a card.";
/// Extended Absence ({3}{B} instant), MTGJSON AtomicCards.
const EXTENDED_ABSENCE: &str = "Exile target creature or planeswalker. Extended Absence deals 1 damage to each opponent and you gain 1 life.";
/// Mind Meanderer ({3}{G}{U}{U}, 4/4), MTGJSON AtomicCards.
const MIND_MEANDERER: &str = "Flying\nThis creature has vigilance as long as you control a Jace planeswalker.\nWhen this creature enters, it fights up to one target creature an opponent controls. (Each deals damage equal to its power to the other.)";
/// Murmuring Volume ({3} artifact), MTGJSON AtomicCards.
const MURMURING_VOLUME: &str =
    "{T}: Add one mana of any color.\n{2}, {T}, Discard a card: Draw a card.";

/// Duelist's loot mode, in card-text order.
const LOOT: usize = 2;

/// The near-tie the rows sit in: scores equal to three decimals, the reading
/// of the signals script's `f_score_tie` diagnostic.
const TIE: f64 = 0.001;

/// Accurate mode as the FRA runs play it: VeryHard, temperature 0.
fn accurate() -> AiConfig {
    let mut config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    config.temperature = 0.0;
    config
}

/// A session whose plan is the default deck's (land target 6), so the keep
/// order reads the discarding player's plan as it does in a real game.
fn planned_session() -> Arc<AiSession> {
    let mut session = AiSession::default();
    session.plan.insert(P0, default_deck_plan());
    session.plan.insert(P1, default_deck_plan());
    Arc::new(session)
}

/// Stock both libraries so no line is a deck-out loss; P0 draws `own_top`.
fn stock_libraries(s: &mut GameScenario, own_top: &str) {
    for _ in 0..20 {
        s.add_land_to_library_top(P0, own_top);
        s.add_land_to_library_top(P1, "Plains");
    }
}

fn add_duelist(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_hand_from_oracle(P0, "Divining Duelist", 3, 2, DUELIST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id()
}

fn add_extended_absence(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, "Extended Absence", true, EXTENDED_ABSENCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id()
}

fn add_mind_meanderer(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_hand_from_oracle(P0, "Mind Meanderer", 4, 4, MIND_MEANDERER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Green,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            generic: 3,
        })
        .id()
}

/// `count` basic lands for P0, untapped.
fn lands(s: &mut GameScenario, color: ManaColor, count: usize) -> Vec<ObjectId> {
    (0..count).map(|_| s.add_basic_land(P0, color)).collect()
}

fn tap(state: &mut GameState, ids: &[ObjectId]) {
    for id in ids {
        state.objects.get_mut(id).expect("land").tapped = true;
    }
}

/// Cast the Duelist, choose its loot, and pass until P0 is at the discard.
fn to_loot_discard(state: GameState, duelist: ObjectId) -> GameState {
    let card_id = state.objects[&duelist].card_id;
    let mut r = GameRunner::from_state(state);
    r.act(GameAction::CastSpell {
        object_id: duelist,
        card_id,
        targets: Vec::new(),
        payment_mode: Default::default(),
    })
    .expect("cast");
    for _ in 0..8 {
        if matches!(r.state().waiting_for, WaitingFor::AbilityModeChoice { .. }) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        matches!(r.state().waiting_for, WaitingFor::AbilityModeChoice { .. }),
        "reach-guard: mode prompt, got {}",
        r.state().waiting_for.variant_name()
    );
    r.act(GameAction::SelectModes {
        indices: vec![LOOT],
    })
    .expect("loot");
    for _ in 0..8 {
        if matches!(r.state().waiting_for, WaitingFor::DiscardChoice { .. }) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        matches!(r.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "reach-guard: loot discard, got {}",
        r.state().waiting_for.variant_name()
    );
    r.state().clone()
}

/// The production root scores at the prompt, by the discarded card's name.
fn named_scores(state: &GameState, session: &Arc<AiSession>) -> Vec<(String, f64)> {
    crate::search::score_candidates_with_session(state, P0, &accurate(), session)
        .into_iter()
        .map(|(action, score)| (discarded_names(state, &action), score))
        .collect()
}

fn discarded_names(state: &GameState, action: &GameAction) -> String {
    match action {
        GameAction::SelectCards { cards } => cards
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect::<Vec<_>>()
            .join("+"),
        other => format!("{other:?}"),
    }
}

fn score_of(scores: &[(String, f64)], name: &str) -> f64 {
    scores
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, s)| *s)
        .fold(f64::NEG_INFINITY, f64::max)
}

/// What the production chooser discards at the prompt.
fn chosen(state: &GameState, session: &Arc<AiSession>) -> String {
    let mut rng = ChaCha20Rng::seed_from_u64(7);
    let action =
        crate::search::choose_action_with_session(state, P0, &accurate(), &mut rng, session)
            .expect("an action at the discard prompt");
    discarded_names(state, &action)
}

fn is_land(name: &str) -> bool {
    matches!(name, "Island" | "Swamp" | "Forest" | "Plains" | "Mountain")
}

/// LD1, the 1018 T12 and 1024 T10 shape: six lands in play, all tapped once
/// the Duelist is cast, so nothing in hand is castable inside the horizon;
/// the hand holds a land (P0's land target is 6, so a seventh land adds
/// nothing the plan wants) and a removal spell with a legal target, plus the
/// Island the loot draws. The search ties the land and the spell; the keep
/// order gives up the land (3.0) before Extended Absence. The land SHALL be
/// discarded.
#[test]
fn ld1_loot_discard_at_six_lands_pitches_the_land_over_a_removal_spell() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s, "Island");
    s.add_creature(P1, "Opposing Bear", 3, 3);
    lands(&mut s, ManaColor::Blue, 3);
    let swamps = lands(&mut s, ManaColor::Black, 3);
    s.add_land_to_hand(P0, "Swamp");
    let duelist = add_duelist(&mut s);
    add_extended_absence(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::PreCombatMain;
    tap(&mut st, &swamps);
    let prompt = to_loot_discard(st, duelist);

    let session = planned_session();
    let scores = named_scores(&prompt, &session);
    eprintln!("LD1 discard scores {scores:?}");
    let land = score_of(&scores, "Swamp").max(score_of(&scores, "Island"));
    let spell = score_of(&scores, "Extended Absence");
    assert!(
        (land - spell).abs() <= TIE,
        "precondition: the search ties the land and the spell: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// LD2, the 1004 T21 shape: seven lands in play, one left untapped after the
/// Duelist; the hand holds a Swamp (past the land target of 6) and Mind
/// Meanderer, which the lone open land cannot cast, plus the drawn Forest.
/// The search ties every candidate; the keep order gives up a land first.
/// A land SHALL be discarded.
#[test]
fn ld2_loot_discard_at_seven_lands_pitches_a_land_over_a_creature() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::Upkeep);
    stock_libraries(&mut s, "Forest");
    lands(&mut s, ManaColor::Blue, 4);
    let swamps = lands(&mut s, ManaColor::Black, 3);
    s.add_land_to_hand(P0, "Swamp");
    let duelist = add_duelist(&mut s);
    add_mind_meanderer(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::Upkeep;
    tap(&mut st, &swamps);
    let prompt = to_loot_discard(st, duelist);

    let session = planned_session();
    let scores = named_scores(&prompt, &session);
    eprintln!("LD2 discard scores {scores:?}");
    let land = score_of(&scores, "Swamp").max(score_of(&scores, "Forest"));
    let spell = score_of(&scores, "Mind Meanderer");
    assert!(
        (land - spell).abs() <= TIE,
        "precondition: the search ties the land and the creature: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// LD3, guard: the search separates the candidates by more than the near-tie
/// band. Seven lands, four of them open after the Duelist; the hand holds a
/// Swamp (past the land target) and a 4/4 the open mana casts, plus the drawn
/// Island. Discarding a land keeps the cast; the search's pick SHALL stand,
/// and it is a land (JE6's board on e6602dad2, Island 81.79 against the
/// creature 70.11). The row also pins the other direction: the creature's
/// score is more than the band below the land's, so no tie-break applies.
#[test]
fn ld3_guard_a_separated_search_pick_stands() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s, "Island");
    lands(&mut s, ManaColor::Blue, 7);
    s.add_land_to_hand(P0, "Swamp");
    s.add_creature_to_hand(P0, "Keeper", 4, 4);
    let duelist = add_duelist(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::PreCombatMain;
    let prompt = to_loot_discard(st, duelist);

    let session = planned_session();
    let scores = named_scores(&prompt, &session);
    eprintln!("LD3 discard scores {scores:?}");
    let best = scores
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("scores");
    let keeper = score_of(&scores, "Keeper");
    assert!(
        best.1 - keeper > TIE,
        "precondition: the search separates: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert_eq!(pick, best.0, "the search's pick stands: {scores:?}");
}

/// LD5, guard where the search and the keep order disagree. Four lands in
/// play, one left open after the Duelist, so the drawn Island is the next
/// land drop the plan needs (`KeepTier::NeededManaSource`, kept last); the
/// hand holds a 2/2 for {U} and Mind Meanderer, which the Islands cannot
/// cast. The keep order gives up the 2/2 (5.5) before Mind Meanderer (13.0),
/// but the search casts the 2/2 with the open Island and discards Mind
/// Meanderer by more than the band. The search's pick SHALL stand.
#[test]
fn ld5_guard_the_search_overrules_the_keep_order_outside_the_band() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s, "Island");
    lands(&mut s, ManaColor::Blue, 4);
    s.add_creature_to_hand(P0, "Cheap Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        });
    let duelist = add_duelist(&mut s);
    add_mind_meanderer(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::PreCombatMain;
    let prompt = to_loot_discard(st, duelist);

    let session = planned_session();
    let scores = named_scores(&prompt, &session);
    eprintln!("LD5 discard scores {scores:?}");
    let meanderer = score_of(&scores, "Mind Meanderer");
    let bear = score_of(&scores, "Cheap Bear");
    assert!(
        meanderer - bear > TIE,
        "precondition: the search prefers giving up Mind Meanderer by more than the band: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert_eq!(
        pick, "Mind Meanderer",
        "the search's pick stands: {scores:?}"
    );
}

/// LD4, the discard as a cost (CR 118.1 + CR 701.9b): Murmuring Volume's
/// "{2}, {T}, Discard a card: Draw a card." at six lands, the hand a Swamp
/// and Extended Absence with a legal target. The same keep order applies:
/// the land SHALL be discarded to pay the cost.
#[test]
fn ld4_discard_cost_follows_the_keep_order() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s, "Island");
    s.add_creature(P1, "Opposing Bear", 3, 3);
    lands(&mut s, ManaColor::Black, 6);
    let volume = s
        .add_artifact_from_oracle(P0, "Murmuring Volume", MURMURING_VOLUME)
        .id();
    s.add_land_to_hand(P0, "Swamp");
    add_extended_absence(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::PreCombatMain;
    let mut r = GameRunner::from_state(st);
    r.act(GameAction::ActivateAbility {
        source_id: volume,
        ability_index: 1,
    })
    .expect("activate the loot");
    for _ in 0..4 {
        if matches!(r.state().waiting_for, WaitingFor::PayCost { .. }) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    let prompt = r.state().clone();
    assert!(
        matches!(prompt.waiting_for, WaitingFor::PayCost { .. }),
        "reach-guard: discard cost, got {}",
        prompt.waiting_for.variant_name()
    );

    let session = planned_session();
    let scores = named_scores(&prompt, &session);
    eprintln!("LD4 cost scores {scores:?}");
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}
