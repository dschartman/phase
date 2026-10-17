//! "At the beginning of your upkeep, if this creature isn't prepared, it
//! becomes prepared." (Woodwork Prodigy, Paradox Shaper, Stingerquill
//! Voxmancer; MTGJSON AtomicCards, verified 2026-10-17).
//!
//! CR 722.3a: prepared is a designation a permanent with a prepare spell can
//! have; it can't gain it while it already has it. CR 603.4: a triggered
//! ability of the form "When/Whenever/At [trigger event], if [condition],
//! [effect]" checks the condition when the event occurs and again on
//! resolution; if it is false when the event occurs, the ability doesn't
//! trigger.
//!
//! The cards are parsed from their raw MTGJSON entries through
//! `CardDatabase::from_mtgjson` (the production oracle loader, which also links
//! the prepare spell face), so these rows exercise the real parser rather than
//! a pre-parsed export.

use std::io::Write;
use std::sync::OnceLock;

use engine::database::CardDatabase;
use engine::game::coverage::card_face_gaps;
use engine::game::effects::prepare::can_cast_prepared_copy_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde_json::{json, Value};

const WOODWORK_PRODIGY: &str = "Woodwork Prodigy";
const PARADOX_SHAPER: &str = "Paradox Shaper";
const STINGERQUILL_VOXMANCER: &str = "Stingerquill Voxmancer";

/// One MTGJSON face of a `prepare`-layout card, trimmed to the fields the
/// oracle loader reads. Text is the verbatim Oracle text.
#[allow(clippy::too_many_arguments)]
fn face(
    card: &str,
    face_name: &str,
    side: &str,
    mana_cost: &str,
    mana_value: f64,
    colors: &[&str],
    type_line: &str,
    types: &[&str],
    subtypes: &[&str],
    power_toughness: Option<(&str, &str)>,
    text: &str,
    oracle_id: &str,
) -> Value {
    let mut face = json!({
        "name": card,
        "faceName": face_name,
        "side": side,
        "layout": "prepare",
        "manaCost": mana_cost,
        "manaValue": mana_value,
        "colors": colors,
        "colorIdentity": colors,
        "text": text,
        "type": type_line,
        "types": types,
        "subtypes": subtypes,
        "supertypes": [],
        "keywords": ["Prepared"],
        "identifiers": { "scryfallOracleId": oracle_id },
    });
    if let Some((power, toughness)) = power_toughness {
        face["power"] = json!(power);
        face["toughness"] = json!(toughness);
    }
    face
}

fn atomic_cards() -> Value {
    const WOODWORK_CARD: &str = "Woodwork Prodigy // Soul Tether";
    const WOODWORK_ID: &str = "f3ed157f-a09b-405f-aec4-0e7a81d80fc8";
    const SHAPER_CARD: &str = "Paradox Shaper // Omit Variables";
    const SHAPER_ID: &str = "511951ed-fbff-4e44-9429-27f237496672";
    const VOXMANCER_CARD: &str = "Stingerquill Voxmancer // Vicious Verse";
    const VOXMANCER_ID: &str = "e8f755f7-ec93-4d2e-bffc-c719984fa13d";
    json!({
        "meta": {},
        "data": {
            WOODWORK_CARD: [
                face(
                    WOODWORK_CARD, WOODWORK_PRODIGY, "a", "{2}{R/G}", 3.0, &["R", "G"],
                    "Creature \u{2014} Cat Druid", &["Creature"], &["Cat", "Druid"],
                    Some(("3", "3")),
                    "At the beginning of your upkeep, if this creature isn't prepared, it becomes prepared. (While it's prepared, you may cast a copy of its spell. Doing so unprepares it.)",
                    WOODWORK_ID,
                ),
                face(
                    WOODWORK_CARD, "Soul Tether", "b", "{2}{R/G}", 3.0, &["R", "G"],
                    "Sorcery", &["Sorcery"], &[], None,
                    "Create a Heartwood token. (It's a red and green artifact with \"{T}: Add {R} or {G}.\")",
                    WOODWORK_ID,
                ),
            ],
            SHAPER_CARD: [
                face(
                    SHAPER_CARD, PARADOX_SHAPER, "a", "{1}{U/B}", 2.0, &["U", "B"],
                    "Creature \u{2014} Octopus Wizard", &["Creature"], &["Octopus", "Wizard"],
                    Some(("1", "3")),
                    "At the beginning of your upkeep, if this creature isn't prepared, it becomes prepared.\n{2}: Put target card from your graveyard on the bottom of your library.",
                    SHAPER_ID,
                ),
                face(
                    SHAPER_CARD, "Omit Variables", "b", "{U/B}", 2.0, &["U", "B"],
                    "Sorcery", &["Sorcery"], &[], None,
                    "Mill three cards. (Put the top three cards of your library into your graveyard.)",
                    SHAPER_ID,
                ),
            ],
            VOXMANCER_CARD: [
                face(
                    VOXMANCER_CARD, STINGERQUILL_VOXMANCER, "a", "{B/R}", 1.0, &["B", "R"],
                    "Creature \u{2014} Goblin Sorcerer", &["Creature"], &["Goblin", "Sorcerer"],
                    Some(("1", "2")),
                    "At the beginning of your upkeep, if this creature isn't prepared, it becomes prepared. (While it's prepared, you may cast a copy of its spell. Doing so unprepares it.)",
                    VOXMANCER_ID,
                ),
                face(
                    VOXMANCER_CARD, "Vicious Verse", "b", "{B/R}", 1.0, &["B", "R"],
                    "Sorcery", &["Sorcery"], &[], None,
                    "Vicious Verse deals 1 damage to target opponent.",
                    VOXMANCER_ID,
                ),
            ],
        }
    })
}

/// The three cards parsed from raw MTGJSON by the production oracle loader.
fn prepare_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let mut file = tempfile::NamedTempFile::new().expect("temp AtomicCards file");
        file.write_all(atomic_cards().to_string().as_bytes())
            .expect("write temp AtomicCards file");
        file.flush().expect("flush temp AtomicCards file");
        CardDatabase::from_mtgjson(file.path()).expect("oracle loader parses the prepare cards")
    })
}

fn add_library(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for index in 0..count {
        scenario.add_card_to_library_top(player, &format!("{player:?} Library Card {index}"));
    }
}

/// `card` on P0's battlefield at P0's untap step, optionally already prepared.
fn setup(card: &str, prepared: bool) -> (GameRunner, ObjectId) {
    let db = prepare_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    add_library(&mut scenario, P0, 3);
    add_library(&mut scenario, P1, 3);
    let source = scenario.add_real_card(P0, card, Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(
        runner.state().objects[&source].back_face.is_some(),
        "{card} must hydrate its prepare spell face"
    );
    if prepared {
        runner.state_mut().debug_mode = true;
        runner
            .act(GameAction::Debug(DebugAction::SetPrepared {
                object_id: source,
                prepared: true,
            }))
            .expect("mark the source prepared");
        assert!(
            is_prepared(&runner, source),
            "precondition: {card} is prepared"
        );
    } else {
        assert!(
            !is_prepared(&runner, source),
            "precondition: {card} starts unprepared"
        );
    }
    (runner, source)
}

fn is_prepared(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].prepared.is_some()
}

fn triggers_from_source(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

fn exiled_objects(runner: &GameRunner) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Exile)
        .map(|object| object.id)
        .collect();
    ids.sort();
    ids
}

fn assert_no_unimplemented(ability: &AbilityDefinition, card: &str) {
    assert!(
        !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }),
        "{card}: parsed an Unimplemented effect: {:?}",
        ability.effect
    );
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        assert_no_unimplemented(sub_ability, card);
    }
}

/// CR 722.3a + CR 603.4: an unprepared Woodwork Prodigy becomes prepared at
/// its controller's upkeep, and its prepare spell (Soul Tether) can then be
/// cast that turn.
#[test]
fn woodwork_prodigy_unprepared_becomes_prepared_at_upkeep_and_soul_tether_is_castable() {
    let (mut runner, woodwork) = setup(WOODWORK_PRODIGY, false);

    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(
        triggers_from_source(&runner, woodwork),
        1,
        "the upkeep trigger goes on the stack while Woodwork Prodigy isn't prepared"
    );
    runner.advance_until_stack_empty();
    assert!(
        is_prepared(&runner, woodwork),
        "CR 722.3a: resolving the upkeep trigger makes Woodwork Prodigy prepared"
    );

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(is_prepared(&runner, woodwork));
    for _ in 0..3 {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }
    assert!(
        can_cast_prepared_copy_now(runner.state(), P0, woodwork),
        "CR 722.3c: while Woodwork Prodigy is prepared its controller may cast Soul Tether"
    );

    runner
        .act(GameAction::CastPreparedCopy { source: woodwork })
        .expect("casting the prepared Soul Tether copy is legal");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state while casting Soul Tether: {other:?}"),
        }
    }
    assert!(
        !is_prepared(&runner, woodwork),
        "CR 722.3c: casting the copy unprepares Woodwork Prodigy"
    );
    runner.advance_until_stack_empty();

    let heartwoods = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield
                && object.is_token
                && object.name == "Heartwood"
                && object.controller == P0
        })
        .count();
    assert_eq!(
        heartwoods, 1,
        "Soul Tether resolves and creates a Heartwood token"
    );
}

/// CR 603.4 + CR 722.3a: when Woodwork Prodigy is already prepared as its
/// controller's upkeep begins, the ability doesn't trigger, so nothing happens:
/// it stays prepared with the same linked copy in exile.
#[test]
fn woodwork_prodigy_already_prepared_at_upkeep_does_not_trigger() {
    let (mut runner, woodwork) = setup(WOODWORK_PRODIGY, true);
    let exile_before = exiled_objects(&runner);

    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(
        triggers_from_source(&runner, woodwork),
        0,
        "CR 603.4: the intervening-if is false, so the ability doesn't trigger"
    );
    runner.advance_until_stack_empty();
    assert!(is_prepared(&runner, woodwork), "it stays prepared");
    assert_eq!(
        exiled_objects(&runner),
        exile_before,
        "no new prepare-spell copy is created in exile"
    );
}

/// The same clause on Stingerquill Voxmancer: unprepared at upkeep, it
/// becomes prepared.
#[test]
fn stingerquill_voxmancer_unprepared_becomes_prepared_at_upkeep() {
    let (mut runner, voxmancer) = setup(STINGERQUILL_VOXMANCER, false);

    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(triggers_from_source(&runner, voxmancer), 1);
    runner.advance_until_stack_empty();
    assert!(
        is_prepared(&runner, voxmancer),
        "CR 722.3a: resolving the upkeep trigger makes Stingerquill Voxmancer prepared"
    );
}

/// Parse rows: each card in the class carries one upkeep trigger whose
/// intervening-if is hoisted onto the trigger and whose effect makes the source
/// prepared, and the face has no coverage gap.
#[test]
fn prepared_upkeep_class_parses_with_trigger_condition_and_no_gaps() {
    let db = prepare_db();
    for card in [WOODWORK_PRODIGY, PARADOX_SHAPER, STINGERQUILL_VOXMANCER] {
        let face = db
            .get_face_by_name(card)
            .unwrap_or_else(|| panic!("{card} is in the parsed database"));
        assert_eq!(face.triggers.len(), 1, "{card}: exactly one trigger");
        let trigger = &face.triggers[0];
        assert!(
            trigger.condition.is_some(),
            "{card}: CR 603.4 intervening-if must be on the trigger, got {trigger:?}"
        );
        let execute = trigger
            .execute
            .as_deref()
            .unwrap_or_else(|| panic!("{card}: trigger has an effect"));
        assert_no_unimplemented(execute, card);
        assert!(
            matches!(
                execute.effect.as_ref(),
                Effect::BecomePrepared {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "{card}: the effect makes the source prepared, got {:?}",
            execute.effect
        );
        assert!(
            execute.sub_ability.is_none(),
            "{card}: nothing follows the prepare instruction"
        );
        assert_eq!(
            card_face_gaps(face),
            Vec::<String>::new(),
            "{card}: the face is fully supported"
        );
    }
}
