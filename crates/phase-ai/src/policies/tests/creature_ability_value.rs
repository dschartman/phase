//! `card_value::intrinsic_value` for creatures: the body plus what its
//! abilities do, read from the engine's parse of the card.
//!
//! Report (`fork-checks/96d0bfd2a/JUDGE.md`, S4 and S5): the scalar priced
//! every creature on its body and pips alone, so the keep order gave up
//! Liliana the Repentant (6.0) and Fatehold Chronologist (4.5) before a
//! cantrip (Twinned Vision, 13.0 at a library mean of 12.0) at 1029 T21 and
//! 1018 T14, with eleven cards in Liliana's graveyard and her exhaust in
//! reach. What the scalar counted before this change is the step 0 table in
//! `deck-builder/sims/fra-phase/2026-10-20-creature-abilities/NOTE.md`: for
//! a creature, `1.5 * power + toughness` and half a point a pip, nothing else.
//!
//! The cards are real; their Oracle text is the MTGJSON text
//! (`client/public/card-data.json`), parsed through the shipped parser:
//!
//! ```text
//! Liliana the Repentant  {1}{B}  Legendary Creature — Human Warlock 2/2
//!   Whenever another creature or planeswalker you control enters, mill two cards.
//!   Exhaust — {5}{B}: Return target creature or planeswalker card from your
//!   graveyard to the battlefield. Put a +1/+1 counter on Liliana. Activate only
//!   as a sorcery. (Activate each exhaust ability only once.)
//! Fatehold Chronologist  {1}{W/U}  Creature — Bird Wizard 1/2
//!   Flying
//!   This creature enters prepared.
//!   Prepare spell: Peer Review {2}{W/U} Sorcery
//!   Create a 2/2 colorless Wizard Soldier creature token named Cadet. Surveil 1.
//! Archive Arbiter        {6}  Creature — Sphinx 4/4
//!   Flying
//!   When this creature enters, choose one —
//!   • Destroy target noncreature, nonland permanent.
//!   • You gain 4 life.
//! Mind Meanderer         {3}{G}{U}{U}  Creature — Elemental 4/4
//!   Flying
//!   When this creature enters, it fights up to one target creature an opponent
//!   controls.
//! Twinned Vision         {1}{U/R}  Instant
//!   Draw a card. If this spell wasn't cast from your hand, draw two cards
//!   instead. Flashback—{1}{U/R}{U/R}, Discard a card.
//! ```
//!
//! The opposing creatures, the graveyard bodies and the opposing planeswalker
//! are made-up vanilla shapes priced by the formula in each row.

use engine::game::effects::prepare::prepare_object;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::zones::Zone;

use crate::card_value::{given_up_cards, intrinsic_value, permanent_card_credit};

const LILIANA_TEXT: &str = "Whenever another creature or planeswalker you control enters, mill \
    two cards.\nExhaust — {5}{B}: Return target creature or planeswalker card from your graveyard \
    to the battlefield. Put a +1/+1 counter on Liliana. Activate only as a sorcery. (Activate \
    each exhaust ability only once.)";
const TWINNED_VISION_TEXT: &str = "Draw a card. If this spell wasn't cast from your hand, draw \
    two cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from \
    your graveyard for its flashback cost. Then exile it.)";
const CHRONOLOGIST_TEXT: &str = "Flying\nThis creature enters prepared. (While it's prepared, \
    you may cast a copy of its spell. Doing so unprepares it.)";
const PEER_REVIEW_TEXT: &str =
    "Create a 2/2 colorless Wizard Soldier creature token named Cadet. Surveil 1.";
const ARBITER_TEXT: &str = "Flying\nWhen this creature enters, choose one —\n• Destroy target \
    noncreature, nonland permanent.\n• You gain 4 life.";
const MEANDERER_TEXT: &str = "Flying\nThis creature has vigilance as long as you control a Jace \
    planeswalker.\nWhen this creature enters, it fights up to one target creature an opponent \
    controls. (Each deals damage equal to its power to the other.)";

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// P0's board: `lands` on the battlefield, `hand_lands` Swamps in hand and a
/// library of five vanilla 4/4s for {4} (12.0 each: 1.5 * 4 + 4 + 0.5 * 4),
/// so one expected draw is 12.0 and Twinned Vision is 13.0 (two pips and one
/// draw). P1 has ten Plains in its library.
struct Board {
    scenario: GameScenario,
}

impl Board {
    fn new(lands: &[ManaColor], hand_lands: usize) -> Self {
        let mut scenario = GameScenario::new();
        for &colour in lands {
            scenario.add_basic_land(P0, colour);
        }
        for i in 0..hand_lands {
            scenario
                .add_land_to_hand(P0, &format!("Swamp {i}"))
                .with_subtypes(vec!["Swamp"]);
        }
        for _ in 0..10 {
            scenario.add_land_to_library_top(P1, "Plains");
        }
        Self { scenario }
    }

    fn twinned_vision(&mut self) -> ObjectId {
        self.scenario
            .add_spell_to_hand_from_oracle(P0, "Twinned Vision", true, "")
            .with_mana_cost(cost(&[ManaCostShard::BlueRed], 1))
            .from_oracle_text_with_keywords(&["Flashback"], TWINNED_VISION_TEXT)
            .id()
    }

    fn liliana_in_hand(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_hand_from_oracle(P0, "Liliana the Repentant", 2, 2, LILIANA_TEXT)
            .with_mana_cost(cost(&[ManaCostShard::Black], 1))
            .id()
    }

    fn liliana_in_play(&mut self) -> ObjectId {
        self.scenario
            .add_creature_from_oracle(P0, "Liliana the Repentant", 2, 2, LILIANA_TEXT)
            .with_mana_cost(cost(&[ManaCostShard::Black], 1))
            .id()
    }

    /// A vanilla 4/4 for {4} in P0's graveyard: 12.0 on its body.
    fn graveyard_beast(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_graveyard(P0, "Graveyard Beast", 4, 4)
            .with_mana_cost(cost(&[], 4))
            .id()
    }

    fn chronologist(&mut self, zone: Zone) -> ObjectId {
        let mut builder = match zone {
            Zone::Hand => self.scenario.add_creature_to_hand_from_oracle(
                P0,
                "Fatehold Chronologist",
                1,
                2,
                CHRONOLOGIST_TEXT,
            ),
            _ => self.scenario.add_creature_from_oracle(
                P0,
                "Fatehold Chronologist",
                1,
                2,
                CHRONOLOGIST_TEXT,
            ),
        };
        builder.with_mana_cost(cost(&[ManaCostShard::WhiteBlue], 1));
        builder.id()
    }

    fn arbiter(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_hand_from_oracle(P0, "Archive Arbiter", 4, 4, ARBITER_TEXT)
            .with_mana_cost(cost(&[], 6))
            .id()
    }

    fn meanderer(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_hand_from_oracle(P0, "Mind Meanderer", 4, 4, MEANDERER_TEXT)
            .with_mana_cost(cost(
                &[
                    ManaCostShard::Green,
                    ManaCostShard::Blue,
                    ManaCostShard::Blue,
                ],
                3,
            ))
            .id()
    }

    fn opposing_creature(&mut self, power: i32, toughness: i32, generic: u32) -> ObjectId {
        self.scenario
            .add_creature(P1, "Opposing Creature", power, toughness)
            .with_mana_cost(cost(&[], generic))
            .id()
    }

    fn state(self) -> GameState {
        let mut state = self.scenario.build().state().clone();
        for _ in 0..5 {
            let card_id = CardId(state.next_object_id);
            let id = create_object(
                &mut state,
                card_id,
                P0,
                "Library Beast".to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.mana_cost = cost(&[], 4);
        }
        state
    }
}

/// Peer Review as Fatehold Chronologist's prepare spell (CR 722.2a), parsed
/// from its Oracle text through the shipped parser.
fn give_peer_review(state: &mut GameState, id: ObjectId) {
    let parsed = parse_oracle_text(
        PEER_REVIEW_TEXT,
        "Peer Review",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        !parsed.abilities.is_empty(),
        "Peer Review parses a spell ability"
    );
    state.objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
        name: "Peer Review".to_string(),
        card_types: CardType {
            core_types: vec![CoreType::Sorcery],
            ..CardType::default()
        },
        mana_cost: cost(&[ManaCostShard::WhiteBlue], 2),
        abilities: parsed.abilities,
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });
}

fn first_given_up(state: &GameState, pool: &[ObjectId]) -> ObjectId {
    given_up_cards(state, pool, 1, None)[0]
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{what}: {actual}, expected {expected}"
    );
}

// --- Liliana the Repentant: the exhaust by its best graveyard target ------

/// CA1 (Done-when 2, the 1029 T21 shape): Twinned Vision and Liliana in
/// hand, a 4/4 (12.0) in the graveyard, three Swamps and an Island in play
/// and two Swamps in hand, so {5}{B} is within two land drops. Liliana is
/// her body and pips (6.0) plus the exhaust's best target (12.0), 18.0, over
/// Twinned Vision's 13.0: Twinned Vision is discarded.
#[test]
fn ca1_liliana_with_a_target_and_her_exhaust_in_reach_is_kept_over_a_cantrip() {
    let mut board = Board::new(
        &[
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Blue,
        ],
        2,
    );
    let vision = board.twinned_vision();
    let liliana = board.liliana_in_hand();
    board.graveyard_beast();
    let state = board.state();
    assert_close(intrinsic_value(&state, vision), 13.0, "Twinned Vision");
    assert_close(intrinsic_value(&state, liliana), 18.0, "Liliana");
    assert_eq!(first_given_up(&state, &[liliana, vision]), vision);
}

/// CA2 (Done-when 2, the guard): the same board with no creature card in the
/// graveyard. The exhaust returns nothing, so it adds nothing: Liliana is
/// 6.0 and is given up before Twinned Vision.
#[test]
fn ca2_an_empty_graveyard_adds_nothing_for_the_exhaust() {
    let mut board = Board::new(
        &[
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Blue,
        ],
        2,
    );
    let vision = board.twinned_vision();
    let liliana = board.liliana_in_hand();
    let state = board.state();
    assert_close(intrinsic_value(&state, liliana), 6.0, "Liliana");
    assert_eq!(first_given_up(&state, &[liliana, vision]), liliana);
}

/// CA3: a graveyard holding only an instant: the exhaust returns a creature
/// or planeswalker card, so it adds nothing.
#[test]
fn ca3_a_noncreature_graveyard_card_is_not_a_target() {
    let mut board = Board::new(
        &[
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Blue,
        ],
        2,
    );
    let liliana = board.liliana_in_hand();
    board
        .scenario
        .add_spell_to_graveyard(P0, "Spent Instant", true)
        .with_mana_cost(cost(&[], 4));
    let state = board.state();
    assert_close(intrinsic_value(&state, liliana), 6.0, "Liliana");
}

/// CA4: the exhaust's mana out of reach, three ways. Three Islands and no
/// land in hand (six mana is three short); six Islands (no black source for
/// the {B} pip); three Swamps in play and four in hand (one land a turn,
/// CR 305.2: two drops reach five). Each prices the exhaust at half its
/// target, 6.0 + 0.5 * 12.0 = 12.0, under Twinned Vision.
#[test]
fn ca4_an_exhaust_out_of_reach_is_discounted() {
    let boards: [(&[ManaColor], usize); 3] = [
        (&[ManaColor::Blue; 3], 0),
        (&[ManaColor::Blue; 6], 0),
        (&[ManaColor::Black; 3], 4),
    ];
    for (lands, hand_lands) in boards {
        let mut board = Board::new(lands, hand_lands);
        let liliana = board.liliana_in_hand();
        board.graveyard_beast();
        let state = board.state();
        assert_close(
            intrinsic_value(&state, liliana),
            12.0,
            &format!("Liliana with {} lands, {hand_lands} in hand", lands.len()),
        );
    }
}

/// CA5: Liliana on the battlefield. Her exhaust once spent (CR 702.177a)
/// returns nothing more, and with no other reanimation her mill trigger is
/// a cost: two cards at half a point, 6.0 - 1.0 = 5.0. While the exhaust is
/// live the mill is fuel for it (no cost), and with an empty graveyard she is
/// 6.0. The leaf credit is unchanged: a creature earns none, the board sum
/// credits its body.
#[test]
fn ca5_a_spent_exhaust_leaves_the_mill_as_a_cost() {
    let mut board = Board::new(&[ManaColor::Black; 6], 0);
    let liliana = board.liliana_in_play();
    let mut state = board.state();
    assert_close(intrinsic_value(&state, liliana), 6.0, "live exhaust");
    assert_eq!(permanent_card_credit(&state, &state.objects[&liliana]), 0.0);
    state.activated_abilities_this_game.insert((liliana, 0), 1);
    assert_close(intrinsic_value(&state, liliana), 5.0, "spent exhaust");
}

// --- Fatehold Chronologist: its prepare spell ------------------------------

/// CB1 (Done-when 3): Chronologist prepared on the battlefield (CR 722.3c:
/// its controller may cast the copy of Peer Review). Its value includes Peer
/// Review's payoff, the 2/2 Cadet (5.0): 4.5 + 5.0 = 9.5.
#[test]
fn cb1_a_prepared_chronologist_counts_its_prepare_spell() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let chronologist = board.chronologist(Zone::Battlefield);
    let mut state = board.state();
    give_peer_review(&mut state, chronologist);
    let mut events = Vec::new();
    prepare_object(&mut state, chronologist, &mut events);
    assert!(state.objects[&chronologist].prepared.is_some());
    assert_close(intrinsic_value(&state, chronologist), 9.5, "prepared");
}

/// CB2 (Done-when 3): the same Chronologist unprepared (it has cast its copy,
/// CR 722.3c). Its prepare spell is not available: 4.5, the body alone.
#[test]
fn cb2_an_unprepared_chronologist_is_its_body() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let chronologist = board.chronologist(Zone::Battlefield);
    let mut state = board.state();
    give_peer_review(&mut state, chronologist);
    assert!(state.objects[&chronologist].prepared.is_none());
    assert_close(intrinsic_value(&state, chronologist), 4.5, "unprepared");
}

/// CB3 (the 1010 T5 and 1018 T14 shape): Chronologist in hand enters
/// prepared, so as a card it is worth its prepare spell too: 9.5.
#[test]
fn cb3_a_chronologist_in_hand_enters_prepared() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let chronologist = board.chronologist(Zone::Hand);
    let mut state = board.state();
    give_peer_review(&mut state, chronologist);
    assert!(
        !state.objects[&chronologist]
            .replacement_definitions
            .is_empty(),
        "the fixture parses 'enters prepared'"
    );
    assert_close(intrinsic_value(&state, chronologist), 9.5, "in hand");
}

// --- Archive Arbiter: the enters modes on the current board ----------------

/// CC1 (Done-when 4): no opposing noncreature, nonland permanent (only a
/// creature and a land). The destroy mode has nothing to remove, so the
/// enters value is the life mode's: 4 life at 0.15 of a card a point, at one
/// expected draw of 12.0, 7.2. Arbiter: 13.0 + 7.2 = 20.2.
#[test]
fn cc1_arbiter_with_no_noncreature_target_is_worth_the_life_mode() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let arbiter = board.arbiter();
    board.opposing_creature(3, 3, 3);
    board.scenario.add_basic_land(P1, ManaColor::White);
    let state = board.state();
    assert_close(
        intrinsic_value(&state, arbiter),
        13.0 + 4.0 * 0.15 * 12.0,
        "Arbiter",
    );
}

/// CC2 (Done-when 4): an opposing planeswalker, loyalty 9, {2}{W}{W} (11.0:
/// two pips' worth of four and her loyalty). The destroy mode removes it, over
/// the life mode's 7.2: Arbiter is 13.0 + 11.0 = 24.0.
#[test]
fn cc2_arbiter_destroys_the_noncreature_permanent_it_removes() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let arbiter = board.arbiter();
    let walker = board
        .scenario
        .add_planeswalker_from_oracle(P1, "Opposing Walker", "Gideon", 9, "")
        .with_mana_cost(cost(&[ManaCostShard::White, ManaCostShard::White], 2))
        .id();
    let state = board.state();
    assert_close(intrinsic_value(&state, walker), 11.0, "the walker");
    assert_close(intrinsic_value(&state, arbiter), 24.0, "Arbiter");
}

// --- Mind Meanderer: the fight by its exchange -----------------------------

/// CD1 (Done-when 4): the only opposing creature is a 5/5, which the fight
/// does not kill and which kills Meanderer (CR 701.14a: each deals damage
/// equal to its power to the other). The fight is "up to one", so it is
/// declined: no kill is credited, Meanderer is its body, 13.0.
#[test]
fn cd1_a_fight_that_loses_credits_no_kill() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let meanderer = board.meanderer();
    board.opposing_creature(5, 5, 5);
    let state = board.state();
    assert_close(intrinsic_value(&state, meanderer), 13.0, "Meanderer");
}

/// CD2: a 2/2 for {2} (6.0) beside the 5/5: Meanderer kills it and survives,
/// so the fight is worth the 2/2: 13.0 + 6.0 = 19.0.
#[test]
fn cd2_a_fight_that_wins_is_worth_the_creature_it_kills() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let meanderer = board.meanderer();
    board.opposing_creature(5, 5, 5);
    board.opposing_creature(2, 2, 2);
    let state = board.state();
    assert_close(intrinsic_value(&state, meanderer), 19.0, "Meanderer");
}

/// CD3: a 4/4 for {4} (12.0) alone: both die, and the kill (12.0) is worth
/// less than Meanderer's own body (13.0), so the trade is declined: 13.0.
#[test]
fn cd3_a_trade_down_is_declined() {
    let mut board = Board::new(&[ManaColor::Blue; 3], 0);
    let meanderer = board.meanderer();
    board.opposing_creature(4, 4, 4);
    let state = board.state();
    assert_close(intrinsic_value(&state, meanderer), 13.0, "Meanderer");
}
