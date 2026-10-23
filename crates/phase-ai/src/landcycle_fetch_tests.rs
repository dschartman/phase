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

// ---- Judge rows on 497a162b8 (JUDGE.md, fork-checks/497a162b8) ----
//
// Rulings: JR1 (S1, a land in hand is colour supply for the search) and JR2
// (B1, a hand card beyond the one-drop window still asks for its colour, at
// the cards-left-to-draw weight) are red rows; JR3 to JR5 are guards.

const ANY_LAND_AMULET: &str = "{1}, Sacrifice this artifact: Search your library for a land card, reveal it, put it into your hand, then shuffle.";
const VALE: &str =
    "This land enters tapped unless you control two or more other lands.\n{T}: Add {R} or {G}.";

fn jr_hand_creature(s: &mut GameScenario, name: &str, shards: Vec<ManaCostShard>, generic: u32) {
    s.add_creature_to_hand(P0, name, 3, 3)
        .with_mana_cost(cost(shards, generic));
}

/// Build a prompt from an explicit board, a library builder returning the ids
/// to mark basic, and a hand/source setup.
fn jr_prompt(
    board: &[ManaColor],
    lib: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
    setup: impl FnOnce(&mut GameScenario) -> ObjectId,
    pool: Option<Vec<DeckEntry>>,
) -> GameState {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for c in board {
        s.add_basic_land(P0, *c);
    }
    for _ in 0..10 {
        s.add_land_to_library_top(P1, "Plains");
    }
    let basics = lib(&mut s);
    let source = setup(&mut s);
    let mut st = s.build().state().clone();
    st.phase = Phase::PreCombatMain;
    make_basic(&mut st, &basics);
    if let Some(entries) = pool {
        set_deck_pool(&mut st, entries);
    }
    to_search(st, source)
}

fn jr_basics(s: &mut GameScenario, names: &[&str]) -> Vec<ObjectId> {
    names
        .iter()
        .map(|n| s.add_land_to_library_top(P0, n).with_subtypes(vec![n]).id())
        .collect()
}

/// JR1 (focus 4): a Forest already in hand meets the {G} of a castable green
/// card next turn; the {U} card has no source anywhere. A basic-land search
/// with Forest listed first should take the Island.
#[test]
fn jr1_forest_in_hand_search_takes_the_missing_island() {
    let st = jr_prompt(
        &[ManaColor::Black, ManaColor::Black, ManaColor::Black],
        |s| jr_basics(s, &["Forest", "Island", "Swamp"]),
        |s| {
            s.add_land_to_hand(P0, "Forest")
                .with_subtypes(vec!["Forest"]);
            jr_hand_creature(s, "Green Bear", vec![ManaCostShard::Green], 3);
            jr_hand_creature(s, "Blue Bear", vec![ManaCostShard::Blue], 3);
            add_amulet(s)
        },
        None,
    );
    assert_eq!(fetched(&st), "Island");
}

/// JR2 (focus 3, spec Done-when 1's GIVEN): no green source in play, a green
/// card in hand (Mind Meanderer, MV 6 at three lands, so outside one land
/// drop), and no green card left to draw (deck list: its only green card is
/// the one in hand). Island listed first. The Forest should be fetched.
#[test]
fn jr2_green_card_in_hand_beyond_one_drop_still_asks_for_the_forest() {
    let pool = vec![
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
            1,
        ),
        entry(
            "Extended Absence",
            cost(vec![ManaCostShard::Black], 3),
            CoreType::Instant,
            3,
        ),
        entry(
            "Vinelasher Adept",
            cost(vec![ManaCostShard::Green, ManaCostShard::Green], 4),
            CoreType::Creature,
            1,
        ),
        entry("Island", ManaCost::NoCost, CoreType::Land, 7),
        entry("Swamp", ManaCost::NoCost, CoreType::Land, 7),
        entry("Forest", ManaCost::NoCost, CoreType::Land, 2),
    ];
    let st = jr_prompt(
        &[ManaColor::Blue, ManaColor::Black, ManaColor::Black],
        |s| jr_basics(s, &["Island", "Forest", "Swamp"]),
        |s| {
            jr_hand_creature(
                s,
                "Mind Meanderer",
                vec![
                    ManaCostShard::Green,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                ],
                3,
            );
            add_vinelasher(s)
        },
        Some(pool),
    );
    assert_eq!(fetched(&st), "Forest");
}

/// JR3 (focus 5): "search for a land card" with Island, Forest and a typed
/// nonbasic Forest Island dual offered (dual listed last); hand needs {G} and
/// {U}, three Swamps in play. The dual covers both and should be fetched.
#[test]
fn jr3_any_land_search_takes_the_dual_for_two_missing_colours() {
    let st = jr_prompt(
        &[ManaColor::Black, ManaColor::Black, ManaColor::Black],
        |s| {
            let mut ids = jr_basics(s, &["Island", "Forest"]);
            s.add_land_to_library_top(P0, "Watery Grove")
                .with_subtypes(vec!["Forest", "Island"]);
            ids.push(
                s.add_land_to_library_top(P0, "Swamp")
                    .with_subtypes(vec!["Swamp"])
                    .id(),
            );
            ids
        },
        |s| {
            jr_hand_creature(s, "Green Bear", vec![ManaCostShard::Green], 3);
            jr_hand_creature(s, "Blue Bear", vec![ManaCostShard::Blue], 3);
            s.add_artifact_from_oracle(P0, "Land Amulet", ANY_LAND_AMULET)
                .with_mana_cost(cost(Vec::new(), 1))
                .id()
        },
        None,
    );
    assert_eq!(fetched(&st), "Watery Grove");
}

/// JR4 (focus 5): "search for a land card" offering Island first, then
/// Rockfall Vale (nonbasic, {T}: Add {R} or {G}, colours from its ability),
/// Forest. Hand needs {G}; three Swamps. Anything but the Island.
#[test]
fn jr4_any_land_search_prices_rockfall_vale_by_its_ability_colours() {
    let st = jr_prompt(
        &[ManaColor::Black, ManaColor::Black, ManaColor::Black],
        |s| {
            let mut ids = jr_basics(s, &["Island"]);
            s.add_land_to_library_top(P0, "Rockfall Vale")
                .from_oracle_text(VALE);
            ids.extend(jr_basics(s, &["Forest"]));
            ids
        },
        |s| {
            jr_hand_creature(s, "Green Bear", vec![ManaCostShard::Green], 3);
            s.add_artifact_from_oracle(P0, "Land Amulet", ANY_LAND_AMULET)
                .with_mana_cost(cost(Vec::new(), 1))
                .id()
        },
        None,
    );
    let pick = fetched(&st);
    assert!(pick == "Rockfall Vale" || pick == "Forest", "got {pick}");
}

/// JR5 (focus 3): a land the hand needs now against a land two off-colour
/// library cards need. Hand needs {G} (castable next drop); the deck list's
/// cards left to draw need {W} and {R}, neither in play. Offered (any land):
/// a typed Mountain Plains dual first, then Forest. The Forest (0.12) should
/// outrank the dual (2 x 0.04).
#[test]
fn jr5_hand_need_now_outranks_two_library_colours() {
    let pool = vec![
        entry(
            "White Card",
            cost(vec![ManaCostShard::White], 2),
            CoreType::Creature,
            2,
        ),
        entry(
            "Red Card",
            cost(vec![ManaCostShard::Red], 2),
            CoreType::Creature,
            2,
        ),
        entry("Swamp", ManaCost::NoCost, CoreType::Land, 7),
    ];
    let st = jr_prompt(
        &[ManaColor::Black, ManaColor::Black, ManaColor::Black],
        |s| {
            s.add_land_to_library_top(P0, "Sunlit Ridge")
                .with_subtypes(vec!["Mountain", "Plains"]);
            jr_basics(s, &["Forest"])
        },
        |s| {
            jr_hand_creature(s, "Green Bear", vec![ManaCostShard::Green], 3);
            s.add_artifact_from_oracle(P0, "Land Amulet", ANY_LAND_AMULET)
                .with_mana_cost(cost(Vec::new(), 1))
                .id()
        },
        Some(pool),
    );
    assert_eq!(fetched(&st), "Forest");
}
