//! CR 700.2a + CR 603.3c: a modal choice is priced by what each mode buys.
//!
//! At a mode prompt (`WaitingFor::ModeChoice` for a modal spell,
//! `WaitingFor::AbilityModeChoice` for a modal triggered or activated
//! ability) the search often cannot tell modes apart: a tapped blocker, an
//! untapped attacker and a loot can all reach the same leaf, because the leaf
//! has no tapped-status term and the combat where the difference matters is
//! past the horizon (Divining Duelist, `fork-checks/9666a59d6/JUDGE.md`
//! focus 2). This policy gives each mode the value it buys, read from the
//! mode's effects, so the root's tactical blend separates them:
//!
//! - **Tap** an opposing creature (`SetTapState { Single, Tap }`): the combat
//!   it is removed from. CR 502.3: only the active player's permanents untap,
//!   so a creature tapped before its controller's next untap step stays
//!   tapped through every combat until then. It is priced as a blocker in the
//!   AI's next combat, and as an attacker in its controller's combat this
//!   turn if that is still ahead.
//! - **Untap** the AI's own creature (`SetTapState { Single, Untap }`): the
//!   attack it enables in the AI's combat this turn, or the block it enables
//!   in the next opponent's combat before the AI's own untap step.
//! - **Loot** ("draw N cards, then discard N cards"): the card quality
//!   exchanged, `card_value::expected_loot_gain` in expected draws.
//!
//! A mode that buys nothing (a tap that wears off before any combat, an untap
//! of a creature that is untapped or would untap anyway) scores 0. Modes are
//! read structurally from their effects, never by card name; a mode whose
//! effects are none of the three is neutral here.
//!
//! The combat reading is deliberately simple: an attacker deals its power
//! unless some untapped potential blocker *holds* against it (kills it or
//! survives the block, `combat_ai::block_holds`; only a chump does not hold),
//! and a creature with menace needs two potential blockers (CR 702.111b). A
//! mode is worth the change in that damage, at
//! `PolicyPenalties::mode_choice_damage_per_point` a point.
//!
//! The analysis (legal targets, block legality) runs at the root only; in
//! lookahead the policy is neutral, where it would only reorder children.

use engine::game::players::{is_opponent, next_player_in_turn_order};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{
    AbilityDefinition, Effect, EffectScope, QuantityExpr, TapStateChange, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::context::{selected_mode_abilities, PolicyContext};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::card_value::{expected_draw_value, expected_loot_gain};
use crate::combat_ai::{
    block_holds, can_attack, could_attack_if_untapped, BlockLegalitySlices, MarkedDamage,
};
use crate::features::DeckFeatures;

pub struct ModeChoiceValuePolicy;

impl TacticalPolicy for ModeChoiceValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ModeChoiceValue
    }

    /// A `SelectModes` candidate at `ModeChoice` / `AbilityModeChoice` routes
    /// to the ability catch-all (`decision_kind::classify`).
    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: deck-independent; a mode's price is what it buys on this board.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::SelectModes { indices } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("mode_choice_value_na"));
        };
        if !ctx.at_root() {
            return PolicyVerdict::neutral(PolicyReason::new("mode_choice_value_na"));
        }
        let Some(source) = ctx.source_object() else {
            return PolicyVerdict::neutral(PolicyReason::new("mode_choice_value_na"));
        };
        let modes = selected_mode_abilities(ctx.state, &ctx.decision.waiting_for, indices);
        let price = price_modes(ctx, source.id, &modes);
        let penalties = ctx.penalties();
        let value = f64::from(price.damage) * penalties.mode_choice_damage_per_point
            + price.loot_draws * penalties.mode_choice_loot_per_card;
        PolicyVerdict::score(
            value,
            PolicyReason::new("mode_choice_value_priced")
                .with_fact("damage", i64::from(price.damage))
                .with_fact("loot_milli", (price.loot_draws * 1000.0) as i64),
        )
    }
}

/// What the selected modes buy: combat damage opened or prevented, and card
/// quality a loot exchanges, in expected draws.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct ModePrice {
    damage: i32,
    loot_draws: f64,
}

/// CR 700.2a: the selected modes' effects, each priced once; modes add up
/// (a "choose two" choice does both).
fn price_modes(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    modes: &[&AbilityDefinition],
) -> ModePrice {
    let mut price = ModePrice::default();
    for mode in modes {
        let mut node = Some(*mode);
        while let Some(definition) = node {
            node = definition.sub_ability.as_deref();
            if let Some(count) = loot_count(definition) {
                price.loot_draws += loot_draws(ctx, source_id, count);
                // The discard half of the loot is priced with its draw.
                node = node.and_then(|discard| discard.sub_ability.as_deref());
                continue;
            }
            if let Effect::SetTapState {
                target,
                scope: EffectScope::Single,
                state: change,
            } = definition.effect.as_ref()
            {
                price.damage += best_target_damage(ctx, source_id, target, *change);
            }
        }
    }
    price
}

/// CR 121.1 + CR 701.9a: a loot is a draw of N cards for the controller
/// followed by a chosen discard of N cards by the controller, in that order
/// (the discard chooses from a hand that holds the drawn cards). Returns N.
fn loot_count(definition: &AbilityDefinition) -> Option<usize> {
    let Effect::Draw {
        count: QuantityExpr::Fixed { value: draws },
        target: TargetFilter::Controller,
    } = definition.effect.as_ref()
    else {
        return None;
    };
    let discard = definition.sub_ability.as_deref()?;
    let Effect::Discard {
        count: QuantityExpr::Fixed { value: discards },
        target: TargetFilter::Controller,
        selection,
        unless_filter: None,
        filter: None,
    } = discard.effect.as_ref()
    else {
        return None;
    };
    (draws == discards && *draws > 0 && selection.is_chosen() && discard.condition.is_none())
        .then(|| usize::try_from(*draws).ok())
        .flatten()
}

/// A loot of `count` in expected draws: the card quality it exchanges
/// (`expected_loot_gain`) over the value of one expected draw, the unit a
/// discard's price is read in (`self_cost::discard_given_up_cost`). The hand
/// it discards from is the AI's, without the card being cast.
fn loot_draws(ctx: &PolicyContext<'_>, source_id: ObjectId, count: usize) -> f64 {
    let plan = super::self_cost_value::discarding_plan(ctx);
    let pool: Vec<ObjectId> = ctx.state.players[ctx.ai_player.0 as usize]
        .hand
        .iter()
        .copied()
        .filter(|&id| id != source_id)
        .collect();
    let (Some(gain), Some(per_draw)) = (
        expected_loot_gain(ctx.state, ctx.ai_player, &pool, count, plan),
        expected_draw_value(ctx.state, ctx.ai_player, plan),
    ) else {
        return 0.0;
    };
    gain / per_draw
}

/// The most combat damage one legal target of a single tap or untap buys.
fn best_target_damage(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    filter: &TargetFilter,
    change: TapStateChange,
) -> i32 {
    let state = ctx.state;
    let slices = BlockLegalitySlices::collect(state);
    find_legal_targets(state, filter, ctx.ai_player, source_id)
        .into_iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(id),
            TargetRef::Player(_) => None,
        })
        .map(|id| match change {
            TapStateChange::Tap => tap_damage(state, ctx.ai_player, id, &slices),
            TapStateChange::Untap => untap_damage(state, ctx.ai_player, id, &slices),
        })
        .max()
        .unwrap_or(0)
        .max(0)
}

/// When a combat is fought relative to now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CombatTurn {
    /// This turn's combat, still ahead of the step that matters.
    This,
    /// A combat on a later turn: the cleanup step will have removed marked
    /// damage (CR 514.2) and the attacking player's permanents untapped
    /// (CR 502.3).
    Later,
}

impl CombatTurn {
    fn marked(self) -> MarkedDamage {
        match self {
            CombatTurn::This => MarkedDamage::Kept,
            CombatTurn::Later => MarkedDamage::Cleared,
        }
    }
}

/// CR 508.1 + CR 509.1: attackers are declared at the start of the declare
/// attackers step and blockers at the start of the declare blockers step, so
/// at a prompt during the declare attackers step the attack is fixed and the
/// blocks are still ahead.
fn attack_ahead(phase: Phase) -> bool {
    phase < Phase::DeclareAttackers
}

fn blocks_ahead(phase: Phase) -> bool {
    phase <= Phase::DeclareAttackers
}

/// CR 103.1: whether `player` takes a turn after the current one
/// and before `until`'s next turn.
fn turn_comes_before(state: &GameState, player: PlayerId, until: PlayerId) -> bool {
    let mut next = next_player_in_turn_order(state, state.active_player);
    for _ in 0..state.players.len() {
        if next == until {
            return false;
        }
        if next == player {
            return true;
        }
        next = next_player_in_turn_order(state, next);
    }
    false
}

/// The AI's next combat: this turn's while its blocks are still ahead, else
/// its next turn's.
fn ai_next_combat(state: &GameState, ai: PlayerId) -> CombatTurn {
    if state.active_player == ai && blocks_ahead(state.phase) {
        CombatTurn::This
    } else {
        CombatTurn::Later
    }
}

/// The creatures `player` attacks with in a combat: this turn, the declared
/// attackers once the attack is fixed, else those able to attack now; on a
/// later turn, every creature without defender (CR 502.3: they untap, and
/// CR 302.6's summoning sickness has worn off by then).
fn attackers_in(state: &GameState, player: PlayerId, turn: CombatTurn) -> Vec<ObjectId> {
    match turn {
        CombatTurn::This if !attack_ahead(state.phase) => state
            .combat
            .as_ref()
            .map(|combat| {
                combat
                    .attackers
                    .iter()
                    .map(|info| info.object_id)
                    .filter(|id| {
                        state
                            .objects
                            .get(id)
                            .is_some_and(|o| o.controller == player)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        CombatTurn::This => creatures_of(state, player)
            .filter(|&id| can_attack(state, id))
            .collect(),
        CombatTurn::Later => creatures_of(state, player)
            .filter(|&id| {
                state
                    .objects
                    .get(&id)
                    .is_some_and(|o| !o.has_keyword(&Keyword::Defender))
            })
            .collect(),
    }
}

/// CR 509.1a: the creatures that can block for the defenders `defends`
/// picks out: untapped creatures they control. A creature tapped now stays
/// tapped until its controller's untap step, which no window read here
/// reaches.
fn untapped_blockers(state: &GameState, defends: impl Fn(PlayerId) -> bool) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                defends(o.controller)
                    && !o.tapped
                    && o.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .collect()
}

fn creatures_of(state: &GameState, player: PlayerId) -> impl Iterator<Item = ObjectId> + '_ {
    state.battlefield.iter().copied().filter(move |id| {
        state.objects.get(id).is_some_and(|o| {
            o.controller == player && o.card_types.core_types.contains(&CoreType::Creature)
        })
    })
}

/// The damage `attackers` deal without attacking into a block that holds:
/// an attacker counts its power unless some potential blocker can legally
/// block it (CR 509.1b) and holds (`block_holds`); one with menace needs two
/// potential blockers (CR 702.111b) before any of them deters it.
fn safe_damage(
    state: &GameState,
    attackers: &[ObjectId],
    blockers: &[ObjectId],
    slices: &BlockLegalitySlices,
    marked: MarkedDamage,
) -> i32 {
    attackers
        .iter()
        .filter_map(|&id| state.objects.get(&id).map(|o| (id, o)))
        .filter(|(id, attacker)| {
            let legal: Vec<_> = blockers
                .iter()
                .filter(|&&b| b != *id && slices.can_block_pair(state, b, *id))
                .filter_map(|b| state.objects.get(b))
                .collect();
            let enough = if attacker.has_keyword(&Keyword::Menace) {
                legal.len() >= 2
            } else {
                !legal.is_empty()
            };
            !(enough && legal.iter().any(|b| block_holds(b, attacker, marked)))
        })
        .map(|(_, attacker)| attacker.power.unwrap_or(0).max(0))
        .sum()
}

fn without(ids: &[ObjectId], removed: ObjectId) -> Vec<ObjectId> {
    ids.iter().copied().filter(|&id| id != removed).collect()
}

fn with(ids: &[ObjectId], added: ObjectId) -> Vec<ObjectId> {
    let mut out = without(ids, added);
    out.push(added);
    out
}

/// CR 701.26a + CR 502.3: tapping an opposing creature `id`. It stays tapped
/// until its controller's next untap step, so it is removed:
/// - as a blocker from the AI's next combat, unless its controller takes a
///   turn before that combat (a tap after the AI's own blocks untaps first);
/// - as an attacker from its controller's combat this turn, while that
///   attack is still ahead.
///
/// An untapped or AI-controlled creature buys nothing.
fn tap_damage(state: &GameState, ai: PlayerId, id: ObjectId, slices: &BlockLegalitySlices) -> i32 {
    let Some(target) = state.objects.get(&id) else {
        return 0;
    };
    let owner = target.controller;
    if target.tapped
        || !is_opponent(state, ai, owner)
        || !target.card_types.core_types.contains(&CoreType::Creature)
    {
        return 0;
    }
    let mut damage = 0;

    let combat = ai_next_combat(state, ai);
    let stays_tapped = match combat {
        CombatTurn::This => true,
        CombatTurn::Later => !turn_comes_before(state, owner, ai),
    };
    if stays_tapped {
        let attackers = attackers_in(state, ai, combat);
        let blockers = untapped_blockers(state, |p| p == owner);
        damage += safe_damage(
            state,
            &attackers,
            &without(&blockers, id),
            slices,
            combat.marked(),
        ) - safe_damage(state, &attackers, &blockers, slices, combat.marked());
    }

    if state.active_player == owner && attack_ahead(state.phase) && can_attack(state, id) {
        let attackers = attackers_in(state, owner, CombatTurn::This);
        let blockers = untapped_blockers(state, |p| p == ai);
        damage += safe_damage(state, &attackers, &blockers, slices, MarkedDamage::Kept)
            - safe_damage(
                state,
                &without(&attackers, id),
                &blockers,
                slices,
                MarkedDamage::Kept,
            );
    }
    damage
}

/// CR 701.26b: untapping the AI's own tapped creature `id`. It would stay
/// tapped until the AI's next untap step (CR 502.3), so it enables:
/// - an attack in the AI's combat this turn, while the attack is ahead and
///   the creature could attack;
/// - a block in the next opponent's combat before the AI's next turn.
///
/// The two are alternatives (attacking taps it), so the better counts. An
/// untapped or opposing creature buys nothing.
fn untap_damage(
    state: &GameState,
    ai: PlayerId,
    id: ObjectId,
    slices: &BlockLegalitySlices,
) -> i32 {
    let Some(target) = state.objects.get(&id) else {
        return 0;
    };
    if !target.tapped
        || target.controller != ai
        || !target.card_types.core_types.contains(&CoreType::Creature)
    {
        return 0;
    }

    let attack = if state.active_player == ai
        && attack_ahead(state.phase)
        && could_attack_if_untapped(state, id)
    {
        let attackers = attackers_in(state, ai, CombatTurn::This);
        let blockers = untapped_blockers(state, |p| is_opponent(state, ai, p));
        safe_damage(
            state,
            &with(&attackers, id),
            &blockers,
            slices,
            MarkedDamage::Kept,
        ) - safe_damage(state, &attackers, &blockers, slices, MarkedDamage::Kept)
    } else {
        0
    };

    let block = match next_opponent_combat(state, ai) {
        Some((opponent, turn)) => {
            let attackers = attackers_in(state, opponent, turn);
            let blockers = untapped_blockers(state, |p| p == ai);
            safe_damage(state, &attackers, &blockers, slices, turn.marked())
                - safe_damage(
                    state,
                    &attackers,
                    &with(&blockers, id),
                    slices,
                    turn.marked(),
                )
        }
        None => 0,
    };
    attack.max(block)
}

/// The next opponent's combat that comes before the AI's own next untap
/// step: the active opponent's this turn while its blocks are still ahead,
/// else that of the next player in turn order if it is an opponent and its
/// turn comes before the AI's.
fn next_opponent_combat(state: &GameState, ai: PlayerId) -> Option<(PlayerId, CombatTurn)> {
    let active = state.active_player;
    if active != ai && is_opponent(state, ai, active) && blocks_ahead(state.phase) {
        return Some((active, CombatTurn::This));
    }
    let next = next_player_in_turn_order(state, active);
    (next != ai && is_opponent(state, ai, next)).then_some((next, CombatTurn::Later))
}
