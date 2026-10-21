//! `card_value::intrinsic_value` for noncreature cards: what the card does,
//! read from the engine's parse of its Oracle text, not half a point a pip.
//!
//! Report (FRA sealed, 2026-10-17; `fork-checks/ef45b56b6/JUDGE.md`): the
//! scalar priced every noncreature spell at 0.5 a pip and a planeswalker at
//! nothing more, so The Theorist sat at 2.0 under a basic land's 3.0 and the
//! keep order, the discard-cost price and the loot veto all gave removal and
//! card draw away first (`fork-checks/9f72018f4/OPEN.md`: about 20 of the 29
//! one-card loot and flashback pitches were noncreature spells priced 1.0 to
//! 2.0 against an expected draw of 3.5 to 4.5).
//!
//! The cards are real; their Oracle text is the MTGJSON text, parsed through
//! the shipped parser:
//!
//! ```text
//! The Theorist, Jace Beleren  {2}{U}{U}  Legendary Planeswalker — Jace, loyalty 3
//!   At the beginning of each opponent's draw step, you draw a card.
//!   [+1]: Create a 1/1 blue Illusion creature token.
//!   [−2]: For each opponent, return up to one target artifact or creature that
//!   player controls to its owner's hand.
//!   [−6]: Draw three cards. Then put X +1/+1 counters on each creature you
//!   control, where X is the number of cards in your hand.
//! Last Gasp             {1}{B}  Instant  Target creature gets -3/-3 until end of turn.
//! Extended Absence      {3}{B}  Instant  Exile target creature or planeswalker. Extended
//!                                        Absence deals 1 damage to each opponent and you gain 1 life.
//! Break Under Pressure  {2}{B}  Instant  Target opponent sacrifices a creature or
//!                                        planeswalker with the greatest mana value among
//!                                        creatures and planeswalkers they control. You gain 2 life.
//! Multiply by Zero      {1}{B}  Instant  Target creature has base power and toughness 0/0
//!                                        until end of turn.
//! Memory Trap           {2}{W}  Enchantment  When this enchantment enters, exile target
//!                                        nonland permanent an opponent controls until this
//!                                        enchantment leaves the battlefield.
//! Infinite Coursework   {2}{U}  Enchantment — Aura  Enchant creature / When this Aura
//!                                        enters, tap enchanted creature. It becomes unprepared. /
//!                                        Enchanted creature loses all abilities and doesn't untap
//!                                        during its controller's untap step.
//! Icy Reception         {1}{U}  Instant  Choose one — Counter target creature or legendary
//!                                        spell unless its controller pays {3}. / Target creature
//!                                        gets -5/-0 until end of turn.
//! Twinned Vision        {1}{U/R} Instant  Draw a card. If this spell wasn't cast from your
//!                                        hand, draw two cards instead. / Flashback—{1}{U/R}{U/R},
//!                                        Discard a card.
//! ```
//!
//! Divination, Catalog, Giant Growth and the indestructible instant are the
//! classic shapes of their class (draw two; draw two then discard one; a pump;
//! a protection spell).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};

use crate::card_value::{expected_draw_value, given_up_cards, intrinsic_value};

const THEORIST_TEXT: &str = "At the beginning of each opponent's draw step, you draw a card.\n\
    [+1]: Create a 1/1 blue Illusion creature token.\n\
    [−2]: For each opponent, return up to one target artifact or creature that player controls \
    to its owner's hand.\n\
    [−6]: Draw three cards. Then put X +1/+1 counters on each creature you control, where X is \
    the number of cards in your hand.";
const LAST_GASP_TEXT: &str = "Target creature gets -3/-3 until end of turn.";
const EXTENDED_ABSENCE_TEXT: &str = "Exile target creature or planeswalker. Extended Absence \
    deals 1 damage to each opponent and you gain 1 life.";
const BREAK_UNDER_PRESSURE_TEXT: &str = "Target opponent sacrifices a creature or planeswalker \
    with the greatest mana value among creatures and planeswalkers they control. You gain 2 life.";
const MULTIPLY_BY_ZERO_TEXT: &str =
    "Target creature has base power and toughness 0/0 until end of turn.";
const MEMORY_TRAP_TEXT: &str = "When this enchantment enters, exile target nonland permanent an \
    opponent controls until this enchantment leaves the battlefield.";
const INFINITE_COURSEWORK_TEXT: &str = "Enchant creature\nWhen this Aura enters, tap enchanted \
    creature. It becomes unprepared.\nEnchanted creature loses all abilities and doesn't untap \
    during its controller's untap step.";
const ICY_RECEPTION_TEXT: &str = "Choose one —\n• Counter target creature or legendary spell \
    unless its controller pays {3}.\n• Target creature gets -5/-0 until end of turn.";
const TWINNED_VISION_TEXT: &str = "Draw a card. If this spell wasn't cast from your hand, draw \
    two cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from \
    your graveyard for its flashback cost. Then exile it.)";
const DIVINATION_TEXT: &str = "Draw two cards.";
const CATALOG_TEXT: &str = "Draw two cards, then discard a card.";
const GIANT_GROWTH_TEXT: &str = "Target creature gets +3/+3 until end of turn.";
const PROTECTION_TEXT: &str = "Target creature you control gains indestructible until end of turn.";

fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

/// What the opponent has on the battlefield.
#[derive(Clone, Copy, PartialEq)]
enum Opposing {
    Nothing,
    /// A vanilla 3/3 for {2}{G}: `intrinsic_value` 1.5 * 3 + 3 + 0.5 * 3 = 9.0.
    ThreeThree,
    /// A vanilla 4/4 for {4}{G}{G}: 1.5 * 4 + 4 + 0.5 * 6 = 13.0. Last Gasp
    /// cannot kill it.
    FourFour,
}

/// A table: P0 holds the card under test plus an Island and a 3/2 for
/// {2}{U}, over a library of six Islands (the expected draw is a land, 3.0).
struct Table {
    scenario: GameScenario,
}

impl Table {
    fn new(opposing: Opposing) -> Self {
        let mut scenario = GameScenario::new();
        for _ in 0..6 {
            scenario.add_land_to_library_top(P0, "Island");
        }
        for _ in 0..10 {
            scenario.add_land_to_library_top(P1, "Plains");
        }
        match opposing {
            Opposing::Nothing => {}
            Opposing::ThreeThree => {
                scenario
                    .add_creature(P1, "Opposing Bear", 3, 3)
                    .with_mana_cost(cost(&[ManaCostShard::Green], 2));
            }
            Opposing::FourFour => {
                scenario
                    .add_creature(P1, "Opposing Beast", 4, 4)
                    .with_mana_cost(cost(&[ManaCostShard::Green, ManaCostShard::Green], 4));
            }
        }
        Self { scenario }
    }

    fn instant(&mut self, name: &str, text: &str, mana: ManaCost) -> ObjectId {
        self.scenario
            .add_spell_to_hand_from_oracle(P0, name, true, text)
            .with_mana_cost(mana)
            .id()
    }

    fn sorcery(&mut self, name: &str, text: &str, mana: ManaCost) -> ObjectId {
        self.scenario
            .add_spell_to_hand_from_oracle(P0, name, false, text)
            .with_mana_cost(mana)
            .id()
    }

    fn enchantment(
        &mut self,
        name: &str,
        subtype: Option<&str>,
        text: &str,
        mana: ManaCost,
    ) -> ObjectId {
        let mut builder = self.scenario.add_spell_to_hand(P0, name, false);
        builder.as_enchantment();
        // An Aura's "Enchant creature" line is a keyword line; the MTGJSON
        // keyword list names it, as the card data does.
        let keywords: &[&str] = match subtype {
            Some(subtype) => {
                builder.with_subtypes(vec![subtype]);
                &["Enchant"]
            }
            None => &[],
        };
        builder
            .from_oracle_text_with_keywords(keywords, text)
            .with_mana_cost(mana)
            .id()
    }

    fn the_theorist(&mut self) -> ObjectId {
        let mut builder = self
            .scenario
            .add_spell_to_hand(P0, "The Theorist, Jace Beleren", false);
        builder
            .from_oracle_text(THEORIST_TEXT)
            .as_planeswalker_with_loyalty("Jace", 3)
            .as_legendary()
            .with_mana_cost(cost(&[ManaCostShard::Blue, ManaCostShard::Blue], 2))
            .id()
    }

    fn three_drop(&mut self) -> ObjectId {
        self.scenario
            .add_creature_to_hand(P0, "Three Drop", 3, 2)
            .with_mana_cost(cost(&[ManaCostShard::Blue], 2))
            .id()
    }

    fn island(&mut self) -> ObjectId {
        self.scenario.add_land_to_hand(P0, "Island").id()
    }

    fn state(self) -> GameState {
        self.scenario.build().state().clone()
    }
}

fn value(state: &GameState, id: ObjectId) -> f64 {
    intrinsic_value(state, id)
}

/// The value of a removal spell on two tables: facing an opposing 3/3 it can
/// answer, and facing an empty board (the opponent's next creature).
fn removal_values(make: impl Fn(&mut Table) -> ObjectId) -> (f64, f64, f64) {
    let mut facing = Table::new(Opposing::ThreeThree);
    let spell = make(&mut facing);
    let land = facing.island();
    let facing = facing.state();
    let mut empty = Table::new(Opposing::Nothing);
    let empty_spell = make(&mut empty);
    let empty = empty.state();
    (
        value(&facing, spell),
        value(&empty, empty_spell),
        value(&facing, land),
    )
}

fn assert_removal_above_a_land(name: &str, make: impl Fn(&mut Table) -> ObjectId) {
    let (facing, empty, land) = removal_values(make);
    assert_eq!(land, 3.0, "a basic land is 3.0");
    assert!(
        facing > land,
        "{name} facing a 3/3 it answers is worth more than a basic land: {facing} vs {land}"
    );
    assert!(
        empty > land,
        "{name} facing an empty board (the opponent's next creature) is worth more than a basic \
         land: {empty} vs {land}"
    );
}

// --- planeswalker ---------------------------------------------------------

/// CR 306.5b + CR 606.3: a planeswalker enters with its printed loyalty and
/// offers one loyalty ability a turn. The Theorist (loyalty 3, a token
/// maker, a bounce, a card each opponent's draw step) is a bomb in the pool 2
/// notes; it is worth more than a basic land and at least a three-mana 3/2.
#[test]
fn the_theorist_is_valued_above_a_basic_land_and_a_three_mana_creature() {
    let mut table = Table::new(Opposing::Nothing);
    let theorist = table.the_theorist();
    let three_drop = table.three_drop();
    let land = table.island();
    let state = table.state();
    let obj = &state.objects[&theorist];
    assert!(obj.card_types.core_types.contains(&CoreType::Planeswalker));
    assert_eq!(obj.loyalty, Some(3));
    let loyalty_abilities = obj
        .abilities
        .iter()
        .filter(|a| matches!(a.cost, Some(AbilityCost::Loyalty { .. })))
        .count();
    assert_eq!(
        loyalty_abilities, 3,
        "the fixture parses three loyalty abilities"
    );
    assert_eq!(
        value(&state, three_drop),
        8.0,
        "a 3/2 for {{2}}{{U}} is 8.0"
    );
    assert!(
        value(&state, theorist) > value(&state, land),
        "The Theorist {} vs a basic land {}",
        value(&state, theorist),
        value(&state, land)
    );
    assert!(
        value(&state, theorist) >= value(&state, three_drop),
        "The Theorist {} vs a three-mana 3/2 {}",
        value(&state, theorist),
        value(&state, three_drop)
    );
}

// --- removal, per class ---------------------------------------------------

/// CR 704.5f: a -3/-3 that takes a creature to 0 toughness is removal.
#[test]
fn a_lethal_shrink_is_valued_as_removal() {
    assert_removal_above_a_land("Last Gasp", |t| {
        t.instant(
            "Last Gasp",
            LAST_GASP_TEXT,
            cost(&[ManaCostShard::Black], 1),
        )
    });
}

/// CR 406.1: exile is removal.
#[test]
fn exile_is_valued_as_removal() {
    assert_removal_above_a_land("Extended Absence", |t| {
        t.instant(
            "Extended Absence",
            EXTENDED_ABSENCE_TEXT,
            cost(&[ManaCostShard::Black], 3),
        )
    });
}

/// CR 701.21a: an opponent's sacrifice of their best creature is removal.
#[test]
fn an_edict_is_valued_as_removal() {
    assert_removal_above_a_land("Break Under Pressure", |t| {
        t.instant(
            "Break Under Pressure",
            BREAK_UNDER_PRESSURE_TEXT,
            cost(&[ManaCostShard::Black], 2),
        )
    });
}

/// CR 613.4b + CR 704.5f: base toughness 0 is lethal.
#[test]
fn a_zero_toughness_base_setter_is_valued_as_removal() {
    assert_removal_above_a_land("Multiply by Zero", |t| {
        t.instant(
            "Multiply by Zero",
            MULTIPLY_BY_ZERO_TEXT,
            cost(&[ManaCostShard::Black], 1),
        )
    });
}

/// CR 603.6a + CR 610.3: an enchantment whose enters trigger exiles an
/// opposing permanent until it leaves is removal.
#[test]
fn an_enters_exile_enchantment_is_valued_as_removal() {
    assert_removal_above_a_land("Memory Trap", |t| {
        t.enchantment(
            "Memory Trap",
            None,
            MEMORY_TRAP_TEXT,
            cost(&[ManaCostShard::White], 2),
        )
    });
}

/// CR 303.4a + CR 613.1f + CR 502.3: an Aura that strips an opposing creature's abilities and keeps
/// it tapped is removal.
#[test]
fn a_harmful_aura_is_valued_as_removal() {
    assert_removal_above_a_land("Infinite Coursework", |t| {
        t.enchantment(
            "Infinite Coursework",
            Some("Aura"),
            INFINITE_COURSEWORK_TEXT,
            cost(&[ManaCostShard::Blue], 2),
        )
    });
}

/// CR 700.2 + CR 701.6a: a modal spell is worth its best mode; a counterspell
/// answers the next creature spell.
#[test]
fn a_modal_counterspell_is_valued_as_removal() {
    assert_removal_above_a_land("Icy Reception", |t| {
        t.instant(
            "Icy Reception",
            ICY_RECEPTION_TEXT,
            cost(&[ManaCostShard::Blue], 1),
        )
    });
}

/// The removal class reads the board through the lethality helpers: facing a
/// 4/4, exile is worth the 4/4 it answers, while a -3/-3 that cannot kill it
/// is worth only the creature the opponent has yet to play.
#[test]
fn removal_is_worth_the_opposing_creature_it_answers() {
    let mut big = Table::new(Opposing::FourFour);
    let exile = big.instant(
        "Extended Absence",
        EXTENDED_ABSENCE_TEXT,
        cost(&[ManaCostShard::Black], 3),
    );
    let shrink = big.instant(
        "Last Gasp",
        LAST_GASP_TEXT,
        cost(&[ManaCostShard::Black], 1),
    );
    let big = big.state();
    let beast = big
        .battlefield
        .iter()
        .copied()
        .find(|id| big.objects[id].name == "Opposing Beast")
        .unwrap();
    assert_eq!(value(&big, beast), 13.0);
    assert!(
        value(&big, exile) >= value(&big, beast),
        "exile facing a 4/4 is worth at least the 4/4: {} vs {}",
        value(&big, exile),
        value(&big, beast)
    );

    let mut empty = Table::new(Opposing::Nothing);
    let empty_shrink = empty.instant(
        "Last Gasp",
        LAST_GASP_TEXT,
        cost(&[ManaCostShard::Black], 1),
    );
    let empty = empty.state();
    assert_eq!(
        value(&big, shrink),
        value(&empty, empty_shrink),
        "a -3/-3 that cannot kill the 4/4 is worth what it is on an empty board"
    );
    assert!(value(&big, shrink) < value(&big, exile));
}

/// A pump or a protection spell is not removal and keeps today's price.
#[test]
fn a_pump_or_protection_instant_stays_below_removal() {
    let mut table = Table::new(Opposing::ThreeThree);
    let removal = table.instant(
        "Last Gasp",
        LAST_GASP_TEXT,
        cost(&[ManaCostShard::Black], 1),
    );
    let pump = table.instant(
        "Giant Growth",
        GIANT_GROWTH_TEXT,
        cost(&[ManaCostShard::Green], 0),
    );
    let protection = table.instant(
        "Sheltering Word",
        PROTECTION_TEXT,
        cost(&[ManaCostShard::White], 0),
    );
    let state = table.state();
    assert!(value(&state, pump) < value(&state, removal));
    assert!(value(&state, protection) < value(&state, removal));
    assert_eq!(value(&state, pump), 0.5, "a {{G}} pump keeps its pip price");
    assert_eq!(
        value(&state, protection),
        0.5,
        "a {{W}} protection spell keeps its pip price"
    );
}

// --- card advantage -------------------------------------------------------

/// CR 121.1: a card drawn is an expected card from the library. Twinned
/// Vision cast from hand draws one; Divination draws two; Catalog draws two
/// and discards one, netting one.
#[test]
fn a_draw_spell_is_worth_an_expected_draw_per_card_it_nets() {
    let mut table = Table::new(Opposing::Nothing);
    let vision = table.instant(
        "Twinned Vision",
        TWINNED_VISION_TEXT,
        cost(&[ManaCostShard::BlueRed], 1),
    );
    let divination = table.sorcery(
        "Divination",
        DIVINATION_TEXT,
        cost(&[ManaCostShard::Blue], 2),
    );
    let catalog = table.instant("Catalog", CATALOG_TEXT, cost(&[ManaCostShard::Blue], 2));
    let state = table.state();
    let draw = expected_draw_value(&state, P0, None).unwrap();
    assert_eq!(draw, 3.0, "a library of Islands draws a land");
    assert!(
        value(&state, vision) >= draw,
        "Twinned Vision {} vs one expected draw {draw}",
        value(&state, vision)
    );
    assert!(
        value(&state, divination) >= 2.0 * draw,
        "Divination {} vs two expected draws {}",
        value(&state, divination),
        2.0 * draw
    );
    assert!(
        value(&state, catalog) >= draw && value(&state, catalog) < value(&state, divination),
        "Catalog nets one card: {} vs Divination {}",
        value(&state, catalog),
        value(&state, divination)
    );
}

// --- guards: creatures and lands as today ---------------------------------

#[test]
fn creature_and_land_values_are_unchanged() {
    let mut table = Table::new(Opposing::ThreeThree);
    let three_drop = table.three_drop();
    let land = table.island();
    let etb_removal = table
        .scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Removal Body",
            2,
            2,
            "When this creature enters, destroy target creature an opponent controls.",
        )
        .with_mana_cost(cost(&[ManaCostShard::Black, ManaCostShard::Black], 3))
        .id();
    let state = table.state();
    assert_eq!(value(&state, three_drop), 8.0);
    assert_eq!(value(&state, land), 3.0);
    // 1.5 * 2 + 2 + 0.5 * 5 = 7.5, the creature formula, plus its enters
    // trigger on the current board: it destroys the opposing 3/3 (9.0).
    // Re-pinned from 7.5 (its enters trigger unread) by
    // spec-ai-creature-ability-pricing (creature_ability_value.rs).
    assert_eq!(value(&state, etb_removal), 16.5);
}

// --- the sites read the same order ----------------------------------------

/// CR 514.1 + CR 701.9a: the keep order (cleanup discard, discard costs)
/// gives up the second land before a removal spell when no plan says the
/// land is needed.
#[test]
fn the_keep_order_gives_up_a_second_land_before_a_removal_spell() {
    let mut table = Table::new(Opposing::ThreeThree);
    let gasp = table.instant(
        "Last Gasp",
        LAST_GASP_TEXT,
        cost(&[ManaCostShard::Black], 1),
    );
    let first = table.island();
    let second = table.island();
    let state = table.state();
    let given_up = given_up_cards(&state, &[first, second, gasp], 1, None);
    assert_eq!(given_up.len(), 1);
    assert_ne!(given_up[0], gasp, "Last Gasp is kept over a land");
}
