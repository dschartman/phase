//! CR 201.5c: "Text printed on some cards refers to that card by a shortened
//! version of its name. Instances of a card's shortened name used in this
//! manner are treated as though they used the card's full name."
//!
//! A legendary card whose short name is also a planeswalker type (CR 205.3j)
//! — Liliana the Repentant's "Put a +1/+1 counter on Liliana" — must read the
//! short name as the card itself, not as "objects with the Liliana subtype".

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AbilityTag;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Liliana the Repentant (verbatim Oracle text).
const LILIANA_THE_REPENTANT: &str = "Whenever another creature or planeswalker you control enters, mill two cards.\nExhaust — {5}{B}: Return target creature or planeswalker card from your graveyard to the battlefield. Put a +1/+1 counter on Liliana. Activate only as a sorcery. (Activate each exhaust ability only once.)";

fn add_black_mana(runner: &mut GameRunner, n: usize) {
    for _ in 0..n {
        let unit = ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// CR 201.5c + CR 122.1: Liliana's exhaust returns the target card and puts
/// the +1/+1 counter on Liliana herself.
#[test]
fn liliana_the_repentant_exhaust_puts_the_counter_on_liliana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let liliana = scenario
        .add_creature_from_oracle(P0, "Liliana the Repentant", 2, 2, LILIANA_THE_REPENTANT)
        .id();
    let returned = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear", 2, 2)
        .id();
    let mut runner = scenario.build();
    add_black_mana(&mut runner, 6);

    let index = runner.state().objects[&liliana]
        .abilities
        .iter()
        .position(|a| a.ability_tag == Some(AbilityTag::Exhaust))
        .expect("Liliana carries an Exhaust-tagged ability");
    runner
        .activate(liliana, index)
        .target_object(returned)
        .resolve();

    // Reach guard: the exhaust ability resolved its first instruction.
    assert_eq!(
        runner.state().objects[&returned].zone,
        Zone::Battlefield,
        "the target card returns to the battlefield"
    );
    assert_eq!(
        runner.state().objects[&liliana]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "CR 201.5c: \"Liliana\" is Liliana the Repentant, so she gets the counter"
    );
    assert_eq!(
        runner.state().objects[&returned]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "the returned creature gets no counter"
    );
}
