//! CR 608.2n: "As the final part of an instant or sorcery spell's resolution,
//! the spell is put into its owner's graveyard." A resolution that pauses for a
//! choice (a creature type, a surveil split, a "may" choice, any prompt the
//! resolution opens) has not reached its final part, so the spell is still on
//! the stack while the choice is pending, and a later instruction that counts
//! the cards in a graveyard does not see it there.
//!
//! Upstream holds the move back for every pause kind (#9728, the carrier's
//! owed final part); these rows are the fork's guard over that mechanism. Each
//! drives a real card's verbatim Oracle text through the cast pipeline and
//! answers its prompt with a typed `GameAction`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Kindred Judgment, verbatim (MTGJSON).
const KINDRED_JUDGMENT: &str =
    "Choose a creature type. Destroy all creatures that aren't of the chosen type.";

/// Peer Review, verbatim (MTGJSON, the prepare face of Semester Foreseer).
const PEER_REVIEW: &str =
    "Create a 2/2 colorless Wizard Soldier creature token named Cadet. Surveil 1.";

/// Grave Consequences, verbatim (MTGJSON).
const GRAVE_CONSEQUENCES: &str = "Each player may exile any number of cards from their graveyard. Then each player loses 1 life for each card in their graveyard.\nDraw a card.";

fn with_p0_priority(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn in_graveyard(runner: &GameRunner, player: PlayerId, id: ObjectId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .graveyard
        .contains(&id)
}

fn graveyard_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .graveyard
        .len()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// Commit the cast, then pass priority until the stack resolves the spell and
/// the resolution opens a prompt (or the pass loop ends).
fn cast_and_resolve_to_prompt(runner: &mut GameRunner, spell: ObjectId) {
    let _ = runner.cast(spell).commit();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
    }
}

/// CR 608.2n + CR 608.2c: Kindred Judgment pauses on its creature-type choice
/// before its destroy instruction. While that choice is pending the spell is
/// still resolving, so it is on the stack and not in the graveyard; it is put
/// there only after the destroy, as the final part of the resolution.
#[test]
fn kindred_judgment_stays_on_stack_while_its_creature_type_choice_is_pending() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let goblin = scenario
        .add_creature(P0, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let elf = scenario
        .add_creature(P1, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let judgment = scenario
        .add_spell_to_hand_from_oracle(P0, "Kindred Judgment", false, KINDRED_JUDGMENT)
        .id();
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    cast_and_resolve_to_prompt(&mut runner, judgment);
    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!(
            "Kindred Judgment must pause on its creature-type choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(options.iter().any(|o| o == "Goblin"), "Goblin is offered");
    assert_eq!(
        zone(&runner, judgment),
        Zone::Stack,
        "CR 608.2n: the spell is still resolving while its choice is pending"
    );
    assert!(
        !in_graveyard(&runner, P0, judgment),
        "CR 608.2n: the spell is not in its owner's graveyard before its final part"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: "Goblin".to_string(),
        })
        .expect("the creature-type answer is accepted");

    assert_eq!(zone(&runner, elf), Zone::Graveyard, "the non-Goblin dies");
    assert_eq!(zone(&runner, goblin), Zone::Battlefield, "the Goblin lives");
    assert_eq!(
        zone(&runner, judgment),
        Zone::Graveyard,
        "CR 608.2n: the spell is put into its owner's graveyard as the final part"
    );
    assert!(in_graveyard(&runner, P0, judgment));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 608.2n + CR 701.25a: Peer Review pauses on its surveil split, a different
/// prompt kind. The spell is on the stack while the split is pending and goes
/// to the graveyard after it, alongside the surveilled card.
#[test]
fn peer_review_stays_on_stack_while_its_surveil_choice_is_pending() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let top = scenario.add_card_to_library_top(P0, "Island");
    let review = scenario
        .add_spell_to_hand_from_oracle(P0, "Peer Review", false, PEER_REVIEW)
        .id();
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    cast_and_resolve_to_prompt(&mut runner, review);
    let WaitingFor::SurveilChoice { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Peer Review must pause on its surveil choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(cards, &vec![top], "surveil 1 looks at the top card");
    assert_eq!(
        zone(&runner, review),
        Zone::Stack,
        "CR 608.2n: the spell is still resolving while its surveil choice is pending"
    );
    assert!(!in_graveyard(&runner, P0, review));

    // CR 701.25a: the payload is the keep-on-top set; an empty set puts the
    // looked-at card into the graveyard.
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("the surveil split is accepted");

    assert_eq!(
        zone(&runner, top),
        Zone::Graveyard,
        "surveilled into the graveyard"
    );
    assert_eq!(
        zone(&runner, review),
        Zone::Graveyard,
        "CR 608.2n: the spell is put into its owner's graveyard as the final part"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 608.2n + CR 608.2h: Grave Consequences counts each player's graveyard
/// after the players choose what to exile from it. The count is taken while
/// the spell is still on the stack, so its caster loses life for the cards
/// already there and not for Grave Consequences itself.
#[test]
fn grave_consequences_graveyard_count_does_not_include_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P0, &["Island", "Forest"]);
    scenario.with_graveyard(P1, &["Swamp"]);
    scenario.add_card_to_library_top(P0, "Plains");
    let consequences = scenario
        .add_spell_to_hand_from_oracle(P0, "Grave Consequences", true, GRAVE_CONSEQUENCES)
        .id();
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);
    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    cast_and_resolve_to_prompt(&mut runner, consequences);
    // Each player declines to exile anything from their graveyard. Record where
    // the spell is at each prompt; the count is asserted first so a red run
    // shows the off-by-one itself.
    let mut zones_at_prompts = Vec::new();
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => break,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: false }
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        zones_at_prompts.push(zone(&runner, consequences));
        runner.act(action).expect("the answer is accepted");
    }
    assert!(
        !zones_at_prompts.is_empty(),
        "reach: the resolution paused for each player's choice"
    );

    assert_eq!(
        p0_life - life(&runner, P0),
        2,
        "CR 608.2n + CR 608.2h: P0 loses 1 life for each of the two cards in their graveyard, \
         not for the resolving spell"
    );
    assert_eq!(
        p1_life - life(&runner, P1),
        1,
        "P1 loses 1 for its one card"
    );
    assert!(
        zones_at_prompts.iter().all(|z| *z == Zone::Stack),
        "CR 608.2n: the spell is on the stack at every prompt, got {zones_at_prompts:?}"
    );
    assert_eq!(zone(&runner, consequences), Zone::Graveyard);
    assert_eq!(
        graveyard_len(&runner, P0),
        3,
        "the two cards plus the spell"
    );
}

/// One row of the class sweep below: a printed instant or sorcery whose
/// resolution opens a prompt, with the player it targets (if any).
struct PausingSpell {
    name: &'static str,
    instant: bool,
    oracle: &'static str,
    target: Option<PlayerId>,
}

/// Verbatim Oracle text (MTGJSON) for spells whose resolution pauses on a
/// different prompt kind each: a library search, a dig, a scry before a draw,
/// a discard choice, an opponent's sacrifice choice, a hand reveal and pick,
/// an opponent's pile split.
const PAUSING_SPELLS: &[PausingSpell] = &[
    PausingSpell {
        name: "Demonic Tutor",
        instant: false,
        oracle: "Search your library for a card, put that card into your hand, then shuffle.",
        target: None,
    },
    PausingSpell {
        name: "Impulse",
        instant: true,
        oracle: "Look at the top four cards of your library. Put one of them into your hand and the rest on the bottom of your library in any order.",
        target: None,
    },
    PausingSpell {
        name: "Preordain",
        instant: false,
        oracle: "Scry 2, then draw a card. (To scry 2, look at the top two cards of your library, then put any number of them on the bottom and the rest on top in any order.)",
        target: None,
    },
    PausingSpell {
        name: "Careful Study",
        instant: false,
        oracle: "Draw two cards, then discard two cards.",
        target: None,
    },
    PausingSpell {
        name: "Diabolic Edict",
        instant: true,
        oracle: "Target player sacrifices a creature of their choice.",
        target: Some(P1),
    },
    PausingSpell {
        name: "Duress",
        instant: false,
        oracle: "Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card.",
        target: Some(P1),
    },
    PausingSpell {
        name: "Fact or Fiction",
        instant: true,
        oracle: "Reveal the top five cards of your library. An opponent separates those cards into two piles. Put one pile into your hand and the other into your graveyard.",
        target: None,
    },
];

/// CR 608.2n + CR 608.2c, the class: whatever prompt a resolution opens, the
/// spell is on the stack at every prompt, and once the resolution ends (the
/// first priority window with an empty stack) it is in its owner's graveyard,
/// never left behind on the stack. Each prompt is answered with the first
/// legal action the engine offers.
#[test]
fn a_spell_paused_on_any_resolution_prompt_reaches_its_graveyard_when_resolution_ends() {
    for row in PAUSING_SPELLS {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for player in [P0, P1] {
            for name in ["Island", "Swamp", "Forest", "Plains", "Mountain", "Island"] {
                scenario.add_card_to_library_top(player, name);
            }
            scenario.add_card_to_hand(player, "Lightning Bolt");
            scenario.add_card_to_hand(player, "Divination");
        }
        scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"]);
        scenario.add_creature(P1, "Hill Giant", 3, 3);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, row.name, row.instant, row.oracle)
            .id();
        let mut runner = scenario.build();
        with_p0_priority(&mut runner);

        let cast = runner.cast(spell);
        let cast = match row.target {
            Some(player) => cast.target_player(player),
            None => cast,
        };
        let _ = cast.commit();
        let mut prompts = Vec::new();
        for _ in 0..32 {
            let state = runner.state();
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                if state.stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is accepted");
                continue;
            }
            assert_eq!(
                zone(&runner, spell),
                Zone::Stack,
                "{}: CR 608.2n: the spell is still resolving at {:?}",
                row.name,
                state.waiting_for
            );
            prompts.push(
                format!("{:?}", state.waiting_for)
                    .chars()
                    .take(40)
                    .collect::<String>(),
            );
            let action = engine::ai_support::legal_actions(state)
                .into_iter()
                .find(|action| !matches!(action, GameAction::PassPriority))
                .unwrap_or_else(|| panic!("{}: no answer to {:?}", row.name, state.waiting_for));
            runner.act(action).expect("a legal answer is accepted");
        }
        assert!(
            !prompts.is_empty(),
            "{}: reach: the resolution paused for a choice",
            row.name
        );
        assert!(
            runner.state().stack.is_empty(),
            "{}: the stack is empty",
            row.name
        );
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{}: back at a priority window",
            row.name
        );
        assert_eq!(
            zone(&runner, spell),
            Zone::Graveyard,
            "{}: CR 608.2n: put into its owner's graveyard as the final part, after prompts {prompts:?}",
            row.name
        );
        assert!(
            runner.state().resolving_stack_entry.is_none(),
            "{}: CR 608.2c: the resolution carrier has settled",
            row.name
        );
    }
}

/// CR 800.4a + CR 608.2n: when a player leaves a multiplayer game while their
/// own spell is paused on their own choice, that resolution is abandoned, and
/// the spell, like every other object they own, leaves the game (it is
/// exiled). The held-back move off the stack is delivered as the resolution is
/// abandoned, so the departing owner's sweep finds the spell and it is not left
/// in the stack zone with no stack entry.
#[test]
fn a_paused_spell_of_a_player_who_leaves_the_game_is_exiled_with_their_objects() {
    use engine::game::elimination::eliminate_player;

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P1, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"]);
    let judgment = scenario
        .add_spell_to_hand_from_oracle(P0, "Kindred Judgment", false, KINDRED_JUDGMENT)
        .id();
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);

    cast_and_resolve_to_prompt(&mut runner, judgment);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "reach: Kindred Judgment is paused on its caster's creature-type choice"
    );
    assert_eq!(zone(&runner, judgment), Zone::Stack);

    eliminate_player(runner.state_mut(), P0, &mut Vec::new());

    assert_eq!(
        zone(&runner, judgment),
        Zone::Exile,
        "CR 800.4a: the departed owner's spell leaves the game with their other objects"
    );
    assert!(runner.state().resolving_stack_entry.is_none());
}

/// Rest in Peace, verbatim (MTGJSON).
const REST_IN_PEACE: &str = "When Rest in Peace enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

/// Diabolic Edict, verbatim (MTGJSON).
const DIABOLIC_EDICT: &str = "Target player sacrifices a creature of their choice.";

/// Three players; P0 casts Diabolic Edict targeting P1 and the resolution pauses
/// on P1's sacrifice choice. P2 controls Rest in Peace when `rest_in_peace`.
fn edict_paused_on_p1_choice(rest_in_peace: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    if rest_in_peace {
        scenario.add_enchantment_from_oracle(PlayerId(2), "Rest in Peace", REST_IN_PEACE);
    }
    let edict = scenario
        .add_spell_to_hand_from_oracle(P0, "Diabolic Edict", true, DIABOLIC_EDICT)
        .id();
    let mut runner = scenario.build();
    with_p0_priority(&mut runner);
    let _ = runner.cast(edict).target_player(P1).commit();
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player, .. } if player == P1
        ),
        "reach: the Edict is paused on P1's sacrifice choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, edict), Zone::Stack);
    (runner, edict)
}

/// CR 800.4a: the spell's owner leaves the game while another player answers
/// the spell's prompt. The spell is an object that player owns, so it leaves
/// the game with the rest of their objects (it is exiled), even though it has
/// been popped from the stack list while it resolves.
#[test]
fn a_paused_spell_whose_owner_leaves_at_another_players_prompt_is_exiled() {
    use engine::game::elimination::eliminate_player;

    let (mut runner, edict) = edict_paused_on_p1_choice(false);
    eliminate_player(runner.state_mut(), P0, &mut Vec::new());
    // P1 still answers the sacrifice; the resolution then ends.
    for _ in 0..8 {
        let state = runner.state();
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
            break;
        }
        let action = engine::ai_support::legal_actions(state)
            .into_iter()
            .find(|action| !matches!(action, GameAction::PassPriority))
            .unwrap_or(GameAction::PassPriority);
        let _ = runner.act(action);
    }
    assert_eq!(
        zone(&runner, edict),
        Zone::Exile,
        "CR 800.4a: the departed owner's spell leaves the game with their other objects"
    );
}

/// CR 800.4a + CR 608.2n + CR 614.1a: the player answering the spell's prompt
/// leaves the game at that prompt (a concession, through the engine's action
/// boundary). The spell's owner stays, so the spell is not left in the stack
/// zone: it is put where the rules put it, its owner's graveyard, or exile
/// under Rest in Peace.
#[test]
fn a_paused_spell_whose_chooser_leaves_at_their_own_prompt_reaches_its_zone() {
    for (rest_in_peace, expected) in [(false, Zone::Graveyard), (true, Zone::Exile)] {
        let (mut runner, edict) = edict_paused_on_p1_choice(rest_in_peace);
        engine::game::engine::apply(
            runner.state_mut(),
            P1,
            GameAction::Concede { player_id: P1 },
        )
        .expect("a player may concede at any time");
        assert_eq!(
            zone(&runner, edict),
            expected,
            "rest_in_peace={rest_in_peace}: the spell reaches its zone"
        );
    }
}
