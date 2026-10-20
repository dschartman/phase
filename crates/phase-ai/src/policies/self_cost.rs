//! Net-value gate for self-cost ability activations.
//!
//! An activated ability whose *cost* spends the AI's own resources — it
//! sacrifices a permanent, pays life, discards, or exiles cards from the AI's
//! own hand/graveyard — should only be activated when its *effect* buys
//! something worth the loss. The free-outlet policy only prices creature
//! sacrifice for aristocrats outlets; land-sacrifice lifegain (Zuran Orb),
//! pay-life pingers, discard-to-grant loops, and self-exile-from-graveyard
//! grants all slip past it, so the AI cracks them every turn for nothing.
//!
//! This module is the single authority that (1) recognizes those four cost
//! shapes on the `AbilityCost` tree, (2) prices the self-inflicted cost, and
//! (3) decides whether the ability's immediate payoff is trivial, and prices
//! the payoffs it can price confidently (draws, out-of-pressure lifegain) for
//! the net-value comparison — a draw the policy can classify as **AI-only**
//! is priced only when the engine can complete an exact present-state
//! draw-delivery preview. An exact partial or zero delivery is priced exactly;
//! a replacement order, optional replacement, or continuation-owned choice is
//! `Unpriced` and therefore leaves the comparison neutral. Only AI-only
//! recipients are previewed — mixed-recipient draws are `Unpriced`, while
//! opponent-only draws are trivial churn.
//!
//! It is deliberately conservative: anything whose
//! payoff scales or is ambiguous — mana production, land search, large or
//! power-derived damage, beneficial counters — is treated as non-trivial, so
//! ramp, fixing, burn finishers, and counter payoffs are never suppressed.
//! That conservatism is typed as [`BenefitAppraisal::Unpriced`], which covers
//! both a non-trivial effect with no confident price and an unmodeled rider
//! sitting beside a priced effect — in either case no directional conclusion
//! about the trade is sound, so the comparison stands down. Off-ability
//! synergy for a resource that is actually spent (a lifegain/reanimator shell)
//! stands the gate down entirely. A sacrifice is never a generic discount:
//! death-trigger value must be represented by the activated ability's real
//! payoff before it can outweigh that cost.
//!
//! Cost-vs-benefit for a self-cost activation is answered **here and only
//! here**: `FreeOutletActivationPolicy` scores aristocrats death-trigger
//! payoff presence for free outlets and holds no cost authority.
//!
//! Only the *scoring* half lives here; the thin `SelfCostValuePolicy` adapter
//! (`self_cost_value.rs`) fetches the activated ability and turns these
//! predicates into a `PolicyVerdict`.

use engine::game::effects::counters::{preview_counter_addition, CounterAdditionPreview};
use engine::game::effects::draw::{preview_draw_delivery, DrawDeliveryPreview};
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::keywords::effective_flashback_cost;
use engine::game::players;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AdditionalCost, CostCategory,
    DiscardSelfScope, Effect, PtValue, QuantityExpr, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::GameState;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::FlashbackCost;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::card_value;
use crate::config::PolicyPenalties;
use crate::eval::{
    board_stats, creature_combat_value, evaluate_creature_intrinsic, KeywordBonuses,
};
use crate::features::landfall::ability_searches_library_for_land;
use crate::features::mana_ramp::target_filter_references_land;
use crate::features::reanimator::is_reanimation_fuel;
use crate::features::DeckFeatures;
use crate::plan::PlanState;

use super::effect_classify::{
    aggregate_player_impact_in, effect_polarity, extract_target_filter, filter_admits_player,
    lethal_to_creature, targeted_player_impact_in_with_bound_parent_target, EffectPolarity,
    PLAYER_IMPACT_PREFERENCE_BAND,
};
use super::self_protection_classify::{
    any_immediate_threat, is_self_protection_effect, self_protection_effect_payoff,
    target_filter_self_scoped,
};
use super::strategy_helpers::{
    permanent_board_value, sacrifice_cost, targetable_threat_value, SINGLE_CARD_VALUE,
};

/// A fixed face-damage payoff at or below this value is trivial — a 1- or
/// 2-point ping for no board effect is not worth spending a real resource on.
const FACE_DAMAGE_TRIVIAL_CEILING: i32 = 2;
/// Gaining this much life or less, with the AI not under life pressure, is not
/// worth a real self-cost (Zuran Orb's 2 is the flagship case).
const TRIVIAL_LIFEGAIN_CEILING: i32 = 3;
/// Multiplier applied to the per-point pay-life cost when the AI's life is a
/// pressured resource (mirrors `LifeTotalResourcePolicy`'s criticality test).
const PAY_LIFE_CRITICALITY_MULT: f64 = 4.0;
/// Deck-commitment floor above which an off-ability synergy payoff (lifegain,
/// reanimator) justifies paying the corresponding self-cost. Mirrors
/// `FreeOutletActivationPolicy::COMMITMENT_FLOOR`.
const SYNERGY_COMMITMENT_FLOOR: f32 = 0.1;

/// True when the ability's cost spends one of the four self-resources this gate
/// prices. Recurses `Composite`/`OneOf`. Only `Exile` from the AI's own hand or
/// graveyard is in scope; the `ExileMaterials` / `CollectEvidence` /
/// `ExileWithAggregate` / `Behold` cost siblings (and every other cost variant)
/// deliberately do not fire the gate — they are structurally different payment
/// shapes, so the catch-all is fail-open (a new cost variant simply gets no
/// gate rather than a spurious veto).
pub(crate) fn self_cost_in_scope(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice(_) | AbilityCost::PayLife { .. } | AbilityCost::Discard { .. } => {
            true
        }
        // Exile-as-cost: only the AI's own hand (a discard by another name) or
        // graveyard is a self-resource loss. Library/other zones and a bare
        // `None` zone are out of scope.
        AbilityCost::Exile { zone, .. } => {
            matches!(zone, Some(Zone::Graveyard) | Some(Zone::Hand))
        }
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().any(self_cost_in_scope)
        }
        _ => false,
    }
}

/// Replacement-aware fact for the narrow "remove N typed counters from this,
/// then put exactly N of that type on this" activated-ability shape.
///
/// This is intentionally not a general counter-value model. It only protects a
/// self-cost that claims to replenish its own exact counter payment, where a
/// replacement preventing the add would turn the activation into a pure loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelfCounterCostPreview {
    Applied,
    Prevented,
    ChoiceRequired,
    Transformed,
    Unsupported,
}

pub(crate) fn self_counter_cost_preview(
    state: &GameState,
    actor: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Option<SelfCounterCostPreview> {
    let (count, counter_type) = self_counter_removal_cost(ability.cost.as_ref()?)?;
    let Effect::PutCounter {
        counter_type: replenished_type,
        count: QuantityExpr::Fixed { value },
        target: TargetFilter::SelfRef,
    } = &*ability.effect
    else {
        return None;
    };
    if ability.sub_ability.is_some()
        || ability.else_ability.is_some()
        || counter_type != replenished_type
        || *value != i32::try_from(count).ok()?
    {
        return None;
    }

    let source = state.objects.get(&source_id)?;
    if source.controller != actor {
        return None;
    }
    match preview_counter_addition(
        state,
        actor,
        ObjectIncarnationRef::from_object(source),
        counter_type.clone(),
        count,
    )? {
        CounterAdditionPreview::Applied { .. } => Some(SelfCounterCostPreview::Applied),
        CounterAdditionPreview::Prevented => Some(SelfCounterCostPreview::Prevented),
        CounterAdditionPreview::ChoiceRequired { .. } => {
            Some(SelfCounterCostPreview::ChoiceRequired)
        }
        CounterAdditionPreview::Transformed { .. } => Some(SelfCounterCostPreview::Transformed),
        CounterAdditionPreview::Unsupported => Some(SelfCounterCostPreview::Unsupported),
    }
}

/// Extract the narrow self-counter payment this preview understands. Parser
/// output may wrap one cost in `Composite`; multi-cost composites are outside
/// this exact replenishment check because the preview does not value their
/// additional payment components.
fn self_counter_removal_cost(cost: &AbilityCost) -> Option<(u32, &CounterType)> {
    match cost {
        AbilityCost::RemoveCounter {
            count,
            counter_type: CounterMatch::OfType(counter_type),
            target: None,
            ..
        } => Some((*count, counter_type)),
        AbilityCost::Composite { costs } if costs.len() == 1 => {
            self_counter_removal_cost(&costs[0])
        }
        _ => None,
    }
}

/// How [`real_self_cost`] prices a "discard a card" leg.
///
/// The two answer different questions, as pay-life's price and
/// [`cost_is_material`]'s life count do. A bound on a trivial payoff asks
/// whether a card is spent at all, so it counts cards. A comparison against a
/// priced payoff asks what the card spent is worth, so it prices the card.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DiscardPrice {
    /// `self_cost_discard_per_card` per card, whatever the card.
    PerCard,
    /// CR 118.1 + CR 701.9b: the card the keep order gives up, measured in
    /// expected draws ([`discard_given_up_cost`]), read against the
    /// discarding player's plan.
    GivenUp(Option<PlanState>),
}

/// Price the self-inflicted portion of `cost` in card-equivalent units.
/// `Composite` sums its sub-costs (you pay them all); `OneOf` takes the minimum
/// (the payer chooses the cheapest). Out-of-scope sub-costs (mana, tap) price 0.
pub(crate) fn real_self_cost(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    penalties: &PolicyPenalties,
    discard: DiscardPrice,
) -> f64 {
    match cost {
        AbilityCost::Sacrifice(sacrifice) => {
            sacrifice_leaf_cost(state, ai_player, source_id, &sacrifice.target, penalties)
        }
        // `amount` is a QuantityExpr — resolve it, then weight by the per-point
        // life cost and by runtime life pressure.
        AbilityCost::PayLife { amount } => {
            let points = resolve_quantity(state, amount, ai_player, source_id).max(0) as f64;
            points
                * penalties.self_cost_pay_life_per_point
                * pay_life_criticality_mult(state, ai_player)
        }
        // Discard `count` is a QuantityExpr (unlike `Exile.count`).
        AbilityCost::Discard {
            count,
            filter,
            selection,
            self_scope,
        } => {
            let cards = resolve_quantity(state, count, ai_player, source_id).max(0) as usize;
            match discard {
                // CR 701.9b: only a chosen discard from hand gives up the card
                // the keep order picks. A random discard or "discard this card"
                // keeps the per-card price.
                DiscardPrice::GivenUp(plan)
                    if self_scope.is_from_hand() && selection.is_chosen() =>
                {
                    discard_given_up_cost(
                        state,
                        ai_player,
                        source_id,
                        cards,
                        filter.as_ref(),
                        plan,
                        penalties,
                    )
                }
                DiscardPrice::PerCard | DiscardPrice::GivenUp(_) => {
                    cards as f64 * penalties.self_cost_discard_per_card
                }
            }
        }
        // `Exile.count` is a plain `u32` here, so multiply directly —
        // `resolve_quantity` takes a `&QuantityExpr` and does not apply. Hand
        // exile is priced as a discard; graveyard exile is cheap.
        AbilityCost::Exile { count, zone, .. } => match zone {
            Some(Zone::Graveyard) => (*count as f64) * penalties.self_cost_exile_graveyard_per_card,
            Some(Zone::Hand) => (*count as f64) * penalties.self_cost_discard_per_card,
            _ => 0.0,
        },
        AbilityCost::Composite { costs } => costs
            .iter()
            .map(|c| real_self_cost(state, ai_player, source_id, c, penalties, discard))
            .sum(),
        AbilityCost::OneOf { costs } => {
            let min = costs
                .iter()
                .map(|c| real_self_cost(state, ai_player, source_id, c, penalties, discard))
                .fold(f64::INFINITY, f64::min);
            if min.is_finite() {
                min
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// CR 118.1 + CR 701.9b: the price of discarding `count` cards from the AI's
/// hand to pay a cost, in expected draws.
///
/// The cards are the ones the keep order gives up
/// ([`card_value::given_up_value`]) among the hand cards the cost admits (not
/// the source, CR 118.3; matching the cost's filter). Their value is divided by
/// [`card_value::expected_draw_value`] (plan-aware, floored at 1.0), so a card
/// as good as an average draw costs exactly one card, the flat price this
/// replaces, and a lone card worth four draws costs four. With an empty
/// library, or fewer admissible cards than the cost demands, the missing part
/// keeps the per-card price.
fn discard_given_up_cost(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    count: usize,
    filter: Option<&TargetFilter>,
    plan: Option<PlanState>,
    penalties: &PolicyPenalties,
) -> f64 {
    let per_card = penalties.self_cost_discard_per_card;
    let Some(expected_draw) = card_value::expected_draw_value(state, ai_player, plan) else {
        return count as f64 * per_card;
    };
    let pool = discard_pool(state, ai_player, source_id, filter);
    let given_up = count.min(pool.len());
    card_value::given_up_value(state, &pool, given_up, plan) / expected_draw * per_card
        + (count - given_up) as f64 * per_card
}

/// The hand cards a discard cost admits: not the source (CR 118.3), and
/// matching the cost's filter.
fn discard_pool(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let filter_ctx = FilterContext::from_source(state, source_id);
    state.players[ai_player.0 as usize]
        .hand
        .iter()
        .copied()
        .filter(|&id| {
            id != source_id
                && filter.is_none_or(|filter| matches_target_filter(state, id, filter, &filter_ctx))
        })
        .collect()
}

/// CR 601.2f + CR 702.34a: the mandatory non-mana cost a cast of `object`
/// pays that includes a discard from hand, or `None`.
///
/// Two sites carry such a cost: a `Required` additional cost (Thrill of
/// Possibility's "As an additional cost to cast this spell, discard a card")
/// and a flashback cost, which functions only from the graveyard (Twinned
/// Vision's "Flashback—{1}{U/R}{U/R}, Discard a card"). CR 601.2f composes the
/// two when both exist; the discard legs would then draw on one hand twice,
/// which a sum of per-leg prices cannot represent, so that shape stands down.
/// Declinable and chosen additional costs (CR 601.2b) never force the loss.
pub(crate) fn mandatory_cast_discard_cost(
    state: &GameState,
    object: &GameObject,
) -> Option<AbilityCost> {
    let additional = match &object.additional_cost {
        Some(AdditionalCost::Required(cost)) => Some(cost.clone()),
        Some(
            AdditionalCost::Optional { .. }
            | AdditionalCost::Kicker { .. }
            | AdditionalCost::Choice(_, _),
        )
        | None => None,
    };
    let flashback = (object.zone == Zone::Graveyard)
        .then(|| effective_flashback_cost(state, object.id))
        .flatten()
        .and_then(|cost| match cost {
            FlashbackCost::NonMana(cost) => Some(cost),
            FlashbackCost::Mana(_) => None,
        });
    let cost = match (additional, flashback) {
        (Some(cost), None) | (None, Some(cost)) => cost,
        (None, None) | (Some(_), Some(_)) => return None,
    };
    discards_from_hand(&cost).then_some(cost)
}

/// True when `cost` charges a discard from hand on every payment path: a
/// `Composite` leg, or every leg of a `OneOf`.
fn discards_from_hand(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Discard { self_scope, .. } => self_scope.is_from_hand(),
        AbilityCost::Composite { costs } => costs.iter().any(discards_from_hand),
        AbilityCost::OneOf { costs } => !costs.is_empty() && costs.iter().all(discards_from_hand),
        _ => false,
    }
}

/// True when the cost spends an amount of life that is material on its own,
/// independent of what [`real_self_cost`] prices it at.
///
/// This exists because the two functions answer different questions. Pricing
/// feeds a *comparison* against a payoff; this feeds a *bound* on a branch where
/// the payoff has already been certified trivial and there is nothing to compare
/// against. `self_cost_value.rs`'s module docs carry the full argument for why
/// the bound is stated as a life count rather than as a price.
///
/// The folds mirror [`real_self_cost`]'s exactly, because they walk the same
/// tree with the same payment semantics: `Composite` charges every leg, so ANY
/// material leg makes the whole cost material; `OneOf` lets the payer choose, so
/// the cost is material only when EVERY alternative is (an empty `OneOf` charges
/// nothing and is not material, matching the `0.0` `real_self_cost` folds it
/// to).
///
/// Every other cost shape is `false`, for two different reasons. Discard and
/// hand-exile already price at `self_cost_discard_per_card` (1.0) per card on
/// the bound path ([`DiscardPrice::PerCard`]), which is exactly the caller's
/// real-cost floor, so listing them would change no verdict. Sacrifice is
/// deliberately excluded: it is priced against the specific permanent the cost
/// would consume, and the cheap end of that range
/// (`sacrifice_token_cost`) sits *below* the floor on purpose, so that cracking
/// a Clue, Food or Treasure stays available — a count bound has no meaning
/// there, because the resource is not fungible the way life is.
pub(crate) fn cost_is_material(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    penalties: &PolicyPenalties,
) -> bool {
    match cost {
        AbilityCost::PayLife { amount } => {
            let points = resolve_quantity(state, amount, ai_player, source_id);
            // A zero-life leg costs nothing and can never be material, whatever
            // threshold a deserialized config carries.
            points > 0 && points >= penalties.self_cost_material_life
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .any(|c| cost_is_material(state, ai_player, source_id, c, penalties)),
        AbilityCost::OneOf { costs } => {
            !costs.is_empty()
                && costs
                    .iter()
                    .all(|c| cost_is_material(state, ai_player, source_id, c, penalties))
        }
        _ => false,
    }
}

/// Cost of the permanent(s) the sacrifice would consume. A `SelfRef` sacrifice
/// is priced against the ability's own source (never the cheapest permanent);
/// any other filter takes the cheapest AI-controlled match.
///
/// **KNOWN MISPRICING on the `SelfRef` branch when the source is a land.**
/// `sacrifice_cost` charges `sacrifice_land_penalty`, whose stated rationale is
/// CR 305.2's one-land-per-turn rate limit on the replacement drop. CR 305.4
/// refutes that for a fetchland: "Effects may also allow players to 'put' lands
/// onto the battlefield. This isn't the same as 'playing a land' and doesn't
/// count as a land played during the current turn." A fetchland puts its
/// replacement onto the battlefield, so it consumes no land drop and is close
/// to manabase-neutral — yet this path prices it as a full land lost, and the
/// AI under-activates it.
///
/// Not corrected here, but the correction is **cheap and the parts already
/// exist** — this is a scoping decision, not a research problem. The discount
/// cannot be "a land sacrificing itself is cheap": a land that sacrifices itself
/// for a non-land effect really does lose a source. The discriminator is whether
/// the ability *replaces* the land, and `policies::fetch_land_patience` already
/// carries both halves of that predicate:
///
/// - `cost_sacrifices_self` — matches `AbilityCost::Sacrifice(sac)` with
///   `sac.target == TargetFilter::SelfRef`, recursing through `Composite`, which
///   is exactly the shape this function short-circuits on;
/// - `effects_are_tapped_land_fetch` — `Effect::SearchLibrary` with a
///   land-referencing filter plus a `ChangeZone` to the battlefield. Relaxing
///   its `EtbTapState::Tapped` requirement generalizes Evolving Wilds to true
///   fetchlands (Flooded Strand), which is the case this docstring is about.
///   `features::mana_ramp::{chain_searches_for_land, chain_puts_land_to_safe_zone,
///   target_filter_references_land}` are the same building blocks.
///
/// Deferred because it is an **unmeasured AI behaviour change** across this
/// function's call sites, `scripts/ai-gate.sh` is 2-player-only and cannot reach
/// the Commander regime the reports come from, and
/// `crates/engine/data/card-data.json` is not generated in this tree, so a
/// printed fetchland's parsed representation was never observed end to end.
/// Owned follow-up; see UNIT2-IMPL-R3 §5.
fn sacrifice_leaf_cost(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: &TargetFilter,
    penalties: &PolicyPenalties,
) -> f64 {
    if matches!(target, TargetFilter::SelfRef) {
        return sacrifice_cost(state, source_id, penalties);
    }
    let filter_ctx = FilterContext::from_source(state, source_id);
    let min = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            (obj.controller == ai_player && matches_target_filter(state, id, target, &filter_ctx))
                .then(|| sacrifice_cost(state, id, penalties))
        })
        .fold(f64::INFINITY, f64::min);
    if min.is_finite() {
        min
    } else {
        0.0
    }
}

/// Outcome of pricing an in-scope self-cost ability's payoff chain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BenefitAppraisal {
    /// No effect in the chain carries a real payoff.
    ///
    /// `any_unmodeled` separates the two very different ways a chain lands
    /// here. With `false` the chain is **Trivial-proper**: every effect was
    /// judged by an explicit classifier arm and every one of them said
    /// "trivial", so "there is no payoff" is a positive finding. With `true`
    /// at least one effect has no classifier arm at all
    /// ([`EffectTriviality::Unmodeled`] — `Effect::Pump` among them), so "no
    /// payoff" is only an absence of evidence. A caller imposing a categorical
    /// bound on the strength of the appraisal alone must require `false`.
    Trivial { any_unmodeled: bool },
    /// Every effect is explicitly classified, every non-trivial effect carries
    /// a confident card-equivalent price, and `value` is their sum.
    Priced { value: f64 },
    /// The chain has a real payoff but no sound comparison basis: either a
    /// classifier-non-trivial effect has no confident price (mana, land search,
    /// dynamic damage, removal, counters, self-protection under threat, ...) or
    /// an unmodeled rider sits beside a priced effect. Stand down.
    Unpriced,
}

/// Four-valued classification of a single chain effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectTriviality {
    /// An explicit classifier arm judged it: no meaningful immediate advantage.
    Trivial,
    /// An explicit classifier arm judged it: a real payoff.
    NonTrivial,
    /// An explicit classifier arm judged it: a priced cost incurred while
    /// receiving another effect's payoff.
    Drawback,
    /// No classifier arm models this effect at all (the old catch-all).
    Unmodeled,
}

/// AI's prediction of which player receives one effect in the chain.
///
/// This is intentionally private policy state: engine target matching remains
/// authoritative for actual resolution, while the policy only predicts its
/// own target chooser's preference before targets are bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipientClass {
    AiOnly,
    OpponentOnly,
    Mixed,
}

/// Ordered state shared by the classifier while it walks one payoff chain.
/// Opponent draws can only make later mandatory parent-target discards trivial
/// up to the cards they supplied.
#[derive(Default)]
struct BenefitClassificationContext {
    remaining_opponent_draw_churn: u32,
}

/// Ordered state shared by pricing while it walks one classified payoff chain.
#[derive(Default)]
struct BenefitPricingContext {
    ai_draws_so_far: u32,
}

impl EffectTriviality {
    /// Map a classifier arm's existing `is_trivial` bool onto the typed form.
    /// Every explicit arm keeps its exact judgement; only the catch-all becomes
    /// [`EffectTriviality::Unmodeled`].
    fn from_is_trivial(is_trivial: bool) -> Self {
        if is_trivial {
            Self::Trivial
        } else {
            Self::NonTrivial
        }
    }
}

/// Appraise the whole payoff chain: is there a real payoff, and can it be
/// priced confidently enough to compare against the self-cost?
///
/// Complements the off-ability synergy check in [`synergy_justifies_self_cost`]
/// — this one measures the ability's *intrinsic* payoff.
pub(crate) fn appraise_benefit(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    penalties: &PolicyPenalties,
) -> BenefitAppraisal {
    let effects = collect_chain_effects(ability);
    appraise_effects(state, ai_player, source_id, ability, &effects, penalties)
}

/// [`appraise_benefit`] for a spell about to be cast from `origin`: the payoff
/// of the effects that cast performs.
///
/// CR 601.2a + CR 614.1a: a sub-ability that replaces its parent "instead"
/// (`AbilityCondition::ConditionInstead`) is decided by where the spell is cast
/// from when its condition reads only that ([`cast_origin_holds`]); Twinned
/// Vision cast by flashback from the graveyard draws two, not one. Any other
/// conditional or branching shape is `Unpriced`, so the comparison stands down.
pub(crate) fn appraise_cast_benefit(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    origin: Zone,
    penalties: &PolicyPenalties,
) -> BenefitAppraisal {
    match effects_cast_from(ability, origin) {
        Some(effects) => {
            appraise_effects(state, ai_player, source_id, ability, &effects, penalties)
        }
        None => BenefitAppraisal::Unpriced,
    }
}

/// The effects a spell performs when cast from `origin`, in chain order, or
/// `None` when the chain carries a condition, branch or mode that `origin`
/// alone does not decide.
pub(crate) fn effects_cast_from(ability: &AbilityDefinition, origin: Zone) -> Option<Vec<&Effect>> {
    let unbranched =
        |node: &AbilityDefinition| node.else_ability.is_none() && node.mode_abilities.is_empty();
    if ability.condition.is_some() || !unbranched(ability) {
        return None;
    }
    let mut effects: Vec<&Effect> = vec![&ability.effect];
    let mut next = ability.sub_ability.as_deref();
    while let Some(node) = next {
        if !unbranched(node) {
            return None;
        }
        match &node.condition {
            None => effects.push(&node.effect),
            // CR 614.1a: "instead" replaces the parent instruction.
            Some(AbilityCondition::ConditionInstead { inner }) => {
                if cast_origin_holds(inner, origin)? {
                    effects.pop();
                    effects.push(&node.effect);
                }
            }
            Some(_) => return None,
        }
        next = node.sub_ability.as_deref();
    }
    Some(effects)
}

/// CR 601.2a: whether `condition` holds for a spell cast from `origin`, read
/// in three-valued logic: `Some` when the cast's origin decides it, `None`
/// when any other fact would be needed. A conjunction with a false conjunct is
/// false and a disjunction with a true disjunct is true, whatever the other
/// terms read, which is what decides "if this spell wasn't cast from your
/// hand" (`Not(And[WasCast{Hand}, owned by you])`) from the origin alone.
fn cast_origin_holds(condition: &AbilityCondition, origin: Zone) -> Option<bool> {
    match condition {
        AbilityCondition::WasCast { zone } => Some(zone.is_none_or(|zone| zone == origin)),
        AbilityCondition::Not { condition } => cast_origin_holds(condition, origin).map(|b| !b),
        AbilityCondition::And { conditions } => {
            let terms: Vec<_> = conditions
                .iter()
                .map(|c| cast_origin_holds(c, origin))
                .collect();
            if terms.contains(&Some(false)) {
                Some(false)
            } else {
                terms.iter().all(|t| *t == Some(true)).then_some(true)
            }
        }
        AbilityCondition::Or { conditions } => {
            let terms: Vec<_> = conditions
                .iter()
                .map(|c| cast_origin_holds(c, origin))
                .collect();
            if terms.contains(&Some(true)) {
                Some(true)
            } else {
                terms.iter().all(|t| *t == Some(false)).then_some(false)
            }
        }
        _ => None,
    }
}

/// The shared walk behind [`appraise_benefit`] and [`appraise_cast_benefit`].
fn appraise_effects(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&Effect],
    penalties: &PolicyPenalties,
) -> BenefitAppraisal {
    let classified = classify_effects(state, ai_player, source_id, ability, effects);
    let mut total = 0.0;
    let mut any_nontrivial = false;
    let mut any_unmodeled = false;
    let mut pricing = BenefitPricingContext::default();
    for (effect, recipient, triviality) in classified {
        match triviality {
            EffectTriviality::Trivial => continue,
            EffectTriviality::Unmodeled => any_unmodeled = true,
            EffectTriviality::NonTrivial => {
                any_nontrivial = true;
                match effect_benefit_value(
                    state,
                    ai_player,
                    source_id,
                    effect,
                    recipient,
                    &mut pricing,
                    penalties,
                ) {
                    Some(value) => total += value,
                    None => return BenefitAppraisal::Unpriced,
                }
            }
            EffectTriviality::Drawback => {
                match effect_benefit_value(
                    state,
                    ai_player,
                    source_id,
                    effect,
                    recipient,
                    &mut pricing,
                    penalties,
                ) {
                    Some(value) => total += value,
                    None => return BenefitAppraisal::Unpriced,
                }
            }
        }
    }
    if !any_nontrivial {
        // All-trivial AND all-unmodeled chains land here — preserving the old
        // `benefit_is_trivial() == true` behaviour byte for byte (the catch-all
        // mapped to trivial), so the reject/marginal gate is not weakened. The
        // flag is carried, not acted on: it lets a caller tell the two cases
        // apart without changing which chains land here.
        return BenefitAppraisal::Trivial { any_unmodeled };
    }
    if any_unmodeled {
        // An unmodeled rider beside a priced effect makes the sum neither a
        // lower nor an upper bound — the measured rider population carries both
        // signs (Token/Surveil benefits, Discard/LoseLife drawbacks), so no
        // directional conclusion is sound. Decided here, during the walk,
        // independent of what the net would have been.
        return BenefitAppraisal::Unpriced;
    }
    BenefitAppraisal::Priced { value: total }
}

/// Confident card-equivalent price of a single NON-TRIVIAL effect.
/// `None` = real but not confidently priceable — this module's documented
/// conservatism, typed as [`BenefitAppraisal::Unpriced`] by the caller.
fn effect_benefit_value(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
    recipient: RecipientClass,
    pricing: &mut BenefitPricingContext,
    penalties: &PolicyPenalties,
) -> Option<f64> {
    match effect {
        // CR 121.1 + CR 121.2: one delivered card is one registry score unit,
        // but a multi-card instruction may deliver only a partial count. The
        // engine-owned preview runs that exact cloned instruction rather than
        // duplicating draw or replacement logic here.
        //
        // CR 614.6 + CR 614.11a: prevention and fully settled non-draw
        // substitutions are exact zeroes. A choice required to order or accept
        // a replacement—or raised by its continuation—is not a zero; the policy
        // returns `None` so `BenefitAppraisal::Unpriced` fails open.
        Effect::Draw { count, .. } if recipient == RecipientClass::AiOnly => {
            // `preview_draw_delivery` starts from its input state. A later
            // chained draw therefore cannot be priced independently without
            // replaying the earlier delivery; stand down rather than double-count.
            if pricing.ai_draws_so_far > 0 {
                return None;
            }
            let requested = resolve_quantity(state, count, ai_player, source_id).max(0) as u32;
            match preview_draw_delivery(state, ai_player, requested) {
                DrawDeliveryPreview::Exact { delivered } => {
                    pricing.ai_draws_so_far = pricing.ai_draws_so_far.saturating_add(delivered);
                    Some(f64::from(delivered) * SINGLE_CARD_VALUE)
                }
                DrawDeliveryPreview::Unknown => None,
            }
        }
        // A harmful targeted effect is worth the most it does to one opposing
        // creature it can target: a kill is that creature's board value, a
        // shrink of a creature in combat the combat value it loses.
        Effect::Pump { target, .. } if effect_polarity(effect) == EffectPolarity::Harmful => {
            harm_value(state, ai_player, source_id, effect, target, penalties)
        }
        // Damage that kills an opposing creature is priced the same way. Damage
        // that can also go to a player, or kills nothing, stays unpriced.
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { .. },
            target,
            ..
        } if !filter_admits_player(target) => {
            harm_value(state, ai_player, source_id, effect, target, penalties)
        }
        Effect::Discard {
            count,
            target: TargetFilter::ParentTarget,
            ..
        } if recipient == RecipientClass::AiOnly => {
            let requested = resolve_quantity(state, count, ai_player, source_id).max(0) as usize;
            let hand_size = state.players[ai_player.0 as usize].hand.len();
            // CR 609.3 + CR 701.9a: a mandatory discard does only as much as
            // possible, so cap the hand-to-graveyard discards at cards held
            // after earlier AI draws in this resolving chain.
            let discarded =
                requested.min(hand_size.saturating_add(pricing.ai_draws_so_far as usize));
            Some(-(discarded as f64) * penalties.self_cost_discard_per_card)
        }
        // Lifegain to the controller, life not a pressured resource: priced on
        // the same per-point axis as paying life. Under life pressure the value
        // is genuinely larger and hard to bound — stand down (`None`),
        // preserving the pre-comparison behaviour exactly.
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        } if !ai_life_critical(state, ai_player) => Some(
            resolve_quantity(state, amount, ai_player, source_id).max(0) as f64
                * penalties.self_cost_pay_life_per_point,
        ),
        // CR 707.2 + CR 202.3: a token copy of a creature card with the chosen
        // mana value. Priced X-AGNOSTICALLY at one card-equivalent: at the
        // activation decision the player has not announced X yet
        // (`ResolvedAbility::chosen_x` is `None`, so the `Variable("X")` bound
        // resolves to 0), and pricing a payoff off a placeholder zero would be
        // worse than pricing it off its shape. One card in hand becomes one
        // creature on the battlefield, so `SINGLE_CARD_VALUE` is the honest
        // floor; how big that creature should be is the scheduling question,
        // which `MomirCurvePolicy` owns.
        Effect::CreateTokenCopyFromPool { .. } => {
            (recipient == RecipientClass::AiOnly).then_some(SINGLE_CARD_VALUE)
        }
        _ => None,
    }
}

/// Classify a chain's effects in order, preserving the per-effect recipient
/// prediction and opponent-draw churn budget.
fn classify_effects<'a>(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&'a Effect],
) -> Vec<(&'a Effect, RecipientClass, EffectTriviality)> {
    let recipients = recipient_classes(state, ai_player, source_id, ability, effects);
    let mut context = BenefitClassificationContext::default();
    effects
        .iter()
        .copied()
        .zip(recipients)
        .map(|(effect, recipient)| {
            let triviality = effect_triviality(
                state,
                ai_player,
                source_id,
                ability,
                effect,
                recipient,
                &mut context,
            );
            (effect, recipient, triviality)
        })
        .collect()
}

/// Classification view used by policy tests for ordered chain behavior.
#[cfg(test)]
pub(crate) fn chain_effect_trivialities(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Vec<EffectTriviality> {
    let effects = collect_chain_effects(ability);
    classify_effects(state, ai_player, source_id, ability, &effects)
        .into_iter()
        .map(|(_, _, triviality)| triviality)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResidualVerdict {
    TrivialAtXZero,
    MeaningfulAtXZero,
    Unknown,
}

/// Classifies residual effects at X=0. The caller removes X-scaled effects
/// before calling, so an omitted X draw cannot contribute opponent-discard churn.
pub(crate) fn residual_effects_at_x_zero(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&Effect],
) -> ResidualVerdict {
    let mut verdict = ResidualVerdict::TrivialAtXZero;
    for (_, _, triviality) in classify_effects(state, ai_player, source_id, ability, effects) {
        match triviality {
            EffectTriviality::Trivial => {}
            EffectTriviality::Unmodeled => return ResidualVerdict::Unknown,
            EffectTriviality::NonTrivial | EffectTriviality::Drawback => {
                verdict = ResidualVerdict::MeaningfulAtXZero;
            }
        }
    }
    verdict
}

fn recipient_classes(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&Effect],
) -> Vec<RecipientClass> {
    let root_player_recipient = matches!(
        recipient_filter(&ability.effect),
        Some(TargetFilter::Player)
    )
    .then(|| predicted_root_player_recipient(state, ai_player, source_id, effects));

    effects
        .iter()
        .map(|effect| match recipient_filter(effect) {
            Some(TargetFilter::Controller) => RecipientClass::AiOnly,
            Some(TargetFilter::ParentTarget) => {
                root_player_recipient.unwrap_or(RecipientClass::Mixed)
            }
            Some(TargetFilter::Player) if std::ptr::eq(*effect, &*ability.effect) => {
                root_player_recipient.unwrap_or(RecipientClass::Mixed)
            }
            Some(filter) => recipient_class_for_filter(state, ai_player, source_id, filter),
            None => RecipientClass::Mixed,
        })
        .collect()
}

fn recipient_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::Draw { target, .. } | Effect::Discard { target, .. } => Some(target),
        _ => extract_target_filter(effect),
    }
}

fn predicted_root_player_recipient(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effects: &[&Effect],
) -> RecipientClass {
    let source_controller = state
        .objects
        .get(&source_id)
        .map(|object| object.controller);
    let aggregate = aggregate_player_impact_in(effects);
    let mut ai_candidate = false;
    let mut ai_accepted = false;
    let mut opponent_candidate = false;
    let mut opponent_accepted = false;

    for player in state.players.iter().filter(|player| !player.is_eliminated) {
        let impact = targeted_player_impact_in_with_bound_parent_target(
            state,
            source_controller,
            Some(source_id),
            effects,
            player.id,
            player.id,
        )
        .unwrap_or(aggregate);
        let prefers_self = if impact > PLAYER_IMPACT_PREFERENCE_BAND {
            true
        } else if impact < -PLAYER_IMPACT_PREFERENCE_BAND {
            false
        } else {
            return RecipientClass::Mixed;
        };
        let accepted = prefers_self == (player.id == ai_player);

        if player.id == ai_player {
            ai_candidate = true;
            ai_accepted = accepted;
        } else if players::is_opponent(state, ai_player, player.id) {
            opponent_candidate = true;
            opponent_accepted |= accepted;
        } else {
            return RecipientClass::Mixed;
        }
    }

    if !ai_candidate || !opponent_candidate {
        return RecipientClass::Mixed;
    }
    match (ai_accepted, opponent_accepted) {
        (true, false) => RecipientClass::AiOnly,
        (false, true) => RecipientClass::OpponentOnly,
        (true, true) | (false, false) => RecipientClass::Mixed,
    }
}

fn recipient_class_for_filter(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    filter: &TargetFilter,
) -> RecipientClass {
    let source_controller = state
        .objects
        .get(&source_id)
        .map(|object| object.controller);
    let ai_matches = engine::game::filter::player_matches_target_filter_in_state(
        state,
        filter,
        ai_player,
        source_controller,
        Some(source_id),
    );
    let mut opponent_matches = false;
    for player in state.players.iter().filter(|player| !player.is_eliminated) {
        if player.id == ai_player {
            continue;
        }
        if !engine::game::filter::player_matches_target_filter_in_state(
            state,
            filter,
            player.id,
            source_controller,
            Some(source_id),
        ) {
            continue;
        }
        if players::is_opponent(state, ai_player, player.id) {
            opponent_matches = true;
        } else {
            return RecipientClass::Mixed;
        }
    }
    match (ai_matches, opponent_matches) {
        (true, false) => RecipientClass::AiOnly,
        (false, true) => RecipientClass::OpponentOnly,
        (true, true) | (false, false) => RecipientClass::Mixed,
    }
}

/// CR 400.7: an effect that moves the ability's own subject off the battlefield
/// to hand or exile is a **save**, not removal — the object ceases to exist and
/// returns as a new object, so whatever was aimed at it no longer applies.
///
/// This arm exists because the removal arms price a zone change through
/// `removal_is_trivial` → `targetable_threat_value`, which is 0 for a permanent
/// the AI already controls. Without it every self-bounce is "trivial", and the
/// materiality veto forbids Selenia, Dark Angel ("Pay 2 life: Return Selenia to
/// its owner's hand") from saving itself with a Doom Blade on the stack.
///
/// **Class, not a card.** In `data/card-data.json` the activated-ability shape
/// `Bounce { target: SelfRef }` covers 67 cards (Selenia, Crovax Ascendant Hero,
/// Amugaba, Arcanis the Omnipotent, …) and every one carries
/// `destination: null`, i.e. hand. Self-scoped `ChangeZone` adds `Hand` (54) and
/// `Exile` (37 — Aetherling, Anurid Brushhopper, Argent Sphinx: "exile it,
/// return it at end of turn").
///
/// The subject predicate is `target_filter_self_scoped`, the same one
/// `is_self_protection_effect` already applies to `Effect::PhaseOut` — the
/// blink-to-dodge sibling of this class — so both dodge shapes read the subject
/// through one authority.
///
/// Destinations are deliberately narrow. `Graveyard` is a self-sacrifice,
/// `Battlefield` is self-reanimation from the graveyard (149 cards), and
/// `Library` gives the card up outright (7); none of those is a save, and this
/// predicate claims none of them.
fn is_self_scoped_save_effect(effect: &Effect) -> bool {
    match effect {
        Effect::Bounce { target, .. } => target_filter_self_scoped(target),
        Effect::ChangeZone {
            destination: Zone::Hand | Zone::Exile,
            target,
            ..
        } => target_filter_self_scoped(target),
        _ => false,
    }
}

/// The gate every "save the source" shape shares: protection is only worth a
/// real cost when there is something to protect against.
///
/// Single authority for the self-protection grant (Adanto Vanguard's
/// indestructible) and the self-scoped zone-change dodge (Selenia's bounce), so
/// the two cannot drift. `self_protection_effect_payoff` answers exactly for the
/// `GenericEffect` grant shape and returns `None` for the zone-change shapes,
/// which then fall through to the threat check — and if the classifier is later
/// taught to preview a dodge exactly, this arm picks that up with no change.
fn save_shape_triviality(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
) -> EffectTriviality {
    EffectTriviality::from_is_trivial(
        match self_protection_effect_payoff(state, ai_player, source_id, effect) {
            Some(has_payoff) => !has_payoff,
            None => !any_immediate_threat(state, ai_player),
        },
    )
}

/// Whether a single effect carries a meaningful immediate advantage, and
/// whether this module models it at all.
fn effect_triviality(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effect: &Effect,
    recipient: RecipientClass,
    context: &mut BenefitClassificationContext,
) -> EffectTriviality {
    match effect {
        Effect::Draw { count, .. } => match recipient {
            RecipientClass::OpponentOnly => {
                if count.is_up_to() {
                    return EffectTriviality::Trivial;
                }
                let count = resolve_quantity(state, count, ai_player, source_id).max(0) as u32;
                context.remaining_opponent_draw_churn =
                    context.remaining_opponent_draw_churn.saturating_add(count);
                EffectTriviality::Trivial
            }
            RecipientClass::AiOnly => EffectTriviality::from_is_trivial(
                resolve_quantity(state, count, ai_player, source_id) < 1,
            ),
            RecipientClass::Mixed => EffectTriviality::NonTrivial,
        },
        Effect::Discard {
            count: count_expr,
            target: TargetFilter::ParentTarget,
            ..
        } => match recipient {
            RecipientClass::AiOnly => {
                let count = resolve_quantity(state, count_expr, ai_player, source_id);
                if count_expr.is_up_to() || count <= 0 {
                    EffectTriviality::Trivial
                } else {
                    EffectTriviality::Drawback
                }
            }
            RecipientClass::OpponentOnly => {
                let count = resolve_quantity(state, count_expr, ai_player, source_id);
                if count_expr.is_up_to() || count < 0 {
                    return EffectTriviality::NonTrivial;
                }
                let count = count as u32;
                let trivial = count <= context.remaining_opponent_draw_churn;
                context.remaining_opponent_draw_churn =
                    context.remaining_opponent_draw_churn.saturating_sub(count);
                EffectTriviality::from_is_trivial(trivial)
            }
            RecipientClass::Mixed => EffectTriviality::NonTrivial,
        },
        // Damage is non-trivial if it is dynamic (power-derived, e.g. Fling),
        // lethal to a player, kills a real creature, or exceeds the face-ping
        // ceiling.
        Effect::DealDamage { amount, target, .. } => EffectTriviality::from_is_trivial(
            deal_damage_is_trivial(state, ai_player, source_id, amount, target),
        ),
        // A shrink aimed at a creature is removal or a combat trick against
        // it: trivial only when it does nothing to any opposing creature it
        // can target ([`harmful_outcomes`] empty). An undecidable (-X/-X)
        // shrink is non-trivial and stays unpriced.
        Effect::Pump { target, .. } if effect_polarity(effect) == EffectPolarity::Harmful => {
            EffectTriviality::from_is_trivial(
                harmful_outcomes(state, ai_player, source_id, effect, target)
                    .is_some_and(|outcomes| outcomes.is_empty()),
            )
        }
        // Small lifegain is trivial unless the AI is under life pressure.
        Effect::GainLife { amount, .. } => EffectTriviality::from_is_trivial(
            resolve_quantity(state, amount, ai_player, source_id) <= TRIVIAL_LIFEGAIN_CEILING
                && !ai_life_critical(state, ai_player),
        ),
        // A self-scoped zone change is a SAVE, not removal. This arm MUST stay
        // above the two removal arms below, which `Bounce` and
        // `ChangeZone { destination: Exile }` would otherwise match.
        effect if is_self_scoped_save_effect(effect) => {
            save_shape_triviality(state, ai_player, source_id, effect)
        }
        // Removal is non-trivial when a worthwhile opponent creature can be hit.
        Effect::Destroy { target, .. } | Effect::Bounce { target, .. } => {
            EffectTriviality::from_is_trivial(removal_is_trivial(
                state, ai_player, source_id, target,
            ))
        }
        Effect::ChangeZone {
            destination: Zone::Exile | Zone::Graveyard,
            target,
            ..
        } => EffectTriviality::from_is_trivial(removal_is_trivial(
            state, ai_player, source_id, target,
        )),
        // A beneficial counter (e.g. an indestructible keyword counter) is
        // non-trivial by default; a harmful counter is removal. The one trivial
        // case is a self-counter that fizzles because paying the cost removes
        // its only recipient (Carrion Feeder into an empty board).
        Effect::PutCounter {
            counter_type,
            target,
            ..
        } => EffectTriviality::from_is_trivial(put_counter_is_trivial(
            state,
            ai_player,
            source_id,
            ability,
            counter_type,
            target,
        )),
        // A mass counter mirrors the single-`PutCounter` classification: a
        // harmful mass counter (e.g. "-1/-1 counter on each creature") is real
        // board interaction and non-trivial whenever it wipes/shrinks a
        // worthwhile opponent creature — only trivial when it has no worthwhile
        // opponent-board impact. A beneficial mass counter is non-trivial by
        // default (conservative), consistent with single `PutCounter`.
        Effect::PutCounterAll {
            counter_type,
            target,
            ..
        } => EffectTriviality::from_is_trivial(if counter_is_harmful(counter_type) {
            removal_is_trivial(state, ai_player, source_id, target)
        } else {
            false
        }),
        // Mana production is ramp — never trivial (Ashnod's/Phyrexian Altar).
        Effect::Mana { .. } => EffectTriviality::NonTrivial,
        // A library search for a land is ramp/fixing (sacrifice-a-land fetch
        // chains) — non-trivial.
        Effect::SearchLibrary { filter, .. } => EffectTriviality::from_is_trivial(
            !(ability_searches_library_for_land(ability) || target_filter_references_land(filter)),
        ),
        // A self-protection grant is only worth a cost when a threat is live.
        effect if is_self_protection_effect(effect) => {
            save_shape_triviality(state, ai_player, source_id, effect)
        }
        // No modeled board impact at all — the honest third answer.
        // CR 707.2 + CR 202.3: a token that's a copy of a creature card chosen
        // at random from the whole corpus (the Momir's Madness emblem). It is a
        // real permanent, not an absence of evidence — leaving it `Unmodeled`
        // priced the payoff at zero and made `SelfCostValuePolicy` hard-veto
        // every activation whose discard met `REAL_COST_FLOOR`, which is every
        // activation. Only the controller's own token is a benefit here.
        Effect::CreateTokenCopyFromPool { .. } => match recipient {
            RecipientClass::AiOnly => EffectTriviality::NonTrivial,
            RecipientClass::OpponentOnly => EffectTriviality::Trivial,
            RecipientClass::Mixed => EffectTriviality::NonTrivial,
        },
        _ => EffectTriviality::Unmodeled,
    }
}

/// A fixed face ping at or below the ceiling, that neither kills a
/// player nor a real creature, is trivial. Dynamic (non-`Fixed`) damage is
/// power-derived (Fling and friends) and always treated as non-trivial so burn
/// finishers are never suppressed.
fn deal_damage_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    amount: &QuantityExpr,
    target: &TargetFilter,
) -> bool {
    let QuantityExpr::Fixed { value } = amount else {
        return false;
    };
    let value = *value;
    if value > FACE_DAMAGE_TRIVIAL_CEILING {
        return false;
    }
    if filter_admits_player(target) && damage_lethal_to_opponent(state, ai_player, value) {
        return false;
    }
    if damage_kills_creature(state, ai_player, source_id, target, value) {
        return false;
    }
    true
}

/// What a harmful targeted effect does to one opposing creature.
#[derive(Debug, Clone, Copy, PartialEq)]
enum HarmOutcome {
    /// CR 704.5f + CR 704.5g: the creature dies.
    Kill(ObjectId),
    /// CR 510.1a: a creature in combat deals and survives less; the combat
    /// value it loses, on `creature_combat_value`'s scale.
    CombatLoss(f64),
}

/// The outcomes `effect` (a shrink or fixed damage) has on each opposing
/// creature `target` admits, from the source's view: only the ones that do
/// something. Empty means it does nothing to any of them; `None` means the
/// effect's size is undecidable here (-X/-X, variable damage).
///
/// Lethality reads the creature's current toughness and the damage already
/// marked on it (`effect_classify::lethal_to_creature`). A shrink that does not
/// kill matters only to a creature that is attacking or blocking: until end of
/// turn and out of combat, it changes nothing.
fn harmful_outcomes(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
    target: &TargetFilter,
) -> Option<Vec<HarmOutcome>> {
    let opponents = players::opponents(state, ai_player);
    let filter_ctx = FilterContext::from_source(state, source_id);
    let mut outcomes = Vec::new();
    for &id in &state.battlefield {
        let Some(obj) = state.objects.get(&id) else {
            continue;
        };
        if !opponents.contains(&obj.controller)
            || !obj.card_types.core_types.contains(&CoreType::Creature)
            || !matches_target_filter(state, id, target, &filter_ctx)
        {
            continue;
        }
        if lethal_to_creature(state, id, &[effect])? {
            outcomes.push(HarmOutcome::Kill(id));
            continue;
        }
        let Effect::Pump {
            power: PtValue::Fixed(dp),
            toughness: PtValue::Fixed(dt),
            ..
        } = effect
        else {
            continue;
        };
        if !in_combat(state, id) {
            continue;
        }
        let (power, toughness) = (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0));
        let after = creature_combat_value(
            (power + dp).max(0),
            toughness + dt,
            |kw| obj.has_keyword(kw),
            &KeywordBonuses::default(),
        );
        let loss = evaluate_creature_intrinsic(state, id) - after;
        if loss > 0.0 {
            outcomes.push(HarmOutcome::CombatLoss(loss));
        }
    }
    Some(outcomes)
}

/// The price of [`harmful_outcomes`]: the best single outcome, a kill at the
/// creature's board value (`strategy_helpers::permanent_board_value`, the
/// price this policy charges for sacrificing one). `None` when undecidable or
/// when the effect does nothing.
fn harm_value(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
    target: &TargetFilter,
    penalties: &PolicyPenalties,
) -> Option<f64> {
    harmful_outcomes(state, ai_player, source_id, effect, target)?
        .into_iter()
        .map(|outcome| match outcome {
            HarmOutcome::Kill(id) => permanent_board_value(state, id, penalties),
            HarmOutcome::CombatLoss(loss) => loss,
        })
        .reduce(f64::max)
}

/// CR 506.4 + CR 508.1: an attacking or blocking creature is in combat until
/// combat ends or it is removed from combat.
fn in_combat(state: &GameState, id: ObjectId) -> bool {
    state.combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == id)
            || combat.blocker_to_attacker.contains_key(&id)
    })
}

fn damage_lethal_to_opponent(state: &GameState, ai_player: PlayerId, value: i32) -> bool {
    players::opponents(state, ai_player).iter().any(|&opp| {
        let player = &state.players[opp.0 as usize];
        !player.is_eliminated && player.life <= value
    })
}

/// True when `value` fixed damage would be lethal to at least one opponent
/// creature the filter admits (via `lethal_to_creature`).
fn damage_kills_creature(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: &TargetFilter,
    value: i32,
) -> bool {
    let opponents = players::opponents(state, ai_player);
    let filter_ctx = FilterContext::from_source(state, source_id);
    let damage = Effect::DealDamage {
        amount: QuantityExpr::Fixed { value },
        target: TargetFilter::Any,
        damage_source: None,
        excess: None,
    };
    state.battlefield.iter().any(|&id| {
        let Some(obj) = state.objects.get(&id) else {
            return false;
        };
        opponents.contains(&obj.controller)
            && obj.card_types.core_types.contains(&CoreType::Creature)
            && matches_target_filter(state, id, target, &filter_ctx)
            && lethal_to_creature(state, id, &[&damage]) == Some(true)
    })
}

fn removal_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: &TargetFilter,
) -> bool {
    targetable_threat_value(state, ai_player, target, source_id) <= 0.0
}

/// A placed counter is beneficial-by-default (indestructible, +1/+1)
/// unless its sign is negative. A harmful counter routes through removal
/// semantics; a beneficial counter is trivial only when it fizzles.
fn put_counter_is_trivial(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    counter_type: &CounterType,
    target: &TargetFilter,
) -> bool {
    if counter_is_harmful(counter_type) {
        return removal_is_trivial(state, ai_player, source_id, target);
    }
    put_counter_fizzles(state, ai_player, source_id, ability, target)
}

/// A self-targeted beneficial counter fizzles when paying the cost necessarily
/// removes the source — sacrificing the only recipient (Carrion Feeder with no
/// other creature to feed it).
fn put_counter_fizzles(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    counter_target: &TargetFilter,
) -> bool {
    if !matches!(counter_target, TargetFilter::SelfRef) {
        return false;
    }
    ability
        .cost
        .as_ref()
        .is_some_and(|cost| sacrifice_must_remove_source(state, ai_player, source_id, cost))
}

/// True when paying `cost` necessarily sacrifices the ability's source — either
/// a `SelfRef` sacrifice, or a filtered sacrifice whose only legal AI-controlled
/// target is the source itself.
fn sacrifice_must_remove_source(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    match cost {
        AbilityCost::Sacrifice(sacrifice) => {
            if matches!(sacrifice.target, TargetFilter::SelfRef) {
                return true;
            }
            let filter_ctx = FilterContext::from_source(state, source_id);
            let mut matched_any = false;
            let mut matched_other = false;
            for &id in &state.battlefield {
                let Some(obj) = state.objects.get(&id) else {
                    continue;
                };
                if obj.controller == ai_player
                    && matches_target_filter(state, id, &sacrifice.target, &filter_ctx)
                {
                    matched_any = true;
                    if id != source_id {
                        matched_other = true;
                    }
                }
            }
            matched_any && !matched_other
        }
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => costs
            .iter()
            .any(|c| sacrifice_must_remove_source(state, ai_player, source_id, c)),
        _ => false,
    }
}

/// Harmful counters are the negative-sign counters (`-1/-1`, negative
/// power/toughness, or a generic counter whose name reads negative). Everything
/// else — `+1/+1`, keyword counters (indestructible), and other typed counters —
/// is treated as beneficial-by-default for the gate.
fn counter_is_harmful(counter_type: &CounterType) -> bool {
    match counter_type {
        CounterType::Minus1Minus1 => true,
        CounterType::PowerToughness { power, toughness } => *power < 0 || *toughness < 0,
        CounterType::Generic(name) => name.starts_with('-'),
        _ => false,
    }
}

/// The AI's life is a pressured resource when it is at or
/// below 5, or at or below the opponents' combined board power. Mirrors
/// `LifeTotalResourcePolicy`'s `ai_critical` test.
fn ai_life_critical(state: &GameState, ai_player: PlayerId) -> bool {
    let ai_life = state.players[ai_player.0 as usize].life;
    let opp_total_power: i32 = players::opponents(state, ai_player)
        .iter()
        .map(|&opp| board_stats(state, opp).power)
        .sum();
    ai_life <= 5 || ai_life <= opp_total_power
}

fn pay_life_criticality_mult(state: &GameState, ai_player: PlayerId) -> f64 {
    if ai_life_critical(state, ai_player) {
        PAY_LIFE_CRITICALITY_MULT
    } else {
        1.0
    }
}

/// Whether off-ability deck synergy justifies paying this self-cost even though
/// the ability's own effect is trivial. Complements the intrinsic-payoff check
/// in [`appraise_benefit`] — it covers value that lands elsewhere in a
/// lifegain/reanimator engine fed by the resource spent.
pub(crate) fn synergy_justifies_self_cost(
    payer: &CostPayer<'_>,
    features: &DeckFeatures,
    ability: &AbilityDefinition,
) -> bool {
    ability
        .cost
        .as_ref()
        .is_some_and(|cost| synergy_justifies_cost_tree(payer, features, cost))
}

/// [`synergy_justifies_self_cost`] for a cost that is not an ability's own,
/// such as the mandatory cost of a cast.
pub(crate) fn synergy_justifies_cost_tree(
    payer: &CostPayer<'_>,
    features: &DeckFeatures,
    cost: &AbilityCost,
) -> bool {
    !contains_sacrifice_cost(cost) && synergy_justifies_cost(payer, features, cost)
}

/// Who pays a self-cost, from what source, under which plan: what the
/// reanimator waiver needs to see the card a discard gives up.
pub(crate) struct CostPayer<'a> {
    pub(crate) state: &'a GameState,
    pub(crate) player: PlayerId,
    pub(crate) source: ObjectId,
    pub(crate) plan: Option<PlanState>,
}

/// A sacrifice leaf remains a real cost even when nested in a composite or a
/// choice of costs.  Treating a sibling lifegain/reanimator cost as a reason to
/// waive the whole tree would reintroduce the aristocrats exception indirectly.
fn contains_sacrifice_cost(cost: &AbilityCost) -> bool {
    cost.categories()
        .contains(&CostCategory::SacrificesPermanent)
}

fn synergy_justifies_cost(
    payer: &CostPayer<'_>,
    features: &DeckFeatures,
    cost: &AbilityCost,
) -> bool {
    match cost {
        // Sacrificing the source is a concrete cost, not a generic aristocrats
        // discount.  Death triggers remain part of an ability's actual payoff
        // appraisal; they must not make an otherwise trivial activation free.
        AbilityCost::Sacrifice(_) => false,
        AbilityCost::PayLife { .. } => features.lifegain.commitment >= SYNERGY_COMMITMENT_FLOOR,
        // CR 701.9b + CR 404.1: a reanimator deck's discard is a discount only
        // for fuel, a card its own reanimation effects can return
        // (`reanimator::is_reanimation_fuel`). Any other card the keep order
        // gives up is a real loss and pays its price.
        AbilityCost::Discard {
            count,
            filter,
            self_scope,
            ..
        } => {
            features.reanimator.commitment >= SYNERGY_COMMITMENT_FLOOR
                && discard_gives_up_only_fuel(payer, count, filter.as_ref(), *self_scope)
        }
        // Exile from the AI's own hand/graveyard: no synergy stand-down. Graveyard
        // exile is a strict loss for a reanimator deck (it removes fuel), and no
        // real card pairs a trivial payoff with a hand-exile self-cost.
        AbilityCost::Exile { .. } => false,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => costs
            .iter()
            .any(|cost| synergy_justifies_cost(payer, features, cost)),
        _ => false,
    }
}

/// True when every card a discard cost gives up is reanimation fuel: the
/// source itself for "discard this card", otherwise the keep order's first
/// `count` admissible hand cards ([`card_value::given_up_cards`]).
fn discard_gives_up_only_fuel(
    payer: &CostPayer<'_>,
    count: &QuantityExpr,
    filter: Option<&TargetFilter>,
    self_scope: DiscardSelfScope,
) -> bool {
    let is_fuel = |card| is_reanimation_fuel(payer.state, payer.player, card);
    if self_scope.is_source_card() {
        return is_fuel(payer.source);
    }
    let cards = resolve_quantity(payer.state, count, payer.player, payer.source).max(0) as usize;
    let pool = discard_pool(payer.state, payer.player, payer.source, filter);
    let given_up = card_value::given_up_cards(payer.state, &pool, cards, payer.plan);
    // No card given up stands on nothing: an empty selection is never fuel.
    !given_up.is_empty() && given_up.into_iter().all(is_fuel)
}

/// Count AI-controlled death-trigger payoff objects currently on the battlefield.
/// Uses `death_trigger_names` as an identity-lookup list — the structural
/// classification already happened at deck-build time in `aristocrats::detect`.
/// Shared with `FreeOutletActivationPolicy` (the aristocrats sac-outlet path).
pub(crate) fn count_death_triggers_on_board(
    state: &GameState,
    player: PlayerId,
    death_trigger_names: &[String],
) -> usize {
    if death_trigger_names.is_empty() {
        return 0;
    }
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj: &&GameObject| obj.controller == player && obj.zone == Zone::Battlefield)
        .filter(|obj| death_trigger_names.iter().any(|name| name == &obj.name))
        .count()
}
