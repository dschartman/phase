//! "When one or more of your opponents are dealt combat damage during your
//! turn, …" — the batched player-recipient damage trigger with a turn
//! restriction (Fblthp, Impossibly Lost).
//!
//! Two axes this class needs that the parser did not compose before:
//!   * the partitive subject "one or more OF your opponents" (CR 102.3: an
//!     opponent is any other player), as opposed to "one or more opponents";
//!   * the trailing "during your turn" restriction on a passive damage event,
//!     which must restrict the trigger rather than be dropped or make the
//!     line unparseable.
//!
//! The runtime tests drive real combat damage through the engine so they fail
//! whenever the trigger does not fire, and they check the rest of Fblthp's
//! ability chain resolves the way its Oracle text reads: draw two, win on an
//! empty library (CR 104.2b), otherwise shuffle Fblthp into its owner's library.

use super::rules::run_combat;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ControllerRef, TargetFilter, TriggerConstraint, TypedFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const FBLTHP: &str = "When one or more of your opponents are dealt combat damage during \
    your turn, draw two cards. If your library has no cards in it, you win the game. \
    Fblthp's owner shuffles him into their library. (If you draw from an empty library \
    this way, you still win the game.)";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

/// P0 controls Fblthp and a 2/2 attacker, with `library` cards in its library.
fn setup(library: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..library {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
    let fblthp = scenario
        .add_creature_from_oracle(P0, "Fblthp, Impossibly Lost", 1, 1, FBLTHP)
        .as_legendary()
        .id();
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    (scenario.build(), fblthp, attacker)
}

/// SHAPE: the trigger line parses to a batched, combat-only `DamageReceived`
/// trigger on opponents, restricted to its controller's turn.
#[test]
fn one_or_more_of_your_opponents_dealt_combat_damage_during_your_turn_parses() {
    let parsed = parse_oracle_text(
        FBLTHP,
        "Fblthp, Impossibly Lost",
        &[],
        &["Creature".to_string()],
        &["Homunculus".to_string()],
    );
    let trigger = parsed.triggers.first().expect("Fblthp has one trigger");
    assert_eq!(trigger.mode, TriggerMode::DamageReceived);
    assert_eq!(
        trigger.valid_target,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent)
        )),
        "CR 102.3: the damaged players are opponents of the controller"
    );
    // CR 603.2c: "one or more" is one trigger per damage event, not per player.
    assert!(trigger.batched);
    assert_eq!(
        trigger.constraint,
        Some(TriggerConstraint::OnlyDuringYourTurn),
        "'during your turn' must restrict the trigger, not be dropped"
    );
}

/// CR 510.2 + CR 603.2: combat damage to the opponent during P0's turn fires the
/// trigger; P0 draws two, the library still has cards so nobody wins, and
/// Fblthp is shuffled into its owner's library.
#[test]
fn fblthp_draws_two_and_shuffles_himself_into_library() {
    let (mut runner, fblthp, attacker) = setup(5);
    let hand_before = hand_len(&runner, P0);
    let p1_life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "precondition: P1 was dealt 2 combat damage"
    );
    assert_eq!(hand_len(&runner, P0), hand_before + 2, "draw two cards");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "the library still had cards, so the win clause must not apply"
    );
    assert_eq!(
        runner.state().objects[&fblthp].zone,
        Zone::Library,
        "Fblthp's owner shuffles him into their library"
    );
    // 5 - 2 drawn + Fblthp.
    assert_eq!(library_len(&runner, P0), 4);
}

/// CR 104.2b + CR 121.4: with one card left, the second draw is from an empty
/// library; the ability's own win clause applies while it resolves, before
/// state-based actions could make P0 lose for that draw (CR 704.5b).
#[test]
fn fblthp_wins_when_the_draws_empty_the_library() {
    let (mut runner, _fblthp, attacker) = setup(1);

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "P0 wins the game, got {:?}",
        runner.state().waiting_for
    );
}
