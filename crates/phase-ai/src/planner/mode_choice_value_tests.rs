//! A modal choice whose modes the leaf cannot tell apart is priced by what
//! each mode buys (`simforge/docs/spec-ai-modal-choice-pricing-2026-10-20.md`;
//! judge F1 and rows JD1, JD2 on 9666a59d6, `fork-checks/9666a59d6/JUDGE.md`).
//!
//! Since 9666a59d6 the move-ordering prior no longer leaks into node values,
//! and Divining Duelist's three enters modes search to the same value: the
//! leaf has no tapped-status term and the next combat is past the horizon.
//! With no policy pricing the modes, the root breaks the tie by action order
//! and loots. These rows read the root at the mode prompt, through the
//! production scorer (`score_candidates`) and the root tactical score, so a
//! mode is chosen by what it buys:
//!
//! - **Tap** an opposing creature: the combat it is removed from. CR 502.3:
//!   only the active player's permanents untap, so a creature tapped on its
//!   controller's turn stays tapped through the AI's next combat, and one
//!   tapped after the AI's own combat untaps before the next.
//! - **Untap** the AI's own creature: the attack or block it enables this
//!   turn cycle.
//! - **Loot**: the card quality exchanged, read from the keep order
//!   (`card_value`).
//!
//! A mode that buys nothing scores 0.
//!
//! Card text verified against MTGJSON AtomicCards (`data/mtgjson`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

use super::PlannerServices;
use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::policies::context::SearchDepth;

/// Divining Duelist ({2}{U}, 3/2), MTGJSON AtomicCards.
const DUELIST: &str = "Flash\nWhen this creature enters, choose one —\n• Tap target creature.\n• Untap target creature.\n• Draw a card, then discard a card.";
/// Dream's Grip ({U} instant), MTGJSON AtomicCards.
const DREAMS_GRIP: &str = "Choose one —\n• Tap target permanent.\n• Untap target permanent.\nEntwine {1} (Choose both if you pay the entwine cost.)";
/// Izzet Charm ({U}{R} instant), MTGJSON AtomicCards.
const IZZET_CHARM: &str = "Choose one —\n• Counter target noncreature spell unless its controller pays {2}.\n• Izzet Charm deals 2 damage to target creature.\n• Draw two cards, then discard two cards.";

/// Duelist's mode indices, in card-text order.
const TAP: usize = 0;
const UNTAP: usize = 1;
const LOOT: usize = 2;

fn measurement() -> AiConfig {
    create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42)
}

/// Whose turn the Duelist is flashed in on, and in which step.
#[derive(Clone, Copy)]
enum When {
    /// P0's own turn, at `phase`.
    Own(Phase),
    /// P1's turn, at `phase`, with P0 holding priority.
    Opponents(Phase),
}

/// Stock both libraries so no line is a deck-out loss (judge S3 on 9666a59d6).
fn stock_libraries(s: &mut GameScenario, own_top: &str) {
    for _ in 0..20 {
        s.add_land_to_library_top(P0, own_top);
        s.add_land_to_library_top(P1, "Plains");
    }
}

fn add_duelist(s: &mut GameScenario) -> ObjectId {
    s.add_creature_to_hand_from_oracle(P0, "Divining Duelist", 3, 2, DUELIST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id()
}

fn build_at(s: GameScenario, when: When) -> GameState {
    let mut st = s.build().state().clone();
    match when {
        When::Own(phase) => st.phase = phase,
        When::Opponents(phase) => {
            st.phase = phase;
            st.active_player = P1;
            st.priority_player = P0;
            st.waiting_for = WaitingFor::Priority { player: P0 };
        }
    }
    st
}

/// Cast `spell` and pass until P0 is at its mode prompt: a modal spell's
/// `ModeChoice` at the cast (CR 601.2b), a modal trigger's
/// `AbilityModeChoice` when it is put on the stack (CR 603.3c).
fn to_mode_prompt(state: GameState, spell: ObjectId) -> GameState {
    let card_id = state.objects[&spell].card_id;
    let mut r = GameRunner::from_state(state);
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: Vec::new(),
        payment_mode: Default::default(),
    })
    .expect("cast");
    for _ in 0..8 {
        if matches!(
            r.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. } | WaitingFor::ModeChoice { .. }
        ) {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. } | WaitingFor::ModeChoice { .. }
        ),
        "reach-guard: mode prompt, got {}",
        r.state().waiting_for.variant_name()
    );
    r.state().clone()
}

/// Production root scores of the single-mode candidates, by mode index.
fn root_scores(state: &GameState) -> Vec<(usize, f64)> {
    let mut v: Vec<(usize, f64)> = crate::search::score_candidates(state, P0, &measurement())
        .into_iter()
        .filter_map(|(a, s)| match a {
            GameAction::SelectModes { indices } if indices.len() == 1 => Some((indices[0], s)),
            _ => None,
        })
        .collect();
    v.sort_by_key(|x| x.0);
    v
}

/// Root tactical score (the policy sum the root blends in) of each
/// single-mode candidate, by mode index.
fn root_tactical(state: &GameState) -> Vec<(usize, f64)> {
    let cfg = measurement();
    let policies = crate::policies::PolicyRegistry::shared();
    let context = AiContext::empty(&cfg.weights);
    let mut services = PlannerServices::new(P0, &cfg, policies, context);
    let ctx = services.build_decision_context_at(None, state);
    let mut v: Vec<(usize, f64)> = ctx
        .candidates
        .iter()
        .filter_map(|c| match &c.action {
            GameAction::SelectModes { indices } if indices.len() == 1 => Some((
                indices[0],
                services.tactical_score(state, &ctx, c, P0, SearchDepth::Root),
            )),
            _ => None,
        })
        .collect();
    v.sort_by_key(|x| x.0);
    v
}

fn of(scores: &[(usize, f64)], mode: usize) -> f64 {
    scores
        .iter()
        .find(|x| x.0 == mode)
        .unwrap_or_else(|| panic!("mode {mode} offered: {scores:?}"))
        .1
}

fn chosen(scores: &[(usize, f64)]) -> usize {
    scores
        .iter()
        .copied()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("a mode")
        .0
}

/// The JD1 table: P0's precombat main, three Islands, an untapped 2/2 and a
/// tapped 2/2; P1 an untapped 3/3, the only blocker. A typeless card in hand.
fn jd1_prompt() -> GameState {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut s, "Island");
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    s.add_creature(P0, "Bear", 2, 2);
    let tapped = s.add_creature(P0, "Tired Bear", 2, 2).id();
    s.add_creature(P1, "Ogre", 3, 3);
    s.add_card_to_hand(P0, "Plains");
    let duelist = add_duelist(&mut s);
    let mut st = s.build().state().clone();
    st.objects.get_mut(&tapped).unwrap().tapped = true;
    to_mode_prompt(st, duelist)
}

/// JD1 (judge row, F1): the three modes SHALL NOT tie exactly at the root,
/// and tapping P1's only untapped blocker in P0's precombat main (the 2/2
/// attacks unblocked) SHALL score above untapping P0's tapped 2/2 (a second
/// attacker the 3/3 still stops). Red at 9666a59d6: tap 37.930 = untap 37.930.
#[test]
fn jd1_duelist_modes_do_not_tie_and_tap_beats_untap_in_main_phase() {
    let prompt = jd1_prompt();
    let sc = root_scores(&prompt);
    eprintln!("JD1 root {sc:?} tactical {:?}", root_tactical(&prompt));
    let mut vals: Vec<f64> = sc.iter().map(|x| x.1).collect();
    vals.sort_by(f64::total_cmp);
    assert!(
        vals.windows(2).all(|w| (w[1] - w[0]).abs() > 1e-9),
        "modes tie exactly: {sc:?}"
    );
    assert!(
        of(&sc, TAP) > of(&sc, UNTAP),
        "tapping the only blocker SHALL score above untapping a second attacker: {sc:?}"
    );
}

/// The JD2 table: Duelist flashed in at P1's end step. P0 an untapped 2/2,
/// P1 an untapped 3/3, a typeless card in P0's hand.
fn jd2_prompt() -> GameState {
    let mut s = GameScenario::new();
    stock_libraries(&mut s, "Island");
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    s.add_creature(P0, "Bear", 2, 2);
    s.add_creature(P1, "Ogre", 3, 3);
    s.add_card_to_hand(P0, "Plains");
    let duelist = add_duelist(&mut s);
    to_mode_prompt(build_at(s, When::Opponents(Phase::End)), duelist)
}

/// JD2 (judge row, F1): at P1's end step, tapping P1's 3/3 (it stays tapped
/// through P0's next combat, CR 502.3) SHALL score above untapping P0's
/// creature, which is already untapped. Red at 9666a59d6: all three 29.992.
#[test]
fn jd2_duelist_at_opponents_end_step_tap_beats_untap() {
    let prompt = jd2_prompt();
    let sc = root_scores(&prompt);
    eprintln!("JD2 root {sc:?} tactical {:?}", root_tactical(&prompt));
    assert!(
        of(&sc, TAP) > of(&sc, UNTAP),
        "tap SHALL score above an untap with nothing to untap: {sc:?}"
    );
}

/// P0 holds an Island (on par with its Island draws, so a loot exchanges
/// nothing); P0 an untapped 2/2, P1 an untapped 3/3 that blocks it.
fn blocker_board(when: When) -> GameState {
    let mut s = GameScenario::new();
    stock_libraries(&mut s, "Island");
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    s.add_creature(P0, "Bear", 2, 2);
    s.add_creature(P1, "Ogre", 3, 3);
    s.add_land_to_hand(P0, "Island");
    let duelist = add_duelist(&mut s);
    to_mode_prompt(build_at(s, when), duelist)
}

/// Done-when 2, first half: Duelist entering on the opponent's end step with
/// an opposing untapped creature that would block the AI's attacker next
/// turn. Tap SHALL outscore loot (here the loot swaps an Island for an
/// Island), at the policy and at the root, and is chosen.
#[test]
fn tap_at_opponents_end_step_outscores_loot() {
    let prompt = blocker_board(When::Opponents(Phase::End));
    let tactical = root_tactical(&prompt);
    let sc = root_scores(&prompt);
    eprintln!("opp end: root {sc:?} tactical {tactical:?}");
    assert!(
        of(&tactical, TAP) > of(&tactical, LOOT),
        "tap SHALL outscore loot: {tactical:?}"
    );
    assert_eq!(chosen(&sc), TAP, "tap SHALL be chosen: {sc:?}");
}

/// Done-when 2, second half: the same board at the AI's own end step. The
/// 3/3 untaps in P1's untap step before it could block (CR 502.3), so tap
/// SHALL score 0: no more than a mode that buys nothing (the untap, every
/// P0 creature already untapped), and below the same tap a turn earlier.
#[test]
fn tap_at_own_end_step_scores_zero() {
    let own = root_tactical(&blocker_board(When::Own(Phase::End)));
    let opp = root_tactical(&blocker_board(When::Opponents(Phase::End)));
    eprintln!("own end tactical {own:?}; opp end tactical {opp:?}");
    assert!(
        (of(&own, TAP) - of(&own, UNTAP)).abs() < 1e-9,
        "tap at the AI's own end step SHALL buy nothing: {own:?}"
    );
    assert!(
        of(&opp, TAP) > of(&own, TAP),
        "the tap that holds a blocker down SHALL outscore the one that does not: own {own:?} opp {opp:?}"
    );
}

/// No creature worth tapping or untapping: P1 has none, P0 only the Duelist.
/// `dead_hand` holds two typeless cards (worth nothing, `card_value`), else
/// two 4/4 creature cards (keepers). The library is `library_top`: Islands
/// (an average draw on the land price), or typeless cards (draws worth less
/// than every keeper, under the keep floor).
fn loot_board(dead_hand: bool, junk_library: bool) -> GameState {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..20 {
        if junk_library {
            s.add_card_to_library_top(P0, "Blank");
        } else {
            s.add_land_to_library_top(P0, "Island");
        }
        s.add_land_to_library_top(P1, "Plains");
    }
    for _ in 0..3 {
        s.add_basic_land(P0, ManaColor::Blue);
    }
    for _ in 0..2 {
        if dead_hand {
            s.add_card_to_hand(P0, "Blank");
        } else {
            s.add_creature_to_hand(P0, "Keeper", 4, 4);
        }
    }
    let duelist = add_duelist(&mut s);
    to_mode_prompt(build_at(s, When::Own(Phase::PreCombatMain)), duelist)
}

/// Done-when 3, first half: no creature worth tapping or untapping and a
/// hand with a dead card. Loot SHALL outscore tap and untap at the policy
/// and SHALL be chosen.
#[test]
fn loot_with_a_dead_card_and_nothing_to_tap_is_chosen() {
    let prompt = loot_board(true, false);
    let tactical = root_tactical(&prompt);
    let sc = root_scores(&prompt);
    eprintln!("dead hand: root {sc:?} tactical {tactical:?}");
    assert!(
        of(&tactical, LOOT) > of(&tactical, TAP) && of(&tactical, LOOT) > of(&tactical, UNTAP),
        "loot SHALL outscore modes that buy nothing: {tactical:?}"
    );
    assert_eq!(chosen(&sc), LOOT, "loot SHALL be chosen: {sc:?}");
}

/// Done-when 3, second half: a hand of keepers and a library of cards under
/// the keep floor. The card a loot would give up is a keeper or the draw
/// itself, so the loot SHALL score low: no more than a mode that buys
/// nothing (the untap: every P0 creature is untapped; the tap is not the
/// reference, `anti_self_harm` already prices a tap whose only targets are
/// P0's own), and below the loot of a dead card.
#[test]
fn loot_with_a_hand_of_keepers_and_a_junk_library_scores_low() {
    let keepers = root_tactical(&loot_board(false, true));
    let dead = root_tactical(&loot_board(true, false));
    eprintln!("keepers {keepers:?}; dead {dead:?}");
    assert!(
        of(&keepers, LOOT) <= of(&keepers, UNTAP) + 1e-9,
        "a loot that can only give up a keeper or a junk draw SHALL buy nothing: {keepers:?}"
    );
    assert!(
        of(&dead, LOOT) > of(&keepers, LOOT),
        "the dead-card loot SHALL outscore the keeper loot: dead {dead:?} keepers {keepers:?}"
    );
}

/// Done-when 4: the reading is generic. Dream's Grip, a modal SPELL (the
/// `ModeChoice` path, not the Duelist's triggered `AbilityModeChoice`), whose
/// modes target any permanent: cast at P1's end step with P1's untapped 3/3
/// that would block P0's 2/2 next turn. Tap SHALL outscore untap (P0's
/// creature is already untapped).
#[test]
fn dreams_grip_tap_mode_is_priced_like_the_duelist() {
    let mut s = GameScenario::new();
    stock_libraries(&mut s, "Island");
    s.add_basic_land(P0, ManaColor::Blue);
    s.add_creature(P0, "Bear", 2, 2);
    s.add_creature(P1, "Ogre", 3, 3);
    let grip = s
        .add_spell_to_hand_from_oracle(P0, "Dream's Grip", true, DREAMS_GRIP)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let prompt = to_mode_prompt(build_at(s, When::Opponents(Phase::End)), grip);
    let tactical = root_tactical(&prompt);
    eprintln!("Dream's Grip tactical {tactical:?}");
    assert!(
        of(&tactical, 0) > of(&tactical, 1),
        "Dream's Grip's tap SHALL outscore its untap: {tactical:?}"
    );
}

/// Done-when 4, loot leg: Izzet Charm's "draw two cards, then discard two
/// cards" is a loot of two. With two dead cards in hand it SHALL outscore
/// the same loot with two keepers in hand.
#[test]
fn izzet_charm_loot_mode_reads_the_hand() {
    let prompt = |dead_hand: bool| {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        stock_libraries(&mut s, "Island");
        s.add_basic_land(P0, ManaColor::Blue);
        s.add_basic_land(P0, ManaColor::Red);
        s.add_creature(P1, "Ogre", 3, 3);
        for _ in 0..2 {
            if dead_hand {
                s.add_card_to_hand(P0, "Blank");
            } else {
                s.add_creature_to_hand(P0, "Keeper", 4, 4);
            }
        }
        let charm = s
            .add_spell_to_hand_from_oracle(P0, "Izzet Charm", true, IZZET_CHARM)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
                generic: 0,
            })
            .id();
        to_mode_prompt(build_at(s, When::Own(Phase::PreCombatMain)), charm)
    };
    let dead = root_tactical(&prompt(true));
    let keepers = root_tactical(&prompt(false));
    eprintln!("Izzet Charm dead {dead:?} keepers {keepers:?}");
    assert!(
        of(&dead, 2) > of(&keepers, 2),
        "the loot of two dead cards SHALL outscore the loot of two keepers: dead {dead:?} keepers {keepers:?}"
    );
}
