//! The search's leaf credits a graveyard card's castable recursion at that
//! cast's payoff, and reads a surplus land in hand below a spell
//! (`simforge/docs/spec-ai-leaf-graveyard-and-surplus-land-2026-10-20.md`;
//! judge focus 2 and S1 on 754e28a81).
//!
//! At a discard cost (CR 118.1 + CR 701.9b) the root scores each candidate as
//! its continuation plus a tenth of its cost price. The leaf counts a surplus
//! land in hand and a spell in hand as one card each, and a flashback card
//! (CR 702.34a) in the graveyard at a flat recursion bonus of about 0.005 of a
//! card, so the line that pitches the flashback card and casts it from the
//! graveyard inside the horizon nets a card that the line keeping it never
//! sees (1010 T22 L2922, 1015 T24, 1027 T18; step 0 in
//! `deck-builder/sims/fra-phase/2026-10-20-cost-discard/NOTE.md`). An
//! override after the search was rejected (`fork-checks/754e28a81/JUDGE.md`,
//! B1 and B2: it threw away a won game and gave up a mana creature); these
//! rows ask the leaf to read the two things it misses.
//!
//! CD1 to CD4 are the make agent's rows from the archived red commit
//! bed2980e9 (tag `archive/discard-cost-override-2026-10-20`). CD1 and CD2
//! carried a precondition that the search itself gives up the flashback card
//! (the premise of the override); with the leaf reading the cast, the search
//! scores the land first, and that is what they assert now. J1 and J2 are the
//! judge's rows from `fork-checks/754e28a81/judge-out/judge_rows_754e28a81.rs`.
//!
//! Card text verified against MTGJSON AtomicCards (`data/mtgjson`).

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{GameState, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use super::PlannerServices;
use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::deck_profile::DeckArchetype;
use crate::session::AiSession;
use crate::test_support::default_deck_plan;

/// Twinned Vision ({1}{U/R} instant), MTGJSON AtomicCards.
const TWINNED_VISION: &str = "Draw a card. If this spell wasn't cast from your hand, draw two \
    cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from your \
    graveyard for its flashback cost. Then exile it.)";
/// Generous Revival ({2}{W} sorcery), MTGJSON AtomicCards.
const GENEROUS_REVIVAL: &str = "Return target creature card with mana value 3 or less from your \
    graveyard to the battlefield with an additional +1/+1 counter on it.\nFlashback {4}{W} (You \
    may cast this card from your graveyard for its flashback cost. Then exile it.)";
/// Recursive Recruitment ({3}{U}{B} sorcery), MTGJSON AtomicCards.
const RECURSIVE_RECRUITMENT: &str = "Create two 2/2 colorless Wizard Soldier creature tokens \
    named Cadet. If this spell was cast from a graveyard, put a +1/+1 counter on each of them \
    for every three cards in your graveyard.\nFlashback {6}{U}{B} (You may cast this card from \
    your graveyard for its flashback cost. Then exile it.)";
/// Murmuring Volume ({3} artifact), MTGJSON AtomicCards.
const MURMURING_VOLUME: &str =
    "{T}: Add one mana of any color.\n{2}, {T}, Discard a card: Draw a card.";

/// Scores equal to three decimals: `card_value::DISCARD_TIE_BAND`.
const TIE: f64 = 0.001;

/// The turn the leaf rows are read on: the late weight set, where the FRA
/// discards were (T18 to T24).
const LATE_TURN: u32 = 18;

// --- Root rows: the production chooser at a discard cost --------------------

/// Accurate mode as the FRA runs play it: VeryHard, temperature 0.
fn accurate() -> AiConfig {
    let mut config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    config.temperature = 0.0;
    config
}

/// A session whose plan is the default deck's (land target 6) for both
/// seats, as `AiSession::from_game` builds one for every seat with a deck.
fn planned_session() -> AiSession {
    let mut session = AiSession::default();
    session.plan.insert(P0, default_deck_plan());
    session.plan.insert(P1, default_deck_plan());
    session
}

/// Stock both libraries so no line is a deck-out loss.
fn stock_libraries(s: &mut GameScenario) {
    for _ in 0..20 {
        s.add_land_to_library_top(P0, "Island");
        s.add_land_to_library_top(P1, "Plains");
    }
}

fn twinned_vision_in_hand(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, "Twinned Vision", true, "")
        .with_mana_cost(twinned_vision_cost())
        .from_oracle_text_with_keywords(&["Flashback"], TWINNED_VISION)
        .id()
}

fn twinned_vision_in_graveyard(s: &mut GameScenario, player: PlayerId) -> ObjectId {
    s.add_spell_to_graveyard(player, "Twinned Vision", true)
        .with_mana_cost(twinned_vision_cost())
        .from_oracle_text_with_keywords(&["Flashback"], TWINNED_VISION)
        .id()
}

fn twinned_vision_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::BlueRed],
        generic: 1,
    }
}

/// P0's board for the root rows: `lands` Islands on the battlefield, Murmuring
/// Volume, and an opposing 2/2 so the board is not empty.
fn board(lands: usize) -> (GameScenario, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::Upkeep);
    stock_libraries(&mut s);
    s.add_creature(P1, "Opposing Bear", 2, 2);
    for _ in 0..lands {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    let volume = s
        .add_artifact_from_oracle(P0, "Murmuring Volume", MURMURING_VOLUME)
        .id();
    (s, volume)
}

fn is_discard_cost(w: &WaitingFor) -> bool {
    matches!(
        w,
        WaitingFor::PayCost {
            kind: PayCostKind::Discard,
            ..
        }
    )
}

/// Pass until P0 is at a discard cost.
fn to_discard_cost(mut r: GameRunner) -> GameState {
    for _ in 0..8 {
        if is_discard_cost(&r.state().waiting_for) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    let prompt = r.state().clone();
    assert!(
        is_discard_cost(&prompt.waiting_for),
        "reach-guard: discard cost, got {}",
        prompt.waiting_for.variant_name()
    );
    prompt
}

/// Activate Murmuring Volume's loot and stop at its discard cost.
fn volume_loot_cost(s: GameScenario, volume: ObjectId) -> GameState {
    let mut st = s.build().state().clone();
    st.phase = Phase::Upkeep;
    let mut r = GameRunner::from_state(st);
    r.act(GameAction::ActivateAbility {
        source_id: volume,
        ability_index: 1,
    })
    .expect("activate the loot");
    to_discard_cost(r)
}

/// Cast `spell` from the graveyard by flashback in `phase` and stop at its
/// discard cost.
fn flashback_cost(s: GameScenario, spell: ObjectId, phase: Phase) -> GameState {
    let mut st = s.build().state().clone();
    st.phase = phase;
    let card_id = st.objects[&spell].card_id;
    let mut r = GameRunner::from_state(st);
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: Vec::new(),
        payment_mode: Default::default(),
    })
    .expect("cast by flashback");
    to_discard_cost(r)
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
            .expect("an action at the discard cost");
    discarded_names(state, &action)
}

fn is_land(name: &str) -> bool {
    matches!(name, "Island" | "Swamp" | "Forest" | "Plains" | "Mountain")
}

/// The best land's score at the prompt.
fn best_land(scores: &[(String, f64)]) -> f64 {
    ["Island", "Plains", "Swamp", "Forest", "Mountain"]
        .iter()
        .map(|name| score_of(scores, name))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// CD1, the 1027 T18 shape: nine lands in play (P0's land target is 6, so
/// every land in hand is past it), Murmuring Volume's loot ("{2}, {T},
/// Discard a card: Draw a card."), and a hand of Twinned Vision, castable,
/// and two lands. The search SHALL score a land at least as high as Twinned
/// Vision (within the tie band) and a land SHALL be given up, with no rule
/// after the search.
#[test]
fn cd1_volume_loot_cost_gives_up_a_surplus_land_before_twinned_vision() {
    let (mut s, volume) = board(9);
    twinned_vision_in_hand(&mut s);
    s.add_land_to_hand(P0, "Island");
    s.add_land_to_hand(P0, "Plains");
    let prompt = volume_loot_cost(s, volume);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("CD1 cost scores {scores:?}");
    let spell = score_of(&scores, "Twinned Vision");
    assert!(
        best_land(&scores) > spell - TIE,
        "the search itself scores a land first: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// CD2, the 1015 T24 shape: ten lands in play (six Islands, four Plains),
/// Twinned Vision cast from the graveyard by flashback in the main phase,
/// its "Discard a card" cost paid from a hand of Generous Revival
/// ("Return target creature card with mana value 3 or less from your
/// graveyard to the battlefield with an additional +1/+1 counter on it.
/// Flashback {4}{W}") and two lands, a 3/3 for {2}{W} in the graveyard to
/// return. The search SHALL score a land at least as high as Generous
/// Revival and a land SHALL be given up, keeping the hand cast.
#[test]
fn cd2_flashback_cost_gives_up_a_surplus_land_before_generous_revival() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s);
    s.add_creature(P1, "Opposing Bear", 2, 2);
    for _ in 0..6 {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..4 {
        s.add_basic_land(P0, ManaColor::White);
    }
    s.add_creature_to_graveyard(P0, "Graveyard Knight", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        });
    let in_graveyard = twinned_vision_in_graveyard(&mut s, P0);
    s.add_spell_to_hand_from_oracle(P0, "Generous Revival", false, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Flashback"], GENEROUS_REVIVAL);
    s.add_land_to_hand(P0, "Island");
    s.add_land_to_hand(P0, "Plains");
    let prompt = flashback_cost(s, in_graveyard, Phase::PreCombatMain);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("CD2 cost scores {scores:?}");
    let spell = score_of(&scores, "Generous Revival");
    assert!(
        best_land(&scores) > spell - TIE,
        "the search itself scores a land first: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// CD3, guard: four lands in play, two short of the target, and a hand of
/// Twinned Vision and one Island, the next land drop
/// (`KeepTier::NeededManaSource`). The search separates the two and its pick
/// SHALL stand, whichever card it is.
#[test]
fn cd3_guard_a_needed_land_leaves_the_search_pick_standing() {
    let (mut s, volume) = board(4);
    twinned_vision_in_hand(&mut s);
    s.add_land_to_hand(P0, "Island");
    let prompt = volume_loot_cost(s, volume);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("CD3 cost scores {scores:?}");
    let best = scores
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("scores");
    let runner_up = scores
        .iter()
        .filter(|(name, _)| *name != best.0)
        .map(|(_, score)| *score)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(
        best.1 - runner_up > TIE,
        "precondition: the search separates: {scores:?}"
    );
    let pick = chosen(&prompt, &session);
    assert_eq!(pick, best.0, "the search's pick stands: {scores:?}");
}

/// CD4, guard: exactly six lands in play, on the target, so a land in hand
/// is `KeepTier::Ordinary`, not surplus. The hand is Twinned Vision and two
/// lands. The search's pick SHALL stand (within the tie band).
#[test]
fn cd4_guard_a_land_on_the_target_leaves_the_search_pick_standing() {
    let (mut s, volume) = board(6);
    twinned_vision_in_hand(&mut s);
    s.add_land_to_hand(P0, "Island");
    s.add_land_to_hand(P0, "Plains");
    let prompt = volume_loot_cost(s, volume);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("CD4 cost scores {scores:?}");
    let best = scores
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("scores");
    let pick = chosen(&prompt, &session);
    let tied_with_best = scores
        .iter()
        .any(|(name, score)| *name == pick && (best.1 - score).abs() <= TIE);
    assert!(
        tied_with_best,
        "the search's pick stands: {pick}, {scores:?}"
    );
}

/// J1 (judge, 754e28a81 B1): the flashback card's graveyard cast this turn is
/// lethal ("Each opponent loses 3 life", Flashback {U}, the opponent at 3),
/// its hand cast ({9}{U}) out of reach, and two surplus lands offered. The
/// search reads a won game; the flashback card SHALL be given up.
#[test]
fn j1_lethal_graveyard_cast_is_not_given_up_for_a_surplus_land() {
    let (mut s, volume) = board(9);
    s.with_life(P1, 3);
    s.add_spell_to_hand_from_oracle(P0, "Final Word", true, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 9,
        })
        .from_oracle_text_with_keywords(
            &["Flashback"],
            "Each opponent loses 3 life.\nFlashback {U} (You may cast this card from your \
             graveyard for its flashback cost. Then exile it.)",
        );
    s.add_land_to_hand(P0, "Island");
    s.add_land_to_hand(P0, "Plains");
    let prompt = volume_loot_cost(s, volume);
    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("J1 scores {scores:?}");
    let pick = chosen(&prompt, &session);
    assert_eq!(pick, "Final Word", "scores {scores:?}");
}

/// J2 (judge, 754e28a81 B2): the surplus mana source in hand is a 4/4
/// creature with a mana ability (an accelerant past the mana target), not a
/// land. The surplus-land reading is for lands; the 4/4 SHALL NOT be given up
/// in place of Twinned Vision. With the hand credit the leaf ties the two
/// (pitching either loses one hand card), so the keep order decides: a mana
/// creature past the mana target keeps the tier its body earns
/// (`KeepTier::Surplus` is for lands only), and Twinned Vision SHALL be given
/// up.
#[test]
fn j2_surplus_mana_creature_is_not_given_up_before_twinned_vision() {
    let (mut s, volume) = board(9);
    twinned_vision_in_hand(&mut s);
    s.add_creature_to_hand_from_oracle(P0, "Grove Hulk", 4, 4, "{T}: Add {U}.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });
    let prompt = volume_loot_cost(s, volume);
    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("J2 scores {scores:?}");
    let pick = chosen(&prompt, &session);
    assert_eq!(pick, "Twinned Vision", "scores {scores:?}");
}

// --- Leaf rows: what the planner's two leaves read -------------------------

/// An Aggro context (the FRA decks' archetype) whose session holds the
/// default deck's plan (land target 6) for both seats when `plans`, and no
/// plan otherwise.
fn context(config: &AiConfig, plans: bool) -> AiContext {
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
    let mut context =
        AiContext::analyze_for_player(&deck, &config.weights, &config.archetype_multipliers, P0);
    assert_eq!(context.deck_profile.archetype, DeckArchetype::Aggro);
    let mut session = (*context.session).clone();
    session.plan.clear();
    if plans {
        session.plan.insert(P0, default_deck_plan());
        session.plan.insert(P1, default_deck_plan());
    }
    context.session = Arc::new(session);
    context
}

/// `a`'s leaf values less `b`'s for P0, at the strategy leaf
/// (`evaluate_with_strategy`) and the rollout leaf (`quiesced_leaf_eval`),
/// with the hand-size weight the leaf applies at [`LATE_TURN`].
fn leaf_gaps(plans: bool, a: &GameState, b: &GameState) -> (f64, f64, f64) {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let policies = crate::policies::PolicyRegistry::shared();
    let context = context(&config, plans);
    let w_hand = context.adjusted_weights.for_turn(LATE_TURN).hand_size;
    let mut services = PlannerServices::new(P0, &config, policies, context);
    let strategy = services.evaluate_with_strategy(a) - services.evaluate_with_strategy(b);
    let rollout = services.quiesced_leaf_eval(a) - services.quiesced_leaf_eval(b);
    (strategy, rollout, w_hand)
}

/// A table at [`LATE_TURN`]: P0 with `p0_lands` untapped Islands, P1 with
/// `p1_lands` untapped Islands, both libraries stocked, P0 in its main phase.
fn table(p0_lands: usize, p1_lands: usize) -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s);
    for _ in 0..p0_lands {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..p1_lands {
        s.add_basic_land(P1, ManaColor::Blue);
    }
    s
}

fn late(s: GameScenario) -> GameState {
    let mut state = s.build().state().clone();
    state.turn_number = LATE_TURN;
    state
}

/// A sorcery that does nothing the pricing reads: one card, no payoff.
fn plain_sorcery_in_graveyard(s: &mut GameScenario, player: PlayerId) {
    s.add_spell_to_graveyard(player, "Plain Sorcery", false)
        .with_mana_cost(twinned_vision_cost());
}

fn plain_sorcery_in_hand(s: &mut GameScenario, player: PlayerId) {
    s.add_spell_to_hand(player, "Plain Sorcery", false)
        .with_mana_cost(twinned_vision_cost());
}

fn assert_gap(label: &str, (strategy, rollout, w_hand): (f64, f64, f64), cards: f64) {
    let want = cards * w_hand;
    assert!(
        (strategy - want).abs() < 1e-6,
        "{label}: strategy leaf gap {strategy:.6}, expected {cards} card(s) at the hand-size \
         weight, {want:.6}"
    );
    assert!(
        (rollout - want).abs() < 1e-6,
        "{label}: rollout leaf gap {rollout:.6}, expected {want:.6}"
    );
}

/// GC1 (CR 702.34a + CR 601.2f-h): Twinned Vision in P0's graveyard with nine
/// Islands in play and a card in hand to discard. Its flashback draws two
/// (cast from the graveyard, not the hand) for {1}{U/R}{U/R} and a card, so
/// it nets one card. Against a card with no recursion in its place, the leaf
/// SHALL read one card more at the hand-size weight, at both leaves.
#[test]
fn gc1_a_castable_flashback_draw_two_in_the_graveyard_reads_one_card() {
    let with = {
        let mut s = table(9, 2);
        twinned_vision_in_graveyard(&mut s, P0);
        plain_sorcery_in_hand(&mut s, P0);
        late(s)
    };
    let without = {
        let mut s = table(9, 2);
        plain_sorcery_in_graveyard(&mut s, P0);
        plain_sorcery_in_hand(&mut s, P0);
        late(s)
    };
    assert_gap("GC1", leaf_gaps(true, &with, &without), 1.0);
}

/// GC2: the same card out of reach: one Island in play and no land in hand,
/// so {1}{U/R}{U/R} is not within two land drops (CR 305.2). The leaf SHALL
/// credit nothing for it, not even the flat recursion bonus.
#[test]
fn gc2_a_flashback_out_of_reach_is_not_credited() {
    let with = {
        let mut s = table(1, 2);
        twinned_vision_in_graveyard(&mut s, P0);
        plain_sorcery_in_hand(&mut s, P0);
        late(s)
    };
    let without = {
        let mut s = table(1, 2);
        plain_sorcery_in_graveyard(&mut s, P0);
        plain_sorcery_in_hand(&mut s, P0);
        late(s)
    };
    assert_gap("GC2", leaf_gaps(true, &with, &without), 0.0);
}

/// GC3: reach is read for the graveyard's player the same way. Twinned Vision
/// in P1's graveyard with nine Islands of P1's in play is one card for P1,
/// one card less for P0; with one Island it is nothing.
#[test]
fn gc3_the_opponents_graveyard_cast_reads_the_same_way() {
    for (p1_lands, cards) in [(9, -1.0), (1, 0.0)] {
        let with = {
            let mut s = table(4, p1_lands);
            twinned_vision_in_graveyard(&mut s, P1);
            plain_sorcery_in_hand(&mut s, P1);
            late(s)
        };
        let without = {
            let mut s = table(4, p1_lands);
            plain_sorcery_in_graveyard(&mut s, P1);
            plain_sorcery_in_hand(&mut s, P1);
            late(s)
        };
        assert_gap(
            &format!("GC3 P1 at {p1_lands} lands"),
            leaf_gaps(true, &with, &without),
            cards,
        );
    }
}

/// GC4: reach reads colour. Recursive Recruitment's flashback {6}{U}{B}
/// makes two 2/2s; with nine Islands and no black source it is out of reach
/// and credited nothing, with a Swamp among nine lands it is credited above
/// nothing.
#[test]
fn gc4_a_flashback_whose_colour_is_unmet_is_not_credited() {
    let gap = |swamp: bool| {
        let build = |recruitment: bool| {
            let mut s = table(8, 2);
            s.add_basic_land(
                P0,
                if swamp {
                    ManaColor::Black
                } else {
                    ManaColor::Blue
                },
            );
            if recruitment {
                s.add_spell_to_graveyard(P0, "Recursive Recruitment", false)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
                        generic: 3,
                    })
                    .from_oracle_text_with_keywords(&["Flashback"], RECURSIVE_RECRUITMENT);
            } else {
                plain_sorcery_in_graveyard(&mut s, P0);
            }
            late(s)
        };
        leaf_gaps(true, &build(true), &build(false))
    };
    assert_gap("GC4 no black source", gap(false), 0.0);
    let (strategy, rollout, w_hand) = gap(true);
    assert!(
        strategy > 0.5 * w_hand && rollout > 0.5 * w_hand,
        "GC4 with a Swamp: credited above half a card, strategy {strategy:.6}, rollout \
         {rollout:.6}"
    );
}

/// SL1: nine lands in play, past the default plan's land target of 6, so a
/// land in hand is `KeepTier::Surplus` (`card_value::keep_tier`), worth
/// nothing to hold (`card_value::held_value`). A hand of one spell SHALL read
/// one card above a hand of one surplus land, at both leaves.
#[test]
fn sl1_a_surplus_land_in_hand_reads_below_a_spell() {
    let land = {
        let mut s = table(9, 2);
        s.add_land_to_hand(P0, "Island");
        late(s)
    };
    let spell = {
        let mut s = table(9, 2);
        plain_sorcery_in_hand(&mut s, P0);
        late(s)
    };
    assert_gap("SL1", leaf_gaps(true, &spell, &land), 1.0);
}

/// SL2, guard: six lands in play, on the target, so the land in hand is
/// `Ordinary`; four lands, so it is the next land drop (`NeededManaSource`);
/// nine lands with no plan in the session. A land and a spell SHALL read the
/// same at each.
#[test]
fn sl2_guard_a_land_on_or_below_the_target_or_without_a_plan_reads_as_a_card() {
    for (lands, plans) in [(6, true), (4, true), (9, false)] {
        let land = {
            let mut s = table(lands, 2);
            s.add_land_to_hand(P0, "Island");
            late(s)
        };
        let spell = {
            let mut s = table(lands, 2);
            plain_sorcery_in_hand(&mut s, P0);
            late(s)
        };
        assert_gap(
            &format!("SL2 at {lands} lands, plans {plans}"),
            leaf_gaps(plans, &spell, &land),
            0.0,
        );
    }
}

/// SL3: the reading is a differential like every leaf term. P1 at nine lands
/// holding a surplus Plains reads one card better for P0 than P1 holding a
/// spell.
#[test]
fn sl3_the_opponents_surplus_land_reads_the_same_way() {
    let land = {
        let mut s = table(4, 9);
        s.add_land_to_hand(P1, "Plains");
        late(s)
    };
    let spell = {
        let mut s = table(4, 9);
        plain_sorcery_in_hand(&mut s, P1);
        late(s)
    };
    assert_gap("SL3", leaf_gaps(true, &land, &spell), 1.0);
}

/// SL4, guard (lands only, judge B2): a 4/4 with "{T}: Add {U}." in hand at
/// nine lands is a surplus accelerant in the keep order, but it is a creature
/// the plan does not discount at the leaf: it SHALL read the same as a 4/4
/// without the mana ability.
#[test]
fn sl4_guard_a_surplus_mana_creature_in_hand_is_not_discounted() {
    let build = |mana: bool| {
        let mut s = table(9, 2);
        s.add_creature_to_hand_from_oracle(
            P0,
            "Grove Hulk",
            4,
            4,
            if mana { "{T}: Add {U}." } else { "" },
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        });
        late(s)
    };
    assert_gap("SL4", leaf_gaps(true, &build(true), &build(false)), 0.0);
}

/// A table at nine Islands and a Swamp for P0, `others` plain sorceries in
/// P0's graveyard beside the card under test (or a plain sorcery in its
/// place), and a card in P0's hand.
fn graveyard_pair(others: usize, add: impl Fn(&mut GameScenario)) -> (GameState, GameState) {
    let build = |with: bool| {
        let mut s = table(9, 2);
        s.add_basic_land(P0, ManaColor::Black);
        for _ in 0..others {
            plain_sorcery_in_graveyard(&mut s, P0);
        }
        if with {
            add(&mut s);
        } else {
            plain_sorcery_in_graveyard(&mut s, P0);
        }
        plain_sorcery_in_hand(&mut s, P0);
        late(s)
    };
    (build(true), build(false))
}

/// GC5 (CR 702.138a): an instant "Draw two cards." with "Escape—{U}, Exile
/// two other cards from your graveyard." is two cards from the graveyard when
/// two other cards are there to exile, and nothing with one.
#[test]
fn gc5_an_escape_instant_reads_its_cast_while_the_exile_cost_can_be_paid() {
    let add = |s: &mut GameScenario| {
        s.add_spell_to_graveyard(P0, "Escaping Insight", true)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            })
            .from_oracle_text_with_keywords(
                &["Escape"],
                "Draw two cards.\nEscape—{U}, Exile two other cards from your graveyard. \
                 (You may cast this card from your graveyard for its escape cost.)",
            );
    };
    let (with, without) = graveyard_pair(2, add);
    assert_gap("GC5 two others", leaf_gaps(true, &with, &without), 2.0);
    let (with, without) = graveyard_pair(1, add);
    assert_gap("GC5 one other", leaf_gaps(true, &with, &without), 0.0);
}

/// GC6 (CR 702.138a): a 3/3 creature card with "Escape—{2}{B}, Exile two
/// other cards from your graveyard." puts the same card on the battlefield
/// that a cast from hand would: one card, as the leaf reads a card in hand.
#[test]
fn gc6_an_escape_creature_reads_as_one_card() {
    let (with, without) = graveyard_pair(2, |s| {
        s.add_creature_to_graveyard(P0, "Escaping Brute", 3, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 2,
            })
            .from_oracle_text_with_keywords(
                &["Escape"],
                "Escape—{2}{B}, Exile two other cards from your graveyard. (You may cast \
                 this card from your graveyard for its escape cost.)",
            );
    });
    assert_gap("GC6", leaf_gaps(true, &with, &without), 1.0);
}

/// GC7 (CR 702.84a): unearth returns the creature until the next end step, so
/// what it does on entering is what the graveyard holds: "When this creature
/// enters, draw a card." with Unearth {U} is one card, and a vanilla 2/2 with
/// Unearth {U} nothing.
#[test]
fn gc7_unearth_reads_what_the_creature_does_on_entering() {
    let unearth = |etb: bool| {
        move |s: &mut GameScenario| {
            let text = if etb {
                "When this creature enters, draw a card.\nUnearth {U} ({U}: Return this card \
                 from your graveyard to the battlefield. It gains haste. Exile it at the \
                 beginning of the next end step or if it would leave the battlefield. Unearth \
                 only as a sorcery.)"
            } else {
                "Unearth {U} ({U}: Return this card from your graveyard to the battlefield. It \
                 gains haste. Exile it at the beginning of the next end step or if it would \
                 leave the battlefield. Unearth only as a sorcery.)"
            };
            s.add_creature_to_graveyard(P0, "Unearthed Scout", 2, 2)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 1,
                })
                .from_oracle_text_with_keywords(&["Unearth"], text);
        }
    };
    let (with, without) = graveyard_pair(0, unearth(true));
    assert_gap(
        "GC7 enters and draws",
        leaf_gaps(true, &with, &without),
        1.0,
    );
    let (with, without) = graveyard_pair(0, unearth(false));
    assert_gap("GC7 vanilla", leaf_gaps(true, &with, &without), 0.0);
}

// --- Pair 2: a flashback card in hand carries its graveyard cast -----------
//
// The first pair credited a graveyard cast only once the card was in the
// graveyard; in hand the card was one card. A card whose graveyard credit
// exceeds one card was then cheaper to pitch than to hold: in the 40 games at
// dd79de333, 1010 T20 L2608 gave up Recursive Recruitment (27.145) over a
// surplus Plains (26.653) at nine sources, and flashback cards discarded from
// hand rose from 17 to 23 (`deck-builder/sims/fra-phase/
// 2026-10-20-leaf-graveyard/NOTE.md`). The 96d0bfd2a B1 reading: kept, a
// flashback card gives its hand cast and its graveyard cast; discarded, it
// keeps the graveyard cast, so a discard loses the hand card alone. So a card
// in hand carries the same credit, under the same reach test, beside its one
// hand card. A consequence the rows pin: the leaf reads a flashback card in
// hand above a plain card of the same hand value.

fn recursive_recruitment_in(s: &mut GameScenario, player: PlayerId, zone: &str) -> ObjectId {
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
        generic: 3,
    };
    let mut builder = match zone {
        "hand" => s.add_spell_to_hand_from_oracle(player, "Recursive Recruitment", false, ""),
        _ => s.add_spell_to_graveyard(player, "Recursive Recruitment", false),
    };
    builder
        .with_mana_cost(cost)
        .from_oracle_text_with_keywords(&["Flashback"], RECURSIVE_RECRUITMENT)
        .id()
}

/// HR1, the 1010 T20 L2608 shape: nine lands (four Islands, four Plains, a
/// Swamp), Twinned Vision cast from the graveyard by flashback at upkeep, its
/// "Discard a card" paid from a hand of Recursive Recruitment (its flashback
/// {6}{U}{B} in reach) and a Plains past the land target. A land SHALL be
/// given up.
#[test]
fn hr1_flashback_cost_gives_up_a_surplus_plains_before_recursive_recruitment() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::Upkeep);
    stock_libraries(&mut s);
    s.add_creature(P1, "Opposing Bear", 2, 2);
    for _ in 0..4 {
        s.add_basic_land(P0, ManaColor::Blue);
        s.add_basic_land(P0, ManaColor::White);
    }
    s.add_basic_land(P0, ManaColor::Black);
    let in_graveyard = twinned_vision_in_graveyard(&mut s, P0);
    recursive_recruitment_in(&mut s, P0, "hand");
    s.add_land_to_hand(P0, "Plains");
    let prompt = flashback_cost(s, in_graveyard, Phase::Upkeep);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("HR1 cost scores {scores:?}");
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// HR2, the 1010 T22 L2922 shape: nine Islands, Twinned Vision cast from the
/// graveyard by flashback, its discard paid from a hand of a second Twinned
/// Vision and a surplus Plains. A land SHALL be given up.
#[test]
fn hr2_flashback_cost_gives_up_a_surplus_land_before_a_second_twinned_vision() {
    let (mut s, _volume) = board(9);
    let in_graveyard = twinned_vision_in_graveyard(&mut s, P0);
    twinned_vision_in_hand(&mut s);
    s.add_land_to_hand(P0, "Plains");
    let prompt = flashback_cost(s, in_graveyard, Phase::Upkeep);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("HR2 cost scores {scores:?}");
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// HR3, guard: the hand credit lifts Twinned Vision above a plain card, not a
/// surplus land above either. Volume's loot at nine lands, a hand of a plain
/// cantrip ("Draw a card.", {1}{U} instant), Twinned Vision and an Island:
/// the land SHALL be given up.
#[test]
fn hr3_guard_a_cantrip_and_twinned_vision_with_a_surplus_land_give_up_the_land() {
    let (mut s, volume) = board(9);
    s.add_spell_to_hand_from_oracle(P0, "Plain Cantrip", true, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        });
    twinned_vision_in_hand(&mut s);
    s.add_land_to_hand(P0, "Island");
    let prompt = volume_loot_cost(s, volume);

    let session = Arc::new(planned_session());
    let scores = named_scores(&prompt, &session);
    eprintln!("HR3 cost scores {scores:?}");
    let pick = chosen(&prompt, &session);
    assert!(is_land(&pick), "discarded {pick}, scores {scores:?}");
}

/// HC1: Twinned Vision in hand at nine Islands reads its flashback's card
/// beside its hand card: one card above a plain sorcery in hand, at both
/// leaves. HC2, guard: at one Island and no land in hand the flashback is out
/// of reach and the two read the same.
#[test]
fn hc1_hc2_a_flashback_card_in_hand_carries_its_graveyard_cast_in_reach() {
    for (lands, cards) in [(9, 1.0), (1, 0.0)] {
        let with = {
            let mut s = table(lands, 2);
            twinned_vision_in_hand(&mut s);
            plain_sorcery_in_hand(&mut s, P0);
            late(s)
        };
        let without = {
            let mut s = table(lands, 2);
            plain_sorcery_in_hand(&mut s, P0);
            plain_sorcery_in_hand(&mut s, P0);
            late(s)
        };
        assert_gap(
            &format!("HC1/HC2 at {lands} lands"),
            leaf_gaps(true, &with, &without),
            cards,
        );
    }
}

/// HC3: discarding a flashback card loses its hand card alone. Recursive
/// Recruitment in hand (with a Swamp among nine lands, its flashback in
/// reach) reads the same as the same card in the graveyard beside a plain
/// card of its mana cost in hand: holding it keeps both its casts, pitching
/// it keeps the graveyard cast and loses one hand card.
#[test]
fn hc3_discarding_a_flashback_card_loses_only_its_hand_card() {
    let build = |zone: &str| {
        let mut s = table(8, 2);
        s.add_basic_land(P0, ManaColor::Black);
        recursive_recruitment_in(&mut s, P0, zone);
        if zone == "hand" {
            // The same graveyard size in both states (graveyard_base).
            plain_sorcery_in_graveyard(&mut s, P0);
        } else {
            s.add_spell_to_hand(P0, "Plain Sorcery", false)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
                    generic: 3,
                });
        }
        late(s)
    };
    assert_gap(
        "HC3",
        leaf_gaps(true, &build("hand"), &build("graveyard")),
        0.0,
    );
}

/// HC4: the opponent's hand is read for a card P0 knows (CR 701.20a, a
/// revealed card), not for one it does not: Twinned Vision revealed in P1's
/// hand at nine of P1's Islands is one card less for P0 than a revealed
/// plain sorcery; unrevealed, the same.
#[test]
fn hc4_the_opponents_known_hand_card_is_credited_and_an_unknown_one_is_not() {
    for (revealed, cards) in [(true, -1.0), (false, 0.0)] {
        let build = |flashback: bool| {
            let mut s = table(4, 9);
            let id = if flashback {
                s.add_spell_to_hand_from_oracle(P1, "Twinned Vision", true, "")
                    .with_mana_cost(twinned_vision_cost())
                    .from_oracle_text_with_keywords(&["Flashback"], TWINNED_VISION)
                    .id()
            } else {
                s.add_spell_to_hand(P1, "Plain Sorcery", false)
                    .with_mana_cost(twinned_vision_cost())
                    .id()
            };
            let mut state = late(s);
            if revealed {
                state.public_revealed_cards.insert(id);
            }
            state
        };
        assert_gap(
            &format!("HC4 revealed {revealed}"),
            leaf_gaps(true, &build(true), &build(false)),
            cards,
        );
    }
}
