//! "~ fights up to one target creature an opponent controls" (Mind Meanderer):
//! the controller may choose no target, and then nothing fights.
//!
//! CR 601.2c: "If the spell has a variable number of targets, the player
//! announces how many targets they will choose before they announce those
//! targets." CR 603.3d applies the same process to a triggered ability put on
//! the stack. CR 115.6: an ability that requires targets may allow zero targets
//! to be chosen, so "up to one" always has a legal answer. CR 701.14b: with no
//! creature to fight, neither fights or deals damage.
//!
//! The card data for this class stamps `multi_target {min: 0, max: 1}` and
//! leaves the ability-wide `optional_targeting` false. The zero-target authority
//! for both encodings is `ResolvedAbility::targeting_is_optional()`.
//!
//! Rows:
//! - U1: one opposing creature. The trigger prompts (no auto-assign), the slot
//!   offers no target, and declining leaves both creatures undamaged.
//! - U2: the same board, choosing the creature, fights (positive control).
//! - U3: two opposing creatures. The prompt offers no target, and declining
//!   leaves every creature undamaged.
//! - U4: no opposing creature. "Up to one" still has a legal choice (zero), so
//!   the trigger goes on the stack with no target and resolves with no fight.
//! - S1: the spell form of the class, "Then it fights up to one target creature
//!   you don't control" (Primal Might): the second target may be declined, and
//!   then the first target fights nothing.
//! - S2: the same spell is castable with no creature to fight (CR 115.6).
//! - M1, guard: a mandatory fight ("that creature fights target creature an
//!   opponent controls", Gruul Ragebeast) with no legal target is removed from
//!   the stack (CR 603.3d).
//! - M2, guard: the same mandatory fight with one legal target is assigned
//!   without a prompt, as before, and the crime is recorded (CR 700.13).
//! - D1, guard: a two-fighter "target creature you control fights up to one
//!   target creature you don't control" (Smell Fear) keeps its first fighter
//!   required: with no creature of your own it cannot be cast. Its `multi_target`
//!   does not say which instance of "target" is "up to one", so only a one-slot
//!   fight reads it.

use engine::ai_support::legal_actions;
use engine::game::scenario::{CastCommit, CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim from MTGJSON `AtomicCards.json` (Mind Meanderer, FRA, 4/4).
const MIND_MEANDERER_TEXT: &str = "Flying\nThis creature has vigilance as long as you control a \
    Jace planeswalker.\nWhen this creature enters, it fights up to one target creature an \
    opponent controls. (Each deals damage equal to its power to the other.)";

/// Verbatim from MTGJSON `AtomicCards.json` (Primal Might, sorcery): the spell
/// form, a fight sub-instruction whose fighter is the parent's target.
const PRIMAL_MIGHT_TEXT: &str = "Target creature you control gets +X/+X until end of turn. \
    Then it fights up to one target creature you don't control. (Each deals damage equal to its \
    power to the other.)";

/// Verbatim from MTGJSON `AtomicCards.json` (Smell Fear, sorcery): a two-fighter
/// fight whose second instance of "target" is "up to one".
const SMELL_FEAR_TEXT: &str = "Proliferate. (Choose any number of permanents and/or players, \
    then give each another counter of each kind already there.)\nTarget creature you control \
    fights up to one target creature you don't control.";

/// Verbatim from MTGJSON `AtomicCards.json` (Gruul Ragebeast, 6/6): the same
/// fight with a mandatory target.
const GRUUL_RAGEBEAST_TEXT: &str = "Whenever this creature or another creature you control \
    enters, that creature fights target creature an opponent controls.";

/// Upper bound on staging steps; only there so a broken pipeline fails with a
/// message instead of spinning.
const STAGING_STEP_LIMIT: usize = 40;

/// What the staging helper saw on the way from casting the creature to its
/// enters trigger sitting on the stack.
#[derive(Debug, Default)]
struct StagingObservation {
    /// The slots of every `TriggerTargetSelection` prompt answered, in order.
    prompts: Vec<Vec<TargetSelectionSlot>>,
    /// The legal-action list the engine issued at each of those prompts.
    prompt_actions: Vec<Vec<GameAction>>,
    /// Whether the trigger became a real stack entry.
    staged: bool,
}

/// P0 holds `name` (free to cast, verbatim `text`) and P1 controls one creature
/// per entry of `opponents`. P0 is active and holds priority.
fn build_board(
    name: &str,
    power: i32,
    toughness: i32,
    text: &str,
    opponents: &[(&str, i32, i32)],
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opposing = opponents
        .iter()
        .map(|(name, power, toughness)| scenario.add_creature(P1, name, *power, *toughness).id())
        .collect();
    let source = scenario
        .add_creature_to_hand_from_oracle(P0, name, power, toughness, text)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    (runner, source, opposing)
}

fn meanderer_board(opponents: &[(&str, i32, i32)]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    build_board("Mind Meanderer", 4, 4, MIND_MEANDERER_TEXT, opponents)
}

/// The resolved ability of `source`'s triggered ability on the stack, if any.
fn trigger_ability(state: &GameState, source: ObjectId) -> Option<&ResolvedAbility> {
    state.stack.iter().find_map(|entry| {
        let is_source_trigger = entry.source_id == source
            && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. });
        if !is_source_trigger {
            return None;
        }
        entry.ability()
    })
}

/// Cast `source` and drive the real pipeline until its enters trigger sits on
/// the stack, unresolved, with priority open, or until the stack empties
/// without it (the trigger was removed, CR 603.3d).
///
/// Every `TriggerTargetSelection` prompt is answered with `answer` (`None`
/// declines the slot); the legal actions issued at the prompt are recorded
/// before answering.
fn stage_trigger(
    runner: &mut GameRunner,
    source: ObjectId,
    answer: Option<ObjectId>,
) -> (CastCommit<'_>, StagingObservation) {
    let mut commit = runner.cast(source).commit();
    let mut observation = StagingObservation::default();

    for _ in 0..STAGING_STEP_LIMIT {
        let state = commit.state();
        match &state.waiting_for {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                observation.prompts.push(target_slots.clone());
                observation.prompt_actions.push(legal_actions(state));
                commit
                    .act(GameAction::ChooseTarget {
                        target: answer.map(TargetRef::Object),
                    })
                    .expect("ChooseTarget should be accepted for the enters trigger");
            }
            WaitingFor::Priority { .. } => {
                let on_battlefield =
                    state.objects.get(&source).map(|object| object.zone) == Some(Zone::Battlefield);
                if on_battlefield && state.pending_trigger_entry.is_none() {
                    if trigger_ability(state, source).is_some() {
                        observation.staged = true;
                        return (commit, observation);
                    }
                    if state.stack.is_empty() {
                        return (commit, observation);
                    }
                }
                assert!(
                    !state.stack.is_empty(),
                    "the stack emptied before the creature entered \
                     (observation so far: {observation:?})"
                );
                commit
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while staging the trigger");
            }
            other => panic!(
                "unexpected waiting state while staging the trigger: {other:?} \
                 (observation so far: {observation:?})"
            ),
        }
    }
    panic!(
        "the enters trigger was not staged within {STAGING_STEP_LIMIT} steps \
         (observation so far: {observation:?}, waiting_for: {:?})",
        commit.state().waiting_for
    );
}

/// Assert exactly one prompt with one optional slot offering every object in
/// `offered`, and that the engine issued the no-target answer at it.
///
/// This is the reach guard that separates "up to one" from a mandatory slot: a
/// mandatory slot with exactly one legal choice is bound without a prompt, and
/// a mandatory slot with several offers no "no target" answer.
fn assert_single_optional_prompt_offers(observation: &StagingObservation, offered: &[ObjectId]) {
    let [slots] = observation.prompts.as_slice() else {
        panic!(
            "REACH GUARD: CR 601.2c + CR 603.3d: the trigger must raise exactly one target \
             prompt for \"up to one\" (observation: {observation:?})"
        );
    };
    let [slot] = slots.as_slice() else {
        panic!("REACH GUARD: the prompt must have exactly one slot (slots: {slots:?})");
    };
    assert!(
        slot.optional,
        "CR 115.6: \"up to one target\" makes the slot optional (slot: {slot:?})"
    );
    for object in offered {
        assert!(
            slot.legal_targets.contains(&TargetRef::Object(*object)),
            "the opponent's creature {object:?} must be a legal choice (slot: {slot:?})"
        );
    }
    let [actions] = observation.prompt_actions.as_slice() else {
        unreachable!("one action list is recorded per prompt");
    };
    assert!(
        actions.contains(&GameAction::ChooseTarget { target: None }),
        "the legal-action list at the prompt must offer the no-target answer \
         (actions: {actions:?})"
    );
    for object in offered {
        assert!(
            actions.contains(&GameAction::ChooseTarget {
                target: Some(TargetRef::Object(*object)),
            }),
            "the legal-action list must offer {object:?} too (actions: {actions:?})"
        );
    }
}

fn fight_damage_events(events: &[GameEvent]) -> Vec<(ObjectId, TargetRef, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                is_combat: false,
                ..
            } => Some((*source_id, target.clone(), *amount)),
            _ => None,
        })
        .collect()
}

fn fight_resolved(outcome: &CastOutcome, source: ObjectId) -> bool {
    outcome.events().iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Fight,
                source_id,
                ..
            } if *source_id == source
        )
    })
}

/// Assert that `objects` are on the battlefield with no damage marked.
fn assert_untouched(outcome: &CastOutcome, objects: &[ObjectId]) {
    for object in objects {
        let state_object = outcome.state().objects.get(object).unwrap();
        assert_eq!(
            state_object.zone,
            Zone::Battlefield,
            "{object:?} stays on the battlefield"
        );
        assert_eq!(
            state_object.damage_marked, 0,
            "CR 701.14b: nothing fights, so {object:?} is dealt no damage"
        );
    }
}

fn assert_resolved_to_priority(outcome: &CastOutcome) {
    assert!(
        outcome.state().stack.is_empty(),
        "the stack is empty after resolution (stack: {:?})",
        outcome.state().stack
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution ends at priority (waiting_for: {:?})",
        outcome.final_waiting_for()
    );
}

/// U1. CR 601.2c + CR 603.3d + CR 115.6: with exactly one opposing creature the
/// controller is still asked, the prompt offers no target, and declining
/// resolves the trigger with no fight: Mind Meanderer and the 4/4 both live.
///
/// Fails on revert: the mandatory-shaped slot has one legal choice and is bound
/// without a prompt (seed 1001 T19), so the prompt reach guard fails.
#[test]
fn mind_meanderer_one_opposing_creature_prompts_and_declining_fights_nothing() {
    let (mut runner, meanderer, opposing) = meanderer_board(&[("Rumbling Baloth", 4, 4)]);
    let baloth = opposing[0];

    let (commit, observation) = stage_trigger(&mut runner, meanderer, None);
    assert_single_optional_prompt_offers(&observation, &[baloth]);
    assert!(observation.staged, "the declined trigger reaches the stack");
    assert_eq!(
        trigger_ability(commit.state(), meanderer).map(|ability| ability.targets.clone()),
        Some(Vec::new()),
        "the declined trigger is on the stack with no declared target"
    );

    let outcome = commit.resolve();

    assert_resolved_to_priority(&outcome);
    assert!(
        fight_resolved(&outcome, meanderer),
        "CR 115.6 + CR 701.14b: the fight instruction resolves with nothing to fight; it \
         does not abort the resolution (events were {:?})",
        outcome.events()
    );
    assert_untouched(&outcome, &[meanderer, baloth]);
    assert_eq!(
        fight_damage_events(outcome.events()),
        Vec::new(),
        "no fight damage is dealt (events were {:?})",
        outcome.events()
    );
}

/// U2, positive control. The same prompt, answered with the 4/4: each deals 4
/// damage to the other and both die (CR 701.14a, CR 704.5g).
#[test]
fn mind_meanderer_one_opposing_creature_chosen_fights_it() {
    let (mut runner, meanderer, opposing) = meanderer_board(&[("Rumbling Baloth", 4, 4)]);
    let baloth = opposing[0];

    let (commit, observation) = stage_trigger(&mut runner, meanderer, Some(baloth));
    assert_single_optional_prompt_offers(&observation, &[baloth]);
    assert_eq!(
        trigger_ability(commit.state(), meanderer).map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Object(baloth)]),
        "the chosen creature is the trigger's declared target"
    );

    let outcome = commit.resolve();

    assert_resolved_to_priority(&outcome);
    let mut damage = fight_damage_events(outcome.events());
    damage.sort_by_key(|(source, _, _)| *source);
    let mut expected = vec![
        (meanderer, TargetRef::Object(baloth), 4),
        (baloth, TargetRef::Object(meanderer), 4),
    ];
    expected.sort_by_key(|(source, _, _)| *source);
    assert_eq!(
        damage, expected,
        "CR 701.14a: each deals damage equal to its power to the other"
    );
    for object in [meanderer, baloth] {
        assert_eq!(
            outcome.zone_of(object),
            Zone::Graveyard,
            "CR 704.5g: {object:?} was dealt lethal damage"
        );
    }
}

/// U3. CR 601.2c + CR 115.6: with two opposing creatures the prompt offers
/// both and the no-target answer (seed 1004 offered only the creatures), and
/// declining fights nothing.
///
/// Fails on revert: the slot is not optional, so the engine never issues
/// `ChooseTarget { target: None }`.
#[test]
fn mind_meanderer_two_opposing_creatures_prompt_offers_no_target() {
    let (mut runner, meanderer, opposing) =
        meanderer_board(&[("Rumbling Baloth", 4, 4), ("Grizzly Bears", 2, 2)]);

    let (commit, observation) = stage_trigger(&mut runner, meanderer, None);
    assert_single_optional_prompt_offers(&observation, &opposing);
    assert!(observation.staged, "the declined trigger reaches the stack");

    let outcome = commit.resolve();

    assert_resolved_to_priority(&outcome);
    assert!(
        fight_resolved(&outcome, meanderer),
        "CR 115.6 + CR 701.14b: the fight instruction resolves with nothing to fight; it \
         does not abort the resolution (events were {:?})",
        outcome.events()
    );
    let mut everyone = opposing.clone();
    everyone.push(meanderer);
    assert_untouched(&outcome, &everyone);
}

/// U4. CR 603.3d + CR 115.6: with no opposing creature, zero targets is still a
/// legal choice for "up to one", so the trigger is put on the stack (it is not
/// "simply removed") and resolves with no fight.
///
/// Fails on revert: the slot is treated as mandatory with no legal target, so
/// the trigger is removed from the stack and never staged.
#[test]
fn mind_meanderer_no_opposing_creature_trigger_still_goes_on_the_stack() {
    let (mut runner, meanderer, _) = meanderer_board(&[]);

    let (commit, observation) = stage_trigger(&mut runner, meanderer, None);
    assert!(
        observation.prompts.is_empty(),
        "with no legal target the only answer is zero targets, chosen without a prompt \
         (observation: {observation:?})"
    );
    assert!(
        observation.staged,
        "CR 603.3d: \"up to one\" has a legal choice, so the trigger goes on the stack \
         (observation: {observation:?})"
    );
    assert_eq!(
        trigger_ability(commit.state(), meanderer).map(|ability| ability.targets.clone()),
        Some(Vec::new()),
        "the trigger has no declared target"
    );

    let outcome = commit.resolve();

    assert_resolved_to_priority(&outcome);
    assert!(
        fight_resolved(&outcome, meanderer),
        "CR 115.6 + CR 701.14b: the fight instruction resolves with nothing to fight; it \
         does not abort the resolution (events were {:?})",
        outcome.events()
    );
    assert_untouched(&outcome, &[meanderer]);
    assert!(
        fight_damage_events(outcome.events()).is_empty(),
        "no fight damage is dealt (events were {:?})",
        outcome.events()
    );
}

/// M1, guard. CR 603.3d: a mandatory fight trigger with no legal target is
/// removed from the stack; nothing changes for it.
#[test]
fn mandatory_fight_trigger_with_no_legal_target_is_removed() {
    let (mut runner, ragebeast, _) =
        build_board("Gruul Ragebeast", 6, 6, GRUUL_RAGEBEAST_TEXT, &[]);

    let (commit, observation) = stage_trigger(&mut runner, ragebeast, None);
    assert!(
        observation.prompts.is_empty(),
        "no prompt for a trigger with no legal target (observation: {observation:?})"
    );
    assert!(
        !observation.staged,
        "CR 603.3d: the mandatory trigger is removed from the stack (observation: {observation:?})"
    );
    assert_eq!(
        commit
            .state()
            .objects
            .get(&ragebeast)
            .map(|object| object.zone),
        Some(Zone::Battlefield),
        "REACH GUARD: the creature entered, so its trigger fired"
    );
    assert!(
        commit.state().stack.is_empty(),
        "nothing is left on the stack"
    );
}

/// M2, guard. A mandatory fight trigger with exactly one legal target is bound
/// without a prompt, as before, and resolves. CR 700.13: the target is a
/// permanent an opponent controls, so P0 commits a crime as the trigger is put
/// on the stack.
#[test]
fn mandatory_fight_trigger_with_one_legal_target_auto_assigns_and_fights() {
    let (mut runner, ragebeast, opposing) = build_board(
        "Gruul Ragebeast",
        6,
        6,
        GRUUL_RAGEBEAST_TEXT,
        &[("Grizzly Bears", 2, 2)],
    );
    let bears = opposing[0];

    let (commit, observation) = stage_trigger(&mut runner, ragebeast, None);
    assert!(
        observation.prompts.is_empty(),
        "one legal target for a mandatory slot is assigned without a prompt \
         (observation: {observation:?})"
    );
    assert!(observation.staged, "the trigger reaches the stack");
    assert_eq!(
        trigger_ability(commit.state(), ragebeast).map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Object(bears)]),
        "the only legal creature is the declared target"
    );
    // CR 700.13: the crime is recorded as the trigger is put on the stack. The
    // `BecomesTarget` / `CrimeCommitted` events the auto-assignment builds are
    // not asserted here: on this path (`engine_priority` -> `process_triggers`)
    // they feed the deferred-trigger scan but are not returned in the action's
    // events, a reporting gap outside this change.
    let p0 = commit
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 exists");
    assert_eq!(
        p0.crimes_committed_this_turn, 1,
        "CR 700.13: targeting a permanent an opponent controls is a crime"
    );

    let outcome = commit.resolve();

    assert_resolved_to_priority(&outcome);
    assert!(fight_resolved(&outcome, ragebeast), "the fight resolved");
    assert_eq!(
        outcome.zone_of(bears),
        Zone::Graveyard,
        "CR 704.5g: the 6/6 deals lethal damage to the 2/2"
    );
    assert_eq!(outcome.zone_of(ragebeast), Zone::Battlefield);
}

/// Whether the engine issues a cast of `spell` in the current state.
fn cast_is_offered(state: &GameState, spell: ObjectId) -> bool {
    legal_actions(state).iter().any(|action| {
        matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == spell
        )
    })
}

/// A board with Smell Fear (free) in P0's hand, a Grizzly Bears for P1 and,
/// when requested, a Grizzly Bears for P0. P0 is active and holds priority.
fn smell_fear_board(with_own_creature: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    if with_own_creature {
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Smell Fear", false, SMELL_FEAR_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    (runner, spell)
}

/// D1, guard. CR 601.2c: "target creature you control" is a required target of
/// Smell Fear, so with no creature of P0's own the spell cannot be cast, even
/// though its second target is "up to one" (CR 115.6).
#[test]
fn two_fighter_up_to_one_keeps_its_required_fighter() {
    let (runner, spell) = smell_fear_board(true);
    assert!(
        cast_is_offered(runner.state(), spell),
        "REACH GUARD: with a creature of P0's own, Smell Fear is castable"
    );

    let (runner, spell) = smell_fear_board(false);
    assert!(
        !cast_is_offered(runner.state(), spell),
        "CR 601.2c: with no creature P0 controls, the required first fighter has no legal \
         target, so Smell Fear is not castable"
    );
}

/// A board with Primal Might (free, so X is 0) in P0's hand, a Grizzly Bears
/// for P0 and, when requested, a Rumbling Baloth for P1. P0 is active and holds
/// priority.
fn primal_might_board(
    with_opponent_creature: bool,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let baloth =
        with_opponent_creature.then(|| scenario.add_creature(P1, "Rumbling Baloth", 4, 4).id());
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Primal Might", false, PRIMAL_MIGHT_TEXT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    (runner, spell, bears, baloth)
}

/// S1. CR 601.2c + CR 115.6: Primal Might's second target is "up to one"; the
/// driver declares only the first (an optional slot with no declared intent is
/// answered with no target), so nothing fights: the Bears is not dealt damage,
/// does not fight itself (CR 701.14c), and the Baloth is untouched.
///
/// Fails on revert: the second slot is required, so the cast cannot complete
/// with only the Bears declared.
#[test]
fn primal_might_second_target_declined_fights_nothing() {
    let (mut runner, spell, bears, baloth) = primal_might_board(true);
    let baloth = baloth.expect("the board has an opponent creature");

    let outcome = runner.cast(spell).target_objects(&[bears]).resolve();

    assert_resolved_to_priority(&outcome);
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Graveyard,
        "REACH GUARD: the spell resolved"
    );
    assert!(
        fight_resolved(&outcome, spell),
        "CR 115.6 + CR 701.14b: the fight instruction resolves with nothing to fight \
         (events were {:?})",
        outcome.events()
    );
    assert_untouched(&outcome, &[bears, baloth]);
}

/// S2. CR 115.6: with no creature P0 doesn't control, zero targets is a legal
/// choice for "up to one", so Primal Might is castable for its first target alone.
///
/// Fails on revert: the second slot is required with no legal target, so the
/// engine does not issue the cast.
#[test]
fn primal_might_is_castable_with_nothing_to_fight() {
    let (runner, spell, _, _) = primal_might_board(false);
    assert!(
        cast_is_offered(runner.state(), spell),
        "CR 115.6: \"up to one\" has a legal answer, so the cast is offered"
    );
}
