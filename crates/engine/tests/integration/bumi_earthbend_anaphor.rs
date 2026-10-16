//! CR 603.7c + CR 608.2c + CR 608.2k: a delayed trigger created in the same resolution as a
//! token. The delayed body's anaphor names what the text names:
//!
//! - the object whose event fires an event-delayed trigger (Bumi's Feast Lecture's earthbend
//!   return, CR 701.66a; "When target creature you control dies this turn, return it ..."),
//!   which must not be rebound to the created token;
//! - the created token, when the body names it by a noun the token agrees with ("that token",
//!   "that creature", "that Spirit", "they"), or by a bare "it" when the condition introduces
//!   no object of its own.
//!
//! Printed cards use their verbatim Oracle text (MTGJSON AtomicCards 2026-10-12). Texts marked
//! "constructed" compose printed templates where no printed card combines them; each reading
//! is derived in its doc comment.

use std::collections::BTreeSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, DelayedTriggerCondition, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

const BUMI: &str = "Create a Food token. Then earthbend X, where X is twice the number of Foods you control. (A Food token is an artifact with \"{2}, {T}, Sacrifice this token: You gain 3 life.\" To earthbend X, target land you control becomes a 0/0 creature with haste that's still a land. Put X +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";
const EARTHBENDING_LESSON: &str = "Earthbend 4. (Target land you control becomes a 0/0 creature with haste that's still a land. Put four +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
const FLAMESHADOW: &str = "Whenever a nontoken creature you control enters, you may pay {R}. If you do, create a token that's a copy of that creature. That token gains haste. Exile it at the beginning of the next end step.";

/// Constructed: Bumi's grammar with another token and a fixed count (CR 701.66a).
const TREASURE_EARTHBEND: &str = "Create a Treasure token. Then earthbend 2. (To earthbend 2, target land you control becomes a 0/0 creature with haste that's still a land. Put two +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
/// Constructed: the same chain inside a self-ETB trigger.
const ETB_FOOD_EARTHBEND: &str = "When this creature enters, create a Food token. Then earthbend 2. (To earthbend 2, target land you control becomes a 0/0 creature with haste that's still a land. Put two +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
/// Constructed (the generic grammar): "it" is the creature the condition names (CR 608.2k),
/// found in the graveyard (CR 400.7e). The Treasure is never the dying creature.
const TREASURE_RETURN_DYING: &str = "Create a Treasure token. When target creature you control dies this turn, return it to the battlefield tapped.";

// The created token named in an event-delayed body. Every row's reading is the token: a life
// gain, a drawn card and a cast spell are not creatures, and the dying creature is not a Spirit
// or a token, so the demonstrative's noun agrees only with the created token (CR 111.1).

/// Constructed: "that creature" after a life-gain condition.
const SPIRIT_LIFE_THAT_CREATURE_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you gain life this turn, that creature gets +1/+1 until end of turn.";
/// Constructed: "that Spirit" after a life-gain condition.
const SPIRIT_LIFE_THAT_SPIRIT_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you gain life this turn, that Spirit gets +1/+1 until end of turn.";
/// Constructed: "that creature token" after a life-gain condition.
const SPIRIT_LIFE_THAT_CREATURE_TOKEN_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you gain life this turn, that creature token gets +1/+1 until end of turn.";
/// Constructed (Chandra, Acolyte of Flame's "They gain haste"): "they" is both Spirits.
const SPIRITS_LIFE_THEY_PUMP: &str = "Create two 1/1 white Spirit creature tokens. Whenever you gain life this turn, they get +1/+1 until end of turn.";
/// Constructed: "that creature" with a keyword grant.
const SPIRIT_LIFE_THAT_CREATURE_FLYING: &str = "Create a 1/1 white Spirit creature token. Whenever you gain life this turn, that creature gains flying until end of turn.";
/// Constructed: "that creature" after a draw condition (a card drawn is not a creature).
const SPIRIT_DRAW_THAT_CREATURE_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you draw a card this turn, that creature gets +1/+1 until end of turn.";
/// Constructed: "that Spirit" after an opposing creature dies (that creature is not a Spirit).
const SPIRIT_OPP_DIES_THAT_SPIRIT_PUMP: &str = "Create a 1/1 white Spirit creature token. When target creature an opponent controls dies this turn, that Spirit gets +1/+1 until end of turn.";
/// Constructed: when-next cast body, "that token gains" (a spell is not a token).
const SPIRIT_NEXT_CAST_THAT_TOKEN_FLYING: &str = "Create a 1/1 white Spirit creature token. When you next cast a spell this turn, that token gains flying until end of turn.";
/// Constructed (Summon: Brynhildr's condition + Flameshadow Conjuring's "That token gains"):
/// the Spirit gains flying, not the cast nontoken creature.
const SPIRIT_NEXT_CREATURE_CAST_THAT_TOKEN_FLYING: &str = "Create a 1/1 white Spirit creature token. When you next cast a creature spell this turn, that token gains flying until end of turn.";

/// Constructed: "that token" agrees with the created Spirit, not the dying creature.
const SPIRIT_DIES_THAT_TOKEN_PUMP: &str = "Create a 1/1 white Spirit creature token. When target creature you control dies this turn, that token gets +1/+1 until end of turn.";
/// Constructed: a token is never the cast spell.
const SPIRIT_CAST_THAT_TOKEN_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you cast a spell this turn, that token gets +1/+1 until end of turn.";
/// Constructed: "that token" after a life-gain condition.
const SPIRIT_LIFE_THAT_TOKEN_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you gain life this turn, that token gets +1/+1 until end of turn.";
/// Constructed: gaining life introduces no object, so the bare "it" is the Spirit, the nearest
/// object referent (CR 608.2c).
const SPIRIT_LIFE_IT_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you gain life this turn, it gets +1/+1 until end of turn.";
/// Constructed: as above under a draw condition (the condition's subject is the player).
const SPIRIT_DRAW_IT_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever you draw a card this turn, it gets +1/+1 until end of turn.";
/// Constructed: the copy made by an outer trigger, then a life-gain "it".
const COPY_LIFE_IT_PUMP: &str = "Whenever a nontoken creature you control enters, create a token that's a copy of that creature. Whenever you gain life this turn, it gets +1/+1 until end of turn.";
/// Constructed: a targeted instruction between the creator and the delayed trigger; "that
/// token" still names the Spirit (the tapped creature is not described as a token).
const SPIRIT_TAP_THAT_TOKEN_PUMP: &str = "Create a 1/1 white Spirit creature token. Tap target creature an opponent controls. Whenever you gain life this turn, that token gets +1/+1 until end of turn.";

// Event-delayed cleanups of the token.
const SPIRIT_NEXT_CAST_SACRIFICE_THAT_TOKEN: &str = "Create a 1/1 white Spirit creature token. When you next cast a spell this turn, sacrifice that token.";
const SPIRIT_NEXT_CAST_SACRIFICE_IT: &str =
    "Create a 1/1 white Spirit creature token. When you next cast a spell this turn, sacrifice it.";
const SPIRIT_DIES_EXILE_THAT_TOKEN: &str = "Create a 1/1 white Spirit creature token. When target creature you control dies this turn, exile that token.";
const ETB_SPIRIT_LEAVES_EXILE_THAT_TOKEN: &str = "When this creature enters, create a 1/1 white Spirit creature token. When this creature leaves the battlefield, exile that token.";
const COPY_NEXT_CAST_SACRIFICE_THAT_TOKEN: &str = "Whenever a nontoken creature you control enters, create a token that's a copy of that creature. When you next cast a spell this turn, sacrifice that token.";
const COPY_NEXT_CAST_SACRIFICE_IT: &str = "Whenever a nontoken creature you control enters, create a token that's a copy of that creature. When you next cast a spell this turn, sacrifice it.";
const COPY_HASTE_NEXT_CAST_EXILE_IT: &str = "Whenever a nontoken creature you control enters, create a token that's a copy of that creature. That token gains haste. When you next cast a spell this turn, exile it.";

// Judge rows (engine-change-loop step 5).

/// Constructed (Chandra, Acolyte of Flame's "They gain haste"): plural "they" cannot be the
/// one dying creature; it is the two Spirits (CR 608.2c).
const SPIRITS_DIES_THEY_PUMP: &str = "Create two 1/1 white Spirit creature tokens. When target creature you control dies this turn, they get +1/+1 until end of turn.";
/// Constructed: a bare "it" under a condition that names an object is that object
/// (CR 608.2k): the entering creature, not the Spirit.
const SPIRIT_ENTERS_COUNTER_ON_IT: &str = "Create a 1/1 white Spirit creature token. Whenever a nontoken creature you control enters this turn, put a +1/+1 counter on it.";
/// Constructed: "that creature" whose noun describes the condition's subject: the entering
/// creature.
const SPIRIT_ENTERS_THAT_CREATURE_PUMP: &str = "Create a 1/1 white Spirit creature token. Whenever a nontoken creature you control enters this turn, that creature gets +1/+1 until end of turn.";
/// Constructed: Bumi's grammar with a targeted instruction between the token and the
/// earthbend; the earthbent land returns (CR 701.66a).
const FOOD_TAP_EARTHBEND: &str = "Create a Food token. Tap target creature an opponent controls. Then earthbend 2. (To earthbend 2, target land you control becomes a 0/0 creature with haste that's still a land. Put two +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
/// Constructed: "they" under an opponent subject is that opponent, not the Spirit.
const SPIRIT_OPP_DRAWS_THEY_LOSE_LIFE: &str = "Create a 1/1 white Spirit creature token. Whenever an opponent draws a card this turn, they lose 1 life.";

const GAIN_LIFE: &str = "You gain 3 life.";
const DRAW: &str = "Draw a card.";
const DESTROY: &str = "Destroy target creature.";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn base() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for p in 0..2u8 {
        s.with_library_top(
            PlayerId(p),
            &["Filler 1", "Filler 2", "Filler 3", "Filler 4"],
        );
    }
    s
}

fn object_ids(r: &GameRunner) -> BTreeSet<ObjectId> {
    r.state().objects.keys().copied().collect()
}

fn new_tokens(r: &GameRunner, before: &BTreeSet<ObjectId>) -> Vec<ObjectId> {
    let mut v: Vec<ObjectId> = r
        .state()
        .objects
        .iter()
        .filter(|(id, o)| o.is_token && !before.contains(id))
        .map(|(id, _)| *id)
        .collect();
    v.sort();
    v
}

/// Pass priority until the stack is empty, answering trigger ordering, replacement choices and
/// trigger targets with the first option. Panics at any other prompt.
fn settle(r: &mut GameRunner) {
    for _ in 0..200 {
        let action = match r.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                r.choose_first_legal_target()
                    .expect("trigger target accepted");
                continue;
            }
            other => panic!("settle: unexpected prompt {other:?}"),
        };
        r.act(action).expect("settle action accepted");
    }
    panic!("settle: stack did not empty");
}

fn plus_counters(r: &GameRunner, id: ObjectId) -> u32 {
    r.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn pt(r: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let o = &r.state().objects[&id];
    (o.power, o.toughness)
}

fn has_flying(r: &GameRunner, id: ObjectId) -> bool {
    r.state().objects[&id].has_keyword(&Keyword::Flying)
}

fn left_battlefield(r: &GameRunner, id: ObjectId) -> bool {
    r.state()
        .objects
        .get(&id)
        .is_none_or(|o| o.zone != Zone::Battlefield)
}

// ---------------------------------------------------------------------------
// Earthbend return: the earthbent land, not the created token (red at base)
// ---------------------------------------------------------------------------

struct EarthbendRun {
    runner: GameRunner,
    land: ObjectId,
    tokens: Vec<ObjectId>,
    /// Destroy, exile, bounce, in the caster's hand.
    removal: [ObjectId; 3],
}

const REMOVAL: [&str; 3] = [
    DESTROY,
    "Exile target creature.",
    "Return target creature to its owner's hand.",
];
const KILL: usize = 0;
const EXILE: usize = 1;
const BOUNCE: usize = 2;

fn add_removal(s: &mut GameScenario) -> [ObjectId; 3] {
    REMOVAL.map(|text| {
        s.add_spell_to_hand_from_oracle(P0, "Removal Spell", true, text)
            .id()
    })
}

fn no_extra(_: &mut GameScenario) {}

fn chatterfang(s: &mut GameScenario) {
    s.add_creature(P0, "Chatterfang, Squirrel General", 3, 3)
        .with_subtypes(vec!["Squirrel", "Warrior"])
        .from_oracle_text_with_keywords(&["Forestwalk", "Landwalk"], CHATTERFANG);
}

/// Cast an earthbend spell on a Forest. Positive guards: the Forest is a creature with
/// `counters` +1/+1 counters, `tokens` tokens were created, one delayed trigger waits.
fn cast_earthbend(
    extra: fn(&mut GameScenario),
    name: &str,
    keywords: &[&str],
    text: &str,
    counters: u32,
    tokens: usize,
) -> EarthbendRun {
    let mut s = base();
    extra(&mut s);
    let land = s.add_basic_land(P0, ManaColor::Green);
    let spell = s
        .add_spell_to_hand(P0, name, false)
        .from_oracle_text_with_keywords(keywords, text)
        .id();
    let removal = add_removal(&mut s);
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(spell).target_object(land).resolve();
    settle(&mut runner);
    let created = new_tokens(&runner, &before);
    assert!(
        runner.state().objects[&land]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach: earthbend animated the land"
    );
    assert_eq!(
        plus_counters(&runner, land),
        counters,
        "reach: earthbend's counters"
    );
    assert_eq!(created.len(), tokens, "reach: tokens created");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: the earthbend return waits"
    );
    EarthbendRun {
        runner,
        land,
        tokens: created,
        removal,
    }
}

fn remove_land(run: &mut EarthbendRun, which: usize) {
    let land = run.land;
    run.runner
        .cast(run.removal[which])
        .target_object(land)
        .resolve();
    settle(&mut run.runner);
}

fn assert_land_back_tapped_and_tokens_untouched(run: &EarthbendRun) {
    let land = &run.runner.state().objects[&run.land];
    // CR 701.66a: "When that land dies or is put into exile, return it to the battlefield
    // tapped under your control."
    assert_eq!(land.zone, Zone::Battlefield, "the earthbent land returns");
    assert!(land.tapped, "the land returns tapped");
    // CR 400.7: the returned land is a new object; its counters did not follow it.
    assert_eq!(
        plus_counters(&run.runner, run.land),
        0,
        "the returned land has no counters"
    );
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: the return fired"
    );
    for id in &run.tokens {
        let t = &run.runner.state().objects[id];
        assert_eq!(t.zone, Zone::Battlefield, "token {} stays", t.name);
        assert!(
            !t.tapped,
            "token {} is not the earthbend return's object",
            t.name
        );
    }
}

#[test]
fn bumi_feast_lecture_land_returns_tapped_when_it_dies() {
    let mut run = cast_earthbend(
        no_extra,
        "Bumi's Feast Lecture",
        &["Earthbend", "Food"],
        BUMI,
        2,
        1,
    );
    remove_land(&mut run, KILL);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

#[test]
fn bumi_feast_lecture_land_returns_tapped_when_exiled() {
    let mut run = cast_earthbend(
        no_extra,
        "Bumi's Feast Lecture",
        &["Earthbend", "Food"],
        BUMI,
        2,
        1,
    );
    remove_land(&mut run, EXILE);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

/// Chatterfang adds a Squirrel to the creation (two tokens); X counts Foods (one), so the land
/// gets two counters. Neither token is the return's object.
#[test]
fn bumi_feast_lecture_return_leaves_replacement_added_tokens_alone() {
    let mut run = cast_earthbend(
        chatterfang,
        "Bumi's Feast Lecture",
        &["Earthbend", "Food"],
        BUMI,
        2,
        2,
    );
    remove_land(&mut run, KILL);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

/// Ruling 2025-10-02: a land that goes to any zone other than the graveyard or exile is not
/// returned. Control (green at base).
#[test]
fn bumi_feast_lecture_bounced_land_is_not_returned() {
    let mut run = cast_earthbend(
        no_extra,
        "Bumi's Feast Lecture",
        &["Earthbend", "Food"],
        BUMI,
        2,
        1,
    );
    remove_land(&mut run, BOUNCE);
    assert_eq!(run.runner.state().objects[&run.land].zone, Zone::Hand);
    assert_eq!(
        run.runner.state().delayed_triggers.len(),
        1,
        "the return did not fire"
    );
    assert!(!run.runner.state().objects[&run.tokens[0]].tapped);
}

/// Control (green at base): no token creator in the chain.
#[test]
fn earthbending_lesson_land_returns_tapped() {
    let mut run = cast_earthbend(
        no_extra,
        "Earthbending Lesson",
        &["Earthbend"],
        EARTHBENDING_LESSON,
        4,
        0,
    );
    remove_land(&mut run, KILL);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

/// Class, not card: another token and a fixed count.
#[test]
fn token_then_earthbend_returns_the_land() {
    let mut run = cast_earthbend(
        no_extra,
        "Treasure Earthbend",
        &["Earthbend"],
        TREASURE_EARTHBEND,
        2,
        1,
    );
    remove_land(&mut run, KILL);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

/// Class through a trigger: the same chain in a self-ETB triggered ability.
#[test]
fn triggered_token_then_earthbend_returns_the_land() {
    let mut s = base();
    let land = s.add_basic_land(P0, ManaColor::Green);
    let creature = s
        .add_creature_to_hand_from_oracle(P0, "Food Bender", 2, 2, ETB_FOOD_EARTHBEND)
        .id();
    let removal = add_removal(&mut s);
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(creature).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one Food");
    assert_eq!(
        plus_counters(&runner, land),
        2,
        "reach: the land was earthbent"
    );
    let mut run = EarthbendRun {
        runner,
        land,
        tokens,
        removal,
    };
    remove_land(&mut run, KILL);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

/// The generic delayed grammar: the dying creature returns; the Treasure is untouched.
#[test]
fn token_then_dies_this_turn_return_brings_back_the_creature() {
    let mut s = base();
    let bear = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Treasure Return", false, TREASURE_RETURN_DYING)
        .id();
    let kill = s
        .add_spell_to_hand_from_oracle(P0, "Removal Spell", true, DESTROY)
        .id();
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(spell).target_object(bear).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one Treasure");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: one delayed trigger"
    );
    runner.cast(kill).target_object(bear).resolve();
    settle(&mut runner);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    let b = &runner.state().objects[&bear];
    assert_eq!(b.zone, Zone::Battlefield, "the creature returns");
    assert!(b.tapped, "it returns tapped");
    let treasure = &runner.state().objects[&tokens[0]];
    assert_eq!(treasure.zone, Zone::Battlefield, "the Treasure stays");
    assert!(!treasure.tapped, "the Treasure is untouched");
}

// ---------------------------------------------------------------------------
// The created token, named in the body (green at base: regression guards)
// ---------------------------------------------------------------------------

/// What the token spell targets, if anything.
enum Setup {
    None,
    /// An opposing 2/2 the spell targets (and the follow-up removal kills, when it is removal).
    OpposingCreature,
    /// A 2/2 of the caster's the spell targets.
    OwnCreature,
}

struct TokenRun {
    runner: GameRunner,
    tokens: Vec<ObjectId>,
    target: Option<ObjectId>,
}

/// Cast `text`, then cast `follow` (targeting the setup creature when `follow_targets`).
/// Positive guards: `tokens` tokens were created, one delayed trigger waits, and it fired.
fn token_spell_then(
    text: &str,
    tokens: usize,
    setup: Setup,
    follow: &str,
    follow_targets: bool,
) -> TokenRun {
    let mut s = base();
    let target = match setup {
        Setup::None => None,
        Setup::OpposingCreature => Some(s.add_creature(P1, "Opposing Bear", 2, 2).id()),
        Setup::OwnCreature => Some(s.add_creature(P0, "Grizzly Bears", 2, 2).id()),
    };
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Token Spell", false, text)
        .id();
    let other = s
        .add_spell_to_hand_from_oracle(P0, "Follow Spell", true, follow)
        .id();
    let mut runner = s.build();
    let before = object_ids(&runner);
    let cast = runner.cast(spell);
    let _ = match target {
        Some(t) => cast.target_object(t).resolve(),
        None => cast.resolve(),
    };
    settle(&mut runner);
    let created = new_tokens(&runner, &before);
    assert_eq!(created.len(), tokens, "reach: tokens created");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: one delayed trigger"
    );
    let cast = runner.cast(other);
    let _ = match (target, follow_targets) {
        (Some(t), true) => cast.target_object(t).resolve(),
        _ => cast.resolve(),
    };
    settle(&mut runner);
    TokenRun {
        runner,
        tokens: created,
        target,
    }
}

/// The delayed trigger is a "whenever ... this turn" one that stays; the event happened once.
fn assert_tokens_pumped_once(run: &TokenRun) {
    for id in &run.tokens {
        assert_eq!(
            pt(&run.runner, *id),
            (Some(2), Some(2)),
            "the created token got +1/+1"
        );
    }
}

#[test]
fn token_then_gain_life_that_creature_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_LIFE_THAT_CREATURE_PUMP,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_gain_life_that_spirit_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_LIFE_THAT_SPIRIT_PUMP,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_gain_life_that_creature_token_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_LIFE_THAT_CREATURE_TOKEN_PUMP,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn tokens_then_gain_life_they_get_pumped() {
    let run = token_spell_then(SPIRITS_LIFE_THEY_PUMP, 2, Setup::None, GAIN_LIFE, false);
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_gain_life_that_creature_gains_flying() {
    let run = token_spell_then(
        SPIRIT_LIFE_THAT_CREATURE_FLYING,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert!(has_flying(&run.runner, run.tokens[0]), "the Spirit flies");
}

#[test]
fn token_then_draw_that_creature_gets_pumped() {
    let run = token_spell_then(SPIRIT_DRAW_THAT_CREATURE_PUMP, 1, Setup::None, DRAW, false);
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_opposing_creature_dies_that_spirit_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_OPP_DIES_THAT_SPIRIT_PUMP,
        1,
        Setup::OpposingCreature,
        DESTROY,
        true,
    );
    let opp = run.target.expect("opposing creature");
    assert_eq!(
        run.runner.state().objects[&opp].zone,
        Zone::Graveyard,
        "reach: the opposing creature died"
    );
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_next_cast_that_token_gains_flying() {
    let run = token_spell_then(
        SPIRIT_NEXT_CAST_THAT_TOKEN_FLYING,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert!(has_flying(&run.runner, run.tokens[0]), "the Spirit flies");
}

/// The cast creature is a nontoken Grizzly Bears: it must not gain flying; the Spirit does.
#[test]
fn token_then_next_creature_cast_that_token_gains_flying() {
    let mut s = base();
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Token Spell",
            false,
            SPIRIT_NEXT_CREATURE_CAST_THAT_TOKEN_FLYING,
        )
        .id();
    let bear = s.add_creature_to_hand(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(spell).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one Spirit");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: one delayed trigger"
    );
    runner.cast(bear).resolve();
    settle(&mut runner);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "reach: the creature resolved"
    );
    assert!(has_flying(&runner, tokens[0]), "the Spirit flies");
    assert!(!has_flying(&runner, bear), "the cast creature does not");
}

#[test]
fn token_then_dies_this_turn_that_token_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_DIES_THAT_TOKEN_PUMP,
        1,
        Setup::OwnCreature,
        DESTROY,
        true,
    );
    let bear = run.target.expect("own creature");
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert_eq!(
        run.runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "the creature stays dead"
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_cast_this_turn_that_token_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_CAST_THAT_TOKEN_PUMP,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_gain_life_that_token_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_LIFE_THAT_TOKEN_PUMP,
        1,
        Setup::None,
        GAIN_LIFE,
        false,
    );
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_gain_life_it_gets_pumped() {
    let run = token_spell_then(SPIRIT_LIFE_IT_PUMP, 1, Setup::None, GAIN_LIFE, false);
    assert_tokens_pumped_once(&run);
}

#[test]
fn token_then_draw_it_gets_pumped() {
    let run = token_spell_then(SPIRIT_DRAW_IT_PUMP, 1, Setup::None, DRAW, false);
    assert_tokens_pumped_once(&run);
}

/// A targeted instruction between the creator and the delayed trigger must not hide the
/// token's noun.
#[test]
fn token_then_targeted_instruction_that_token_still_gets_pumped() {
    let run = token_spell_then(
        SPIRIT_TAP_THAT_TOKEN_PUMP,
        1,
        Setup::OpposingCreature,
        GAIN_LIFE,
        false,
    );
    assert!(
        run.runner.state().objects[&run.target.expect("opposing creature")].tapped,
        "reach: the opposing creature was tapped"
    );
    assert_tokens_pumped_once(&run);
}

/// A copy of a 2/2 made by an outer trigger, then a life-gain "it": the copy is 3/3, the
/// original stays 2/2.
#[test]
fn copy_token_then_gain_life_it_gets_pumped() {
    let run = copy_engine_then(COPY_LIFE_IT_PUMP, GAIN_LIFE);
    assert_eq!(
        pt(&run.runner, run.copy),
        (Some(3), Some(3)),
        "the copy got +1/+1"
    );
    assert_eq!(
        pt(&run.runner, run.original),
        (Some(2), Some(2)),
        "the original is untouched"
    );
}

// ---------------------------------------------------------------------------
// Event-delayed cleanups name the token (green at base: regression guards)
// ---------------------------------------------------------------------------

#[test]
fn token_then_next_cast_sacrifice_that_token() {
    let run = token_spell_then(
        SPIRIT_NEXT_CAST_SACRIFICE_THAT_TOKEN,
        1,
        Setup::None,
        DRAW,
        false,
    );
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert!(
        left_battlefield(&run.runner, run.tokens[0]),
        "the Spirit was sacrificed"
    );
}

#[test]
fn token_then_next_cast_sacrifice_it() {
    let run = token_spell_then(SPIRIT_NEXT_CAST_SACRIFICE_IT, 1, Setup::None, DRAW, false);
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert!(
        left_battlefield(&run.runner, run.tokens[0]),
        "the Spirit was sacrificed"
    );
}

#[test]
fn token_then_dies_this_turn_exile_that_token() {
    let run = token_spell_then(
        SPIRIT_DIES_EXILE_THAT_TOKEN,
        1,
        Setup::OwnCreature,
        DESTROY,
        true,
    );
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert_eq!(
        run.runner.state().objects[&run.target.expect("own creature")].zone,
        Zone::Graveyard,
        "the dead creature stays in the graveyard"
    );
    assert!(
        left_battlefield(&run.runner, run.tokens[0]),
        "the Spirit was exiled"
    );
}

#[test]
fn self_etb_token_exiled_when_this_creature_leaves() {
    let mut s = base();
    let me = s
        .add_creature_to_hand_from_oracle(
            P0,
            "Spirit Keeper",
            3,
            4,
            ETB_SPIRIT_LEAVES_EXILE_THAT_TOKEN,
        )
        .id();
    let kill = s
        .add_spell_to_hand_from_oracle(P0, "Removal Spell", true, DESTROY)
        .id();
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(me).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one Spirit");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: one delayed trigger"
    );
    runner.cast(kill).target_object(me).resolve();
    settle(&mut runner);
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert!(
        left_battlefield(&runner, tokens[0]),
        "the Spirit was exiled"
    );
}

struct CopyRun {
    runner: GameRunner,
    original: ObjectId,
    copy: ObjectId,
}

/// A copy engine on the battlefield; cast a Grizzly Bears (one copy, one delayed trigger),
/// then cast `follow`.
fn copy_engine_then(engine_text: &str, follow: &str) -> CopyRun {
    let mut s = base();
    s.add_enchantment_from_oracle(P0, "Copy Engine", engine_text);
    let bear = s.add_creature_to_hand(P0, "Grizzly Bears", 2, 2).id();
    let other = s
        .add_spell_to_hand_from_oracle(P0, "Follow Spell", true, follow)
        .id();
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(bear).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one copy");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: one delayed trigger"
    );
    runner.cast(other).resolve();
    settle(&mut runner);
    CopyRun {
        runner,
        original: bear,
        copy: tokens[0],
    }
}

fn assert_copy_left_original_stayed(run: &CopyRun) {
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert_eq!(
        run.runner.state().objects[&run.original].zone,
        Zone::Battlefield,
        "the original stays"
    );
    assert!(left_battlefield(&run.runner, run.copy), "the copy left");
}

#[test]
fn copy_token_then_next_cast_sacrifice_that_token() {
    assert_copy_left_original_stayed(&copy_engine_then(COPY_NEXT_CAST_SACRIFICE_THAT_TOKEN, DRAW));
}

#[test]
fn copy_token_then_next_cast_sacrifice_it() {
    assert_copy_left_original_stayed(&copy_engine_then(COPY_NEXT_CAST_SACRIFICE_IT, DRAW));
}

#[test]
fn copy_token_then_next_cast_exile_it() {
    assert_copy_left_original_stayed(&copy_engine_then(COPY_HASTE_NEXT_CAST_EXILE_IT, DRAW));
}

// ---------------------------------------------------------------------------
// Judge rows: plural "they", the condition's own object, a targeted instruction
// ---------------------------------------------------------------------------

/// Right at base (both Spirits 2/2): plural "they" names the created tokens.
#[test]
fn tokens_then_dies_this_turn_they_get_pumped() {
    let run = token_spell_then(SPIRITS_DIES_THEY_PUMP, 2, Setup::OwnCreature, DESTROY, true);
    assert!(
        run.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert_eq!(
        run.runner.state().objects[&run.target.expect("own creature")].zone,
        Zone::Graveyard,
        "reach: the creature died"
    );
    assert_tokens_pumped_once(&run);
}

/// Cast `text`, then cast a nontoken Grizzly Bears from hand; returns the runner, the
/// Bears and the created token.
fn token_spell_then_cast_bears(text: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = base();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Token Spell", false, text)
        .id();
    let bear = s.add_creature_to_hand(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(spell).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one Spirit");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: one delayed trigger"
    );
    runner.cast(bear).resolve();
    settle(&mut runner);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "reach: the creature resolved"
    );
    (runner, bear, tokens[0])
}

/// Right at base: the counter goes on the entering creature.
#[test]
fn token_then_creature_enters_counter_on_it_is_the_entering_creature() {
    let (runner, bear, spirit) = token_spell_then_cast_bears(SPIRIT_ENTERS_COUNTER_ON_IT);
    assert_eq!(
        plus_counters(&runner, bear),
        1,
        "the entering creature gets it"
    );
    assert_eq!(plus_counters(&runner, spirit), 0, "the Spirit does not");
}

/// Wrong at base (the Spirit was pumped): "that creature" describes the entering creature.
#[test]
fn token_then_creature_enters_that_creature_is_the_entering_creature() {
    let (runner, bear, spirit) = token_spell_then_cast_bears(SPIRIT_ENTERS_THAT_CREATURE_PUMP);
    assert_eq!(
        pt(&runner, bear),
        (Some(3), Some(3)),
        "the entering creature is pumped"
    );
    assert_eq!(pt(&runner, spirit), (Some(1), Some(1)), "the Spirit is not");
}

/// Wrong at base (the Food was tapped): the targeted instruction between the token and the
/// earthbend does not change what the return names.
#[test]
fn token_then_targeted_instruction_then_earthbend_returns_the_land() {
    let mut s = base();
    let opp = s.add_creature(P1, "Opposing Bear", 2, 2).id();
    let land = s.add_basic_land(P0, ManaColor::Green);
    let spell = s
        .add_spell_to_hand(P0, "Food Tap Bend", false)
        .from_oracle_text_with_keywords(&["Earthbend", "Food"], FOOD_TAP_EARTHBEND)
        .id();
    let removal = add_removal(&mut s);
    let mut runner = s.build();
    let before = object_ids(&runner);
    runner.cast(spell).target_objects(&[opp, land]).resolve();
    settle(&mut runner);
    let tokens = new_tokens(&runner, &before);
    assert_eq!(tokens.len(), 1, "reach: one Food");
    assert!(
        runner.state().objects[&opp].tapped,
        "reach: the opposing creature was tapped"
    );
    assert_eq!(
        plus_counters(&runner, land),
        2,
        "reach: the land was earthbent"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: the earthbend return waits"
    );
    let mut run = EarthbendRun {
        runner,
        land,
        tokens,
        removal,
    };
    remove_land(&mut run, KILL);
    assert_land_back_tapped_and_tokens_untouched(&run);
}

/// Right at base: "they" under an opponent subject is the opponent who drew.
#[test]
fn token_then_opponent_draws_they_is_that_opponent() {
    let run = token_spell_then(
        SPIRIT_OPP_DRAWS_THEY_LOSE_LIFE,
        1,
        Setup::None,
        "Each player draws a card.",
        false,
    );
    let state = run.runner.state();
    assert_eq!(
        (state.players[0].life, state.players[1].life),
        (20, 19),
        "the opponent who drew loses 1 life"
    );
}

// ---------------------------------------------------------------------------
// Parse shapes
// ---------------------------------------------------------------------------

fn delayed_payloads(
    text: &str,
    keywords: &[&str],
    types: &[&str],
) -> Vec<(DelayedTriggerCondition, Effect)> {
    let keywords: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(text, "Shape", &keywords, &types, &[]);
    let mut out = Vec::new();
    fn walk(def: &AbilityDefinition, out: &mut Vec<(DelayedTriggerCondition, Effect)>) {
        if let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = def.effect.as_ref()
        {
            out.push((condition.clone(), effect.effect.as_ref().clone()));
            walk(effect, out);
        }
        if let Some(sub) = def.sub_ability.as_deref() {
            walk(sub, out);
        }
    }
    for def in &parsed.abilities {
        walk(def, &mut out);
    }
    for trigger in &parsed.triggers {
        if let Some(exec) = trigger.execute.as_deref() {
            walk(exec, &mut out);
        }
    }
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "reach: the text parses without Unimplemented"
    );
    out
}

/// SHAPE: the return names the land whose event fires it (CR 701.66a), found in the graveyard
/// or exile (CR 400.7e), like every other earthbend card.
#[test]
fn bumi_feast_lecture_return_payload_names_the_dying_land() {
    let payloads = delayed_payloads(BUMI, &["Earthbend", "Food"], &["Sorcery"]);
    assert_eq!(payloads.len(), 1, "reach: one delayed trigger");
    let (condition, payload) = &payloads[0];
    assert!(matches!(
        condition,
        DelayedTriggerCondition::WhenDiesOrExiled {
            filter: TargetFilter::ParentTarget
        }
    ));
    assert!(
        matches!(
            payload,
            Effect::ChangeZone {
                target: TargetFilter::TriggeringSource,
                origin: None,
                destination: Zone::Battlefield,
                enter_tapped: EtbTapState::Tapped,
                ..
            }
        ),
        "the return names the land whose event fires it, got {payload:?}"
    );
}

/// SHAPE: a phase-delayed cleanup after a token copier still names the token.
#[test]
fn phase_delayed_cleanup_after_token_copier_still_names_the_token() {
    let payloads = delayed_payloads(FLAMESHADOW, &[], &["Enchantment"]);
    let (condition, payload) = payloads.first().expect("reach: one delayed trigger");
    assert!(matches!(
        condition,
        DelayedTriggerCondition::AtNextPhase { .. }
    ));
    assert!(
        matches!(
            payload,
            Effect::ChangeZone {
                target: TargetFilter::LastCreated,
                origin: Some(Zone::Battlefield),
                ..
            }
        ),
        "got {payload:?}"
    );
}
