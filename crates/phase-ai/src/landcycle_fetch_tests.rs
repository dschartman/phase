//! A basic-land search fetches the basic whose colour the AI cannot yet make
//! (`simforge/docs/spec-ai-landcycle-fetch-by-colour-2026-10-22.md`; the v2
//! audit's seed 1000 T12, finding 1).
//!
//! At a `SearchChoice` the AI ranks every issued selection with
//! `policies::tutor::score_search_choice_selection`, and a land's term there
//! read only "is the AI short of mana", never colour: every basic tied, and
//! the stable sort kept the first one the engine listed. In the audit P0
//! landcycled Vinelasher Adept with three Islands and three Swamps in play,
//! took an Island over two Forests, and lost with Wrecking Gecko and Mind
//! Meanderer uncastable in hand. These rows read the production chooser
//! (`choose_action_with_session`) at the search prompt the activation reaches.
//!
//! Card text verified against MTGJSON AtomicCards (`data/mtgjson`) and the
//! fork's `client/public/card-data.json`.

use std::sync::Arc;

use engine::game::deck_loading::DeckEntry;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
use crate::session::AiSession;
use crate::test_support::default_deck_plan;

/// Vinelasher Adept ({4}{G}{G}, 2/4), MTGJSON AtomicCards.
const VINELASHER: &str = "Reach\nWhen this creature enters, put three +1/+1 counters on target creature.\nBasic landcycling {2} ({2}, Discard this card: Search your library for a basic land card, reveal it, put it into your hand, then shuffle.)";
/// Undulating Witness ({4}{U}, 3/4), MTGJSON AtomicCards.
const WITNESS: &str = "Flying\n{2}: This creature gets +1/-1 until end of turn.\nBasic landcycling {2} ({2}, Discard this card: Search your library for a basic land card, reveal it, put it into your hand, then shuffle.)";
/// Traveler's Amulet ({1} artifact), MTGJSON AtomicCards.
const AMULET: &str = "{1}, Sacrifice this artifact: Search your library for a basic land card, reveal it, put it into your hand, then shuffle.";
/// Wrecking Gecko ({4}{G}, 5/5), MTGJSON AtomicCards.
const GECKO: &str = "Ward {2} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {2}.)\n{6}{G}{G}: This creature gets +4/+4 and gains trample until end of turn.";
/// Extended Absence ({3}{B} instant), MTGJSON AtomicCards.
const EXTENDED_ABSENCE: &str = "Exile target creature or planeswalker. Extended Absence deals 1 damage to each opponent and you gain 1 life.";

/// Accurate mode as the FRA runs play it: VeryHard, temperature 0.
fn accurate() -> AiConfig {
    let mut config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    config.temperature = 0.0;
    config
}

fn planned_session() -> Arc<AiSession> {
    let mut session = AiSession::default();
    session.plan.insert(P0, default_deck_plan());
    session.plan.insert(P1, default_deck_plan());
    Arc::new(session)
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// The seed 1000 T12 board: three Islands and three Swamps, no green source.
fn audit_board(s: &mut GameScenario) {
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Black);
    }
}

/// P0's library, in creation order, so the first name has the lowest
/// `ObjectId` and is the first selection the engine lists (the audit's
/// Island #3 before Forest #4). Each is a basic land with its land subtype
/// (CR 205.3i, CR 205.4a). P1 gets a stocked library so no line is a deck-out.
fn library(s: &mut GameScenario, names: &[&str]) -> Vec<ObjectId> {
    for _ in 0..10 {
        s.add_land_to_library_top(P1, "Plains");
    }
    names
        .iter()
        .map(|name| {
            s.add_land_to_library_top(P0, name)
                .with_subtypes(vec![name])
                .id()
        })
        .collect()
}

fn make_basic(state: &mut GameState, ids: &[ObjectId]) {
    for id in ids {
        let obj = state.objects.get_mut(id).expect("library land");
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.base_card_types = obj.card_types.clone();
    }
}

fn add_gecko(s: &mut GameScenario) {
    s.add_creature_to_hand_from_oracle(P0, "Wrecking Gecko", 5, 5, GECKO)
        .with_mana_cost(cost(vec![ManaCostShard::Green], 4));
}

fn add_extended_absence(s: &mut GameScenario) {
    s.add_spell_to_hand_from_oracle(P0, "Extended Absence", true, EXTENDED_ABSENCE)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 3));
}

fn add_vinelasher(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_hand_from_oracle(P0, "Vinelasher Adept", 2, 4, VINELASHER)
        .with_mana_cost(cost(vec![ManaCostShard::Green, ManaCostShard::Green], 4))
        .id()
}

fn add_witness(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_hand_from_oracle(P0, "Undulating Witness", 3, 4, WITNESS)
        .with_mana_cost(cost(vec![ManaCostShard::Blue], 4))
        .id()
}

fn add_amulet(s: &mut GameScenario) -> ObjectId {
    s.add_artifact_from_oracle(P0, "Traveler's Amulet", AMULET)
        .with_mana_cost(cost(Vec::new(), 1))
        .id()
}

/// A deck-list entry the AI's remaining-deck view reads (`deck_knowledge`).
fn entry(name: &str, mana_cost: ManaCost, core: CoreType, count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: name.to_string(),
            mana_cost,
            card_type: CardType {
                supertypes: Vec::new(),
                core_types: vec![core],
                subtypes: Vec::new(),
            },
            ..Default::default()
        },
        count,
    }
}

fn set_deck_pool(state: &mut GameState, entries: Vec<DeckEntry>) {
    let entries = Arc::new(entries);
    state.deck_pools.push(PlayerDeckPool {
        player: P0,
        registered_main: entries.clone(),
        current_main: entries,
        ..Default::default()
    });
}

/// The index of `source`'s library-search ability: the card's own ability
/// order, not a hard-coded slot.
fn search_ability_index(state: &GameState, source: ObjectId) -> usize {
    state.objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::SearchLibrary { .. }))
        .expect("a search-library ability")
}

/// Activate `source`'s search ability and pass until P0 is at the search.
fn to_search(state: GameState, source: ObjectId) -> GameState {
    let ability_index = search_ability_index(&state, source);
    let mut r = GameRunner::from_state(state);
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index,
    })
    .expect("activate the search");
    for _ in 0..8 {
        if matches!(r.state().waiting_for, WaitingFor::SearchChoice { .. }) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        matches!(r.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "reach-guard: search prompt, got {}",
        r.state().waiting_for.variant_name()
    );
    r.state().clone()
}

/// What the production chooser fetches at the prompt, by name.
fn fetched(state: &GameState) -> String {
    let mut rng = ChaCha20Rng::seed_from_u64(7);
    let action = crate::search::choose_action_with_session(
        state,
        P0,
        &accurate(),
        &mut rng,
        &planned_session(),
    )
    .expect("an action at the search prompt");
    match action {
        GameAction::SelectCards { cards } => cards
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect::<Vec<_>>()
            .join("+"),
        other => format!("{other:?}"),
    }
}

/// The prompt for `source` built by `setup` on the audit board, with P0's
/// library `names` in listed order.
fn prompt(
    names: &[&str],
    setup: impl FnOnce(&mut GameScenario) -> ObjectId,
    pool: Option<Vec<DeckEntry>>,
) -> GameState {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    audit_board(&mut s);
    let library_ids = library(&mut s, names);
    let source = setup(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::PreCombatMain;
    make_basic(&mut st, &library_ids);
    if let Some(entries) = pool {
        set_deck_pool(&mut st, entries);
    }
    to_search(st, source)
}

/// LF1, the spec's first Done-when: Vinelasher Adept's basic landcycling with
/// an Island listed before a Forest, no green source in play, and Wrecking
/// Gecko ({4}{G}) in hand within a land drop of castable. The hand needs a
/// green source the battlefield lacks; the Forest SHALL be fetched.
#[test]
fn lf1_vinelasher_landcycling_fetches_the_forest_the_hand_needs() {
    let st = prompt(
        &["Island", "Forest", "Swamp", "Forest"],
        |s| {
            add_gecko(s);
            add_vinelasher(s)
        },
        None,
    );
    assert_eq!(fetched(&st), "Forest");
}

/// LF2, the same class on another card: Undulating Witness's basic
/// landcycling (its second ability; the first is a pump), the same board and
/// hand. The demand is read from the hand, not from the cycling card; the
/// Forest SHALL be fetched.
#[test]
fn lf2_undulating_witness_landcycling_fetches_the_forest_the_hand_needs() {
    let st = prompt(
        &["Island", "Forest", "Swamp"],
        |s| {
            add_gecko(s);
            add_witness(s)
        },
        None,
    );
    assert_eq!(fetched(&st), "Forest");
}

/// LF3, a plain "search your library for a basic land card" that is not
/// cycling: Traveler's Amulet on the battlefield, the same board and hand.
/// The Forest SHALL be fetched.
#[test]
fn lf3_travelers_amulet_search_fetches_the_forest_the_hand_needs() {
    let st = prompt(
        &["Island", "Forest", "Swamp"],
        |s| {
            add_gecko(s);
            add_amulet(s)
        },
        None,
    );
    assert_eq!(fetched(&st), "Forest");
}

/// LF4, the audit's exact shape: at the search the hand holds only Extended
/// Absence (its {B} met), and the green cards are still in the library
/// (Wrecking Gecko, two Mind Meanderer), known from P0's deck list. The cards
/// left to draw need a green source the battlefield lacks; the Forest SHALL
/// be fetched.
#[test]
fn lf4_landcycling_fetches_the_forest_the_cards_left_to_draw_need() {
    let pool = vec![
        entry(
            "Wrecking Gecko",
            cost(vec![ManaCostShard::Green], 4),
            CoreType::Creature,
            1,
        ),
        entry(
            "Mind Meanderer",
            cost(
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                ],
                3,
            ),
            CoreType::Creature,
            2,
        ),
        entry(
            "Extended Absence",
            cost(vec![ManaCostShard::Black], 3),
            CoreType::Instant,
            2,
        ),
        entry("Island", ManaCost::NoCost, CoreType::Land, 7),
        entry("Swamp", ManaCost::NoCost, CoreType::Land, 7),
        entry("Forest", ManaCost::NoCost, CoreType::Land, 3),
    ];
    let st = prompt(
        &["Island", "Forest", "Swamp", "Forest"],
        |s| {
            add_extended_absence(s);
            add_vinelasher(s)
        },
        Some(pool),
    );
    assert_eq!(fetched(&st), "Forest");
}

/// LF5, guard, the spec's "no unmet demand": the same landcycling with a
/// Forest already in play, so Wrecking Gecko's {G} is met and no deck list
/// asks for more. Every basic scores alike; the existing choice, the first
/// basic listed (the Island), SHALL stand.
#[test]
fn lf5_guard_no_unmet_colour_keeps_the_first_listed_basic() {
    let st = prompt(
        &["Island", "Forest", "Swamp"],
        |s| {
            s.add_basic_land(P0, ManaColor::Green);
            add_gecko(s);
            add_vinelasher(s)
        },
        None,
    );
    assert_eq!(fetched(&st), "Island");
}

/// LF6, guard: nothing in hand or deck list needs a colour (the hand is a
/// colourless card), so the first basic listed SHALL stand whichever colour
/// it is; here the Forest is listed first and stays the pick over the
/// Island.
#[test]
fn lf6_guard_no_demand_keeps_the_first_listed_basic_whatever_its_colour() {
    let st = prompt(
        &["Forest", "Island", "Swamp"],
        |s| {
            s.add_creature_to_hand(P0, "Colourless Golem", 3, 3)
                .with_mana_cost(cost(Vec::new(), 3));
            add_vinelasher(s)
        },
        None,
    );
    assert_eq!(fetched(&st), "Forest");
}
