//! Card valuation and give-up ordering.
//!
//! Two authorities live here, deliberately kept apart:
//!
//! - [`intrinsic_value`] — the zone-agnostic scalar the search uses to rank
//!   cards (scry/dig/surveil/reveal, cleanup discard, the `CastSpell` additive
//!   baseline).
//! - [`keep_tier`] / [`cmp_keep`] — the *order* in which cards are surrendered
//!   at a minimizing selection. Tier dominates the scalar, so a mana source the
//!   plan still needs is never pitched ahead of a spell that merely scores
//!   lower.
//!
//! `cost_card_value` is the cost-side sibling, relocated here so its
//! divergence from [`intrinsic_value`] is visible in one place instead of
//! hidden in a policy file.
//!
//! [`worth_drawing`] is the keep-on-top test for scry and surveil: the
//! scalar at or above [`KEEP_VALUE_FLOOR`] (the floor the `DigChoice` up-to
//! arm reads) and not a [`KeepTier::Surplus`] mana source.
//!
//! **Which discard seams are tiered.** Among discard seams, [`keep_tier`]
//! reaches exactly one `deterministic_choice` arm: the CR 514.1 cleanup discard
//! (`WaitingFor::DiscardToHandSize`); it also reaches the scry and surveil arms
//! through [`worth_drawing`]. It reaches one scored discard seam, through
//! [`keep_prices`]: the CR 118.1 + CR 701.9b discard cost
//! (`WaitingFor::PayCost { kind: Discard }`, priced by
//! `policies::payment_selection`), so a loot or a flashback that discards
//! gives up cards in the cleanup discard's order. The evidence for widening it
//! there: in the 2026-10-17 FRA signals run, 137 of 176 PayCost discards that
//! kept a land and pitched a castable spell were decided by the flat land
//! price alone (`deck-builder/sims/fra-phase/2026-10-17-signals-r2/
//! investigation-discard-signal.md`). The decision to pay that cost at all
//! (the loot's activation, a flashback cast whose cost discards) reads the
//! same order through [`given_up_value`], priced against
//! [`expected_draw_value`] by `policies::self_cost`, so the card the
//! activation is charged for is the card the `PayCost` prompt then gives up. It has one further consumer, which is a
//! policy rather than a `deterministic_choice` arm: the CR 601.2f sacrifice-cost
//! cast gate (`policies::sacrifice_cost_mana_gate`), which reads the tier of
//! *battlefield* objects. That does not collide with the sentence below, because
//! the two answer different questions about the same battlefield permanent:
//! `strategy_helpers::sacrifice_cost` / `SacrificeTier` decide **which fodder is
//! surrendered first** once a sacrifice is happening, while [`keep_tier`] decides
//! **whether to commit to the cast at all**. The battlefield sacrifice *ordering*
//! seam is scored
//! by `strategy_helpers::sacrifice_cost` instead — a separate authority, shared
//! with `SacrificeValuePolicy` — and ordered by `strategy_helpers::cmp_sacrifice`,
//! which is this module's `cmp_keep` idiom applied to that seam. The effect-driven
//! discard families —
//! `WaitingFor::DiscardChoice` (rummage, Thoughtseize-likes),
//! `ConniveDiscard`, `WardDiscardChoice` — have no
//! deterministic arm at all; they fall through to the policy/eval scorer.
//! One of them reads the keep order after the search, as a tie-break only:
//! at a `DiscardChoice` the AI makes from its own hand (a loot's or a
//! rummage's discard), and at the discard cost, [`keep_order_tie_break`]
//! settles candidates the search scores within [`DISCARD_TIE_BAND`] of each
//! other by [`cmp_keep`]. The evidence: in the 2026-10-20 FRA run at
//! e6602dad2, 34 of the 36 discards after a Divining Duelist loot were exact
//! top-two ties settled by action order, and every tie with a land the plan
//! did not need in hand pitched a spell (`fork-checks/e6602dad2/JUDGE.md`,
//! focus 5). A search that separates the candidates still decides.
//! `ConniveDiscard` and `WardDiscardChoice` are not reached.

use std::cmp::{Ordering, Reverse};

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::{BackFaceData, GameObject};
use engine::game::mana_abilities::is_mana_ability;
use engine::game::players;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef,
    CostCategory, DamageKindFilter, Effect, PlayerFilter, PtValue, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TriggerConstraint, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{EscapeCost, FlashbackCost, Keyword};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::features::reanimator::{has_live_reanimation, once_and_spent, reanimation_target};
use crate::mana_colors::{cost_within_land_drops, land_produced_color_types, UnmetColors};
use crate::plan::PlanState;
use crate::policies::effect_classify::{
    aura_polarity, effect_polarity, filter_admits_creature, lethal_to_creature, EffectPolarity,
};
use crate::policies::self_cost::effects_cast_from;
use crate::tactical_gate::filter_names_only_own_permanents;
use crate::zone_eval::is_intrinsic_mana_source;

/// CR 305.1 + CR 305.2: how a card in a non-battlefield zone contributes to mana
/// development.
///
/// A land is free to deploy — playing one is a special action that uses no mana
/// (CR 305.1) — but is rate-limited to one per turn (CR 305.2: "a player can
/// normally play one land during their turn"); an accelerant costs mana to
/// deploy but is not rate-limited.
///
/// **The two roles are measured on two different deficits, and must be.**
/// [`keep_tier`] reads `PlanState::lands_behind` for `LandDrop` and
/// `PlanState::mana_behind` for `Accelerant`. Matching both against
/// `lands_behind` — which is what this did before — is wrong in a way that
/// cannot be tuned away: `plan::controlled_lands` counts `CoreType::Land` only,
/// so a rock already on the battlefield never reduces that deficit. A turn-10
/// Commander player on two lands plus Sol Ring, Arcane Signet and a Talisman is
/// not short of mana at all, yet read as four lands behind *permanently*, and
/// every spare rock in hand was promoted by a deficit that playing it could not
/// close. `mana_behind` counts the whole standing manabase
/// (`plan::controlled_mana_sources`), so deploying an accelerant does reduce it.
///
/// CR 305.1 + CR 305.2 are why the axes cannot be merged rather than merely why
/// they differ: only a *land* can be played as a land, and only that play is
/// rate-limited to one per turn, so "lands short of the land schedule" and
/// "mana sources short of the mana schedule" are answers to different questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManaRole {
    LandDrop,
    Accelerant,
    None,
}

/// Give-up order for the cleanup discard selection.
/// `Ord` is the contract: **lower tiers are surrendered first**, and the
/// declaration order below IS the specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeepTier {
    /// A land held past the plan's land target (CR 305.2: one land a turn).
    /// Lands only: a nonland mana source past the mana target is `Ordinary`.
    Surplus,
    /// Everything else. Also the tier for every card when no plan authority
    /// exists, and for every mana source while exactly on curve. When all cards
    /// land here, the sort key `(Ordinary, intrinsic)` orders identically to
    /// `intrinsic` alone.
    Ordinary,
    /// A mana source held while behind the plan's schedule on its own axis.
    /// For a land in hand, only the next land drop (CR 305.2: one land a turn).
    NeededManaSource,
}

/// Structural classification of a card as a mana source.
///
/// CR 605.1a via [`is_mana_ability`]: a targeted or loyalty mana ability is not
/// a mana ability, so it does not make its source an accelerant. Detection is
/// structural — no card names, no Oracle text.
///
/// A land that is also a creature (Dryad Arbor) classifies `LandDrop`; CR 305.2's
/// once-per-turn rate limit still governs it.
///
/// Ramp *spells* (Rampant Growth, Cultivate) classify [`ManaRole::None`]: their
/// mana production is a spell effect, not an ability, so `is_mana_ability` does
/// not see them. Extending to that class needs an effect-shape predicate that
/// does not exist yet.
///
/// **Zone-agnostic as implemented**, and deliberately so: the test is structural
/// (`core_types.contains(&CoreType::Land)`, else `abilities.iter().any(is_mana_ability)`)
/// and reads the same on a card in hand as on a permanent on the battlefield. The
/// *consequence* of the classification differs by zone, though, and CR 305.2 is
/// why: a player can normally play only one land per turn, so a **battlefield**
/// land that is sacrificed costs a whole future turn's land drop to replace,
/// whereas a land in hand is merely undeployed. The sacrifice-cost cast gate
/// relies on that reading.
///
/// Accelerants share the `plan::controlled_mana_sources` population that produces
/// the `mana_behind` deficit [`keep_tier`] reads. One-shot self-sacrificing or
/// self-returning sources (Treasure, Gold, Lotus Petal, Grinning Ignus) are not
/// standing mana development, so they cannot be promoted by a deficit they never
/// reduce.
pub(crate) fn mana_role(state: &GameState, obj_id: ObjectId) -> ManaRole {
    let Some(obj) = state.objects.get(&obj_id) else {
        return ManaRole::None;
    };
    if obj.card_types.core_types.contains(&CoreType::Land) {
        return ManaRole::LandDrop;
    }
    if is_intrinsic_mana_source(obj) {
        return ManaRole::Accelerant;
    }
    ManaRole::None
}

/// Which mana sources the plan still needs, and therefore which card the AI
/// surrenders first. Consumed by the CR 514.1 + CR 701.9a cleanup discard
/// (`WaitingFor::DiscardToHandSize`) and by the CR 601.2f sacrifice-cost cast
/// gate (`policies::sacrifice_cost_mana_gate`).
/// A mana source is promoted only while the plan says the player is behind on
/// **its own** development axis — the land schedule for a land, the mana
/// schedule for an accelerant (see [`ManaRole`]) — and a land is demoted to
/// `Surplus` only while the land schedule says the player is past it; an
/// accelerant past the mana target keeps the tier its other value earns,
/// `Ordinary` (a mana creature's body, a rock's other abilities). On curve — and whenever no plan authority exists
/// — every card is `Ordinary`, which reproduces the pure-intrinsic ordering
/// exactly.
///
/// CR 305.2: a player normally plays one land a turn, so a deficit of several
/// lands is closed one land drop at a time. Below the target, only the land in
/// hand that makes the next land drop ([`is_next_land_drop`]) is
/// `NeededManaSource`; every further land in that hand is `Ordinary` and
/// competes with spells on value. A land in any other zone is not competing for
/// the drop and reads the deficit alone.
pub(crate) fn keep_tier(state: &GameState, obj_id: ObjectId, plan: Option<PlanState>) -> KeepTier {
    let role = mana_role(state, obj_id);
    let deficit = match role {
        ManaRole::None => return KeepTier::Ordinary,
        ManaRole::LandDrop => plan.map(|p| p.lands_behind),
        ManaRole::Accelerant => plan.map(|p| p.mana_behind),
    };
    match deficit.map(|d| d.cmp(&0)) {
        Some(Ordering::Greater)
            if role == ManaRole::LandDrop && !is_next_land_drop(state, obj_id) =>
        {
            KeepTier::Ordinary
        }
        Some(Ordering::Greater) => KeepTier::NeededManaSource,
        // CR 305.2: only a land is played as a land, one a turn, so only a
        // land past the land target adds nothing the plan wants. An
        // accelerant past the mana target is a card with a body or an
        // effect: its mana role adds nothing there, and it keeps the tier
        // its other value earns.
        Some(Ordering::Less) if role == ManaRole::LandDrop => KeepTier::Surplus,
        Some(Ordering::Less) | Some(Ordering::Equal) | None => KeepTier::Ordinary,
    }
}

/// CR 305.1 + CR 305.2: whether this land is the one its owner's hand would
/// spend the next land drop on. Only a land in a hand competes for the drop; a
/// land in any other zone reads `true`.
///
/// Among the hand's lands the next drop is the one covering the most colours
/// the near-term hand needs and the battlefield cannot yet produce
/// ([`UnmetColors`], the measure land-play sequencing credits a `PlayLand`
/// with), so a Forest that makes a green spell castable is kept over a sixth
/// Island. Ties go to the lowest object id, so exactly one land is the next
/// drop.
fn is_next_land_drop(state: &GameState, obj_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&obj_id) else {
        return false;
    };
    if obj.zone != Zone::Hand {
        return true;
    }
    let Some(hand) = state.players.get(obj.owner.0 as usize).map(|p| &p.hand) else {
        return true;
    };
    let lands: Vec<ObjectId> = hand
        .iter()
        .copied()
        .filter(|&id| mana_role(state, id) == ManaRole::LandDrop)
        .collect();
    if lands.len() < 2 {
        return true;
    }
    let unmet = UnmetColors::for_player(state, obj.owner);
    let colours_fixed = |id: ObjectId| {
        state.objects.get(&id).map_or(0, |land| {
            unmet.covered(&land_produced_color_types(
                &land.card_types.subtypes,
                &land.abilities,
            ))
        })
    };
    lands
        .into_iter()
        .max_by_key(|&id| (colours_fixed(id), Reverse(id)))
        == Some(obj_id)
}

/// The sort key for a minimizing selection: give-up tier first, intrinsic
/// scalar as the within-tier tie-break.
pub(crate) fn keep_key(
    state: &GameState,
    obj_id: ObjectId,
    plan: Option<PlanState>,
) -> (KeepTier, f64) {
    (
        keep_tier(state, obj_id, plan),
        intrinsic_value(state, obj_id),
    )
}

/// The single authority for minimizing-selection order. Tier dominates;
/// the intrinsic scalar breaks ties within a tier.
pub(crate) fn cmp_keep(a: &(KeepTier, f64), b: &(KeepTier, f64)) -> Ordering {
    a.0.cmp(&b.0)
        .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
}

/// How close two discard candidates' search scores must be for the keep order
/// to choose between them ([`keep_order_tie_break`]): equal to three decimals,
/// the reading of the FRA signals script's `f_score_tie` diagnostic
/// (`deck-builder/tools/phase_signals.py`). At the e6602dad2 run's loot
/// discards the search either tied exactly (45 of 47 prompts) or separated
/// the candidates by more than 0.2, so the band only has to absorb the float
/// noise of a mean over determinized samples. A gap wider than this is the
/// search reading the position, and the search's pick stands.
pub(crate) const DISCARD_TIE_BAND: f64 = 0.001;

/// CR 701.9a + CR 701.9b + CR 118.1: settle a discard the search cannot
/// decide by the keep order. `scored` is the root's scored candidates and
/// `chosen` the index the selector picked; the result is the index to play.
///
/// Among the `SelectCards` candidates that discard as many cards as the
/// chosen one and score within [`DISCARD_TIE_BAND`] of it, the one whose
/// cards come first in [`cmp_keep`] order is played: each candidate's cards
/// are keyed by [`keep_key`] and sorted, and the sorted keys are compared
/// element by element, the order the CR 514.1 cleanup arm ranks its issued
/// selections by. So a land the plan does not need goes before a spell, the
/// card `expected_loot_gain` priced the loot on. The chosen candidate is kept
/// unless another is strictly earlier in keep order, so cards with equal keys
/// (two basic lands) are left to the selector. A candidate outside the band,
/// or of a different size (the empty answer of an "up to" discard), is never
/// substituted: the search's reading stands there.
///
/// The caller decides that the prompt is a discard the AI makes from its own
/// hand and supplies the discarding player's plan; this reads only the
/// candidates' cards.
pub(crate) fn keep_order_tie_break(
    state: &GameState,
    plan: Option<PlanState>,
    scored: &[(GameAction, f64)],
    chosen: usize,
) -> usize {
    let keys_of = |action: &GameAction| -> Option<Vec<(KeepTier, f64)>> {
        let GameAction::SelectCards { cards } = action else {
            return None;
        };
        let mut keys: Vec<_> = cards.iter().map(|&id| keep_key(state, id, plan)).collect();
        keys.sort_by(cmp_keep);
        Some(keys)
    };
    let Some((chosen_action, chosen_score)) = scored.get(chosen) else {
        return chosen;
    };
    let Some(chosen_keys) = keys_of(chosen_action) else {
        return chosen;
    };
    let mut best = (chosen, chosen_keys);
    for (index, (action, score)) in scored.iter().enumerate() {
        let within_band = matches!(
            (score - chosen_score).abs().partial_cmp(&DISCARD_TIE_BAND),
            Some(Ordering::Less | Ordering::Equal)
        );
        if !within_band {
            continue;
        }
        let Some(keys) = keys_of(action) else {
            continue;
        };
        if keys.len() == best.1.len() && cmp_keep_vectors(&keys, &best.1) == Ordering::Less {
            best = (index, keys);
        }
    }
    best.0
}

/// [`cmp_keep`] over two equal-length selections whose keys are each sorted
/// by [`cmp_keep`]: the first differing card decides.
fn cmp_keep_vectors(a: &[(KeepTier, f64)], b: &[(KeepTier, f64)]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| cmp_keep(x, y))
        .find(|order| order.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// The [`cmp_keep`] order as prices, for a give-up decision that is *scored*
/// rather than sorted: the CR 118.1 + CR 701.9b discard cost at
/// `WaitingFor::PayCost` (`policies::payment_selection`). There each
/// candidate selection is priced by a policy and the price competes with the
/// search continuation, so a comparator cannot be used directly; this is the
/// same order the CR 514.1 cleanup discard sorts by, expressed as one scalar
/// per card.
///
/// Construction: sort `pool` by [`keep_key`] / [`cmp_keep`] (first surrendered
/// first), and give the i-th card the i-th smallest [`intrinsic_value`] in the
/// pool. Cards with equal keys share a price, and a card whose key is strictly
/// greater than its predecessor's prices at least [`KEEP_PRICE_STEP`] above it,
/// so the order is strict wherever `cmp_keep` is. Three properties follow, and
/// each is pinned by a test:
///
/// - **Order.** For `a`, `b` in `pool`, `cmp_keep(key(a), key(b))` is the
///   order of their prices. A surplus land prices below every spell, a land
///   the plan still needs above every spell.
/// - **Fail-safe.** When every card is `Ordinary` (no plan authority, or on
///   curve with no mana source in the pool) each card's price is its own
///   `intrinsic_value`, exactly.
/// - **Scale.** The prices are the pool's own values rearranged, so the
///   cheapest card to give up always costs the pool's lowest value. The
///   price of paying the cost at all, which reaches the activation that
///   demanded it through the search, does not move with the reordering.
pub(crate) fn keep_prices(
    state: &GameState,
    pool: &[ObjectId],
    plan: Option<PlanState>,
) -> Vec<(ObjectId, f64)> {
    let mut keyed: Vec<_> = pool
        .iter()
        .map(|&id| (id, keep_key(state, id, plan)))
        .collect();
    keyed.sort_by(|a, b| cmp_keep(&a.1, &b.1));
    let mut values: Vec<f64> = keyed.iter().map(|(_, key)| key.1).collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

    let mut prices: Vec<(ObjectId, f64)> = Vec::with_capacity(keyed.len());
    let mut previous: Option<((KeepTier, f64), f64)> = None;
    for ((id, key), value) in keyed.into_iter().zip(values) {
        let price = match previous {
            None => value,
            Some((prev_key, prev_price)) => match cmp_keep(&prev_key, &key) {
                Ordering::Less => value.max(prev_price + KEEP_PRICE_STEP),
                Ordering::Equal | Ordering::Greater => prev_price,
            },
        };
        prices.push((id, price));
        previous = Some((key, price));
    }
    prices
}

/// CR 118.1 + CR 701.9b: the value the keep order surrenders when `count`
/// cards of `pool` are discarded to pay a cost, on the [`intrinsic_value`]
/// scale. The cards are the first `count` in [`cmp_keep`] order, the ones
/// `policies::payment_selection` then discards.
///
/// Each card is lost at its own value, with one exception. A
/// [`KeepTier::Surplus`] mana source costs nothing: past the plan's target it
/// adds nothing the plan wants (CR 305.2, one land a turn), which is why
/// [`worth_drawing`] calls it not worth drawing, and a card not worth drawing
/// is not worth keeping either. Pricing it at the pool's lowest value instead
/// made a flooded loot look underwater whenever the library's mean fell under
/// a land's 3.0 (signals-r6, seed 1033). Any other card the order gives up is
/// a real loss at its own value, so a needed land kept over a 9.0 creature
/// costs the creature's 9.0, not the pool's lowest value that
/// [`keep_prices`] would assign it.
pub(crate) fn given_up_value(
    state: &GameState,
    pool: &[ObjectId],
    count: usize,
    plan: Option<PlanState>,
) -> f64 {
    given_up_cards(state, pool, count, plan)
        .into_iter()
        .map(|id| held_value(state, id, plan))
        .sum()
}

/// The value of holding one card, on the [`intrinsic_value`] scale: its own
/// value, except a [`KeepTier::Surplus`] mana source, which adds nothing the
/// plan wants (CR 305.2, one land a turn). The single per-card price behind
/// [`given_up_value`], [`expected_draw_value`] and [`expected_loot_gain`], so
/// what a discard gives up and what a draw brings are on one scale.
fn held_value(state: &GameState, id: ObjectId, plan: Option<PlanState>) -> f64 {
    match keep_tier(state, id, plan) {
        KeepTier::Surplus => 0.0,
        KeepTier::Ordinary | KeepTier::NeededManaSource => intrinsic_value(state, id),
    }
}

/// The lands in `player`'s hand the plan counts as [`KeepTier::Surplus`]:
/// past the land target, each adds nothing the plan wants (CR 305.2, one
/// land a turn), the reading [`held_value`] prices at 0. Lands only: an
/// accelerant past the mana target is a card with a body or an effect, and
/// the leaf reads it as one (`fork-checks/754e28a81/JUDGE.md` B2). `plan`
/// `None` (no plan authority) counts none.
pub(crate) fn surplus_lands_in_hand(
    state: &GameState,
    player: PlayerId,
    plan: Option<PlanState>,
) -> usize {
    if plan.is_none() {
        return 0;
    }
    state.players.get(player.0 as usize).map_or(0, |p| {
        p.hand
            .iter()
            .filter(|&&id| {
                mana_role(state, id) == ManaRole::LandDrop
                    && keep_tier(state, id, plan) == KeepTier::Surplus
            })
            .count()
    })
}

/// The first `count` cards of `pool` in [`cmp_keep`] order: the cards a
/// discard of `count` gives up, the ones [`given_up_value`] prices.
pub(crate) fn given_up_cards(
    state: &GameState,
    pool: &[ObjectId],
    count: usize,
    plan: Option<PlanState>,
) -> Vec<ObjectId> {
    keep_prices(state, pool, plan)
        .into_iter()
        .take(count)
        .map(|(id, _)| id)
        .collect()
}

/// CR 121.1 + CR 401.2: the value of one card `player` draws, on the
/// [`intrinsic_value`] scale: the mean over their library. The library's order
/// is hidden, so a draw is priced as an expected card, never as the card on
/// top. A library card the plan counts as a [`KeepTier::Surplus`] mana source
/// is worth 0 to draw, the same price [`given_up_value`] puts on one in hand
/// (a flooded player's draw is often a land the plan does not want). The mean
/// is floored at [`EXPECTED_DRAW_FLOOR`]. `None` only for an empty library,
/// where there is nothing to draw.
pub(crate) fn expected_draw_value(
    state: &GameState,
    player: PlayerId,
    plan: Option<PlanState>,
) -> Option<f64> {
    let library = &state.players.get(player.0 as usize)?.library;
    if library.is_empty() {
        return None;
    }
    let total: f64 = library.iter().map(|&id| held_value(state, id, plan)).sum();
    Some((total / library.len() as f64).max(EXPECTED_DRAW_FLOOR))
}

/// CR 121.1 + CR 701.9a: the card quality a loot of `count` ("draw `count`
/// cards, then discard `count` cards") exchanges for `player`, on the
/// [`intrinsic_value`] scale, with `pool` the hand the discard chooses from
/// (the cards the loot itself draws join it).
///
/// The discard comes after the draw, so the player can always discard a card
/// it just drew and end with the hand it had: a loot never loses card
/// quality. Each of the first `count` cards the keep order gives up from
/// `pool` ([`given_up_cards`], the cards [`given_up_value`] prices) is set
/// against one draw: the pair exchanges the draw's value over the given-up
/// card's, or nothing when the draw is worse, averaged over the library (a
/// draw is an expected card, [`expected_draw_value`]'s reading: the library's
/// order is hidden). A draw with no card left in `pool` to set it against
/// exchanges nothing: the draw is the card discarded. So a dead card in hand
/// against an average library gains about one expected draw, and a hand of
/// keepers against a library of cards under [`KEEP_VALUE_FLOOR`] gains
/// nothing. For `count` above one the pairing is the first-order reading: a
/// later draw could also displace an earlier one, which it does not price.
///
/// `None` when the library holds fewer than `count` cards: the draw runs past
/// it (CR 704.5b), which the search reads as the loss, not as card quality.
pub(crate) fn expected_loot_gain(
    state: &GameState,
    player: PlayerId,
    pool: &[ObjectId],
    count: usize,
    plan: Option<PlanState>,
) -> Option<f64> {
    let library = &state.players.get(player.0 as usize)?.library;
    if library.len() < count || library.is_empty() {
        return None;
    }
    let draws: Vec<f64> = library
        .iter()
        .map(|&id| held_value(state, id, plan))
        .collect();
    let gain = given_up_cards(state, pool, count, plan)
        .into_iter()
        .map(|id| {
            let given_up = held_value(state, id, plan);
            draws
                .iter()
                .map(|draw| (draw - given_up).max(0.0))
                .sum::<f64>()
                / draws.len() as f64
        })
        .sum();
    Some(gain)
}

/// The least an expected draw is worth, on the [`intrinsic_value`] scale.
///
/// A discard's price in draws is the given-up card's value divided by the
/// expected draw, so a mean near zero would price any card at an unbounded
/// number of draws (a 1.0 card at 20 draws against a mean of 0.05) while a mean
/// of exactly zero had no scale at all. At 1.0 no card costs more draws than
/// its own value, and a 1.0 card (a two-mana spell, the cheapest card the FRA
/// decks hold) costs exactly one draw: the flat per-card price this pricing
/// replaced. So the floor and the old fallback agree.
pub(crate) const EXPECTED_DRAW_FLOOR: f64 = 1.0;

/// The smallest price gap [`keep_prices`] puts between two cards that
/// [`cmp_keep`] orders strictly but whose values tie (a surplus land at 3.0
/// against a six-pip spell at 3.0). A tenth of a card: enough to order the
/// selection when the search continuations tie, too small to outweigh a
/// continuation that differs.
pub(crate) const KEEP_PRICE_STEP: f64 = 0.1;

/// The [`intrinsic_value`] floor below which a looked-at card is not worth
/// keeping. The `DigChoice` up-to arm takes nothing when its best card scores
/// under it, and [`worth_drawing`] reads the same floor, so dig, scry and
/// surveil agree on what "worth keeping" means.
pub(crate) const KEEP_VALUE_FLOOR: f64 = 0.1;

/// CR 701.22a + CR 701.25a: whether a looked-at card is worth leaving on top of
/// the library to be drawn, rather than putting it on the bottom (scry) or into
/// the graveyard (surveil).
///
/// Two tests, both read from the existing authorities rather than a second
/// value function:
/// - the scalar: [`intrinsic_value`] at or above [`KEEP_VALUE_FLOOR`];
/// - the plan: a mana source in the [`KeepTier::Surplus`] tier is an excess
///   card. A land scores 3.0 on the scalar, so only this test can bin one; per
///   CR 305.2 a player normally plays one land a turn, and past the plan's land
///   target another land is surplus. An accelerant past the mana target is
///   not (its mana role adds nothing there; its other value is read).
///
/// `plan == None` (no plan authority, as on the rollout path) leaves every card
/// `Ordinary`, so the scalar alone decides.
pub(crate) fn worth_drawing(state: &GameState, obj_id: ObjectId, plan: Option<PlanState>) -> bool {
    keep_tier(state, obj_id, plan) != KeepTier::Surplus
        && intrinsic_value(state, obj_id) >= KEEP_VALUE_FLOOR
}

/// The zone-agnostic scalar a card is ranked by: higher is more desirable to
/// keep or draw. One scale for every card:
///
/// - a creature is its body, `1.5 * power + toughness`, plus half a point a
///   pip, plus what its abilities do ([`creature_ability_payoff`]): an
///   activated ability once, its prepare spell while it has one to cast, an
///   enters trigger on the current board and any other trigger once with its
///   sign. Before this a creature was its body and pips alone, so Liliana the
///   Repentant (6.0) and Fatehold Chronologist (4.5) went before a cantrip
///   (`fork-checks/96d0bfd2a/JUDGE.md` S4). A land is 3.0, unchanged.
/// - a noncreature, nonland card is half a point a pip plus what it does,
///   read from the engine's parse of its abilities ([`noncreature_payoff`]):
///   removal by the creature it answers, card draw by the cards it nets, a
///   creature token by its body, a planeswalker by its loyalty or the best
///   line it pays for, and what a minus line leaves of her. Before this,
///   every such card was the pips alone, so The Theorist sat at 2.0 and Last
///   Gasp at 1.0 under a basic land's 3.0, and every keep-order seam gave
///   removal and card draw away first
///   (`fork-checks/ef45b56b6/JUDGE.md`, `fork-checks/9f72018f4/OPEN.md`).
///
/// It is the one ordering every consumer reads: the keep order ([`keep_key`],
/// so the cleanup discard and the discard-cost prices), the expected draw
/// ([`expected_draw_value`]), scry and surveil ([`worth_drawing`]), dig,
/// reveal and choose-from-zone selections, the mulligan bottom and the
/// large-board cast baseline in `search`.
pub(crate) fn intrinsic_value(state: &GameState, obj_id: ObjectId) -> f64 {
    value_with(state, obj_id, DrawPricing::LibraryMean, Depth::Top)
}

/// Whether [`value_with`] prices a creature's abilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    /// The card being priced: a creature's abilities count.
    Top,
    /// A card priced inside another card's price (the creature a removal
    /// spell kills, the card a reanimation returns, the creature a fight
    /// meets, the library mean): a creature is its body and pips, the
    /// pricing before creature abilities were read. A price never asks for
    /// itself again, and every noncreature card's price is unchanged.
    Nested,
}

/// A card's price inside another card's price ([`Depth::Nested`]).
fn nested_value(state: &GameState, obj_id: ObjectId) -> f64 {
    value_with(state, obj_id, DrawPricing::LibraryMean, Depth::Nested)
}

/// How [`value_with`] prices a card drawn by the card being valued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawPricing {
    /// One expected card from the owner's library ([`draw_unit`]).
    LibraryMean,
    /// Not priced. The library mean behind [`draw_unit`] is taken over this
    /// reading, so a draw spell in the library never asks for the mean of the
    /// library it sits in.
    Unpriced,
}

fn value_with(state: &GameState, obj_id: ObjectId, draws: DrawPricing, depth: Depth) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };
    let types = &obj.card_types.core_types;
    let mut value = 0.0;

    // Creatures: value based on power + toughness
    if types.contains(&CoreType::Creature) {
        let power = obj.power.unwrap_or(0) as f64;
        let toughness = obj.toughness.unwrap_or(0) as f64;
        value += power * 1.5 + toughness;
    }

    // Lands: moderate value (mana development)
    if types.contains(&CoreType::Land) {
        value += 3.0;
    }

    // The mana cost, half a point a pip: a proxy for power on every card.
    if let engine::types::mana::ManaCost::Cost { shards, generic } = &obj.mana_cost {
        let total_mana = shards.len() as f64 + *generic as f64;
        value += total_mana * 0.5;
    }

    if !types.contains(&CoreType::Creature) && !types.contains(&CoreType::Land) {
        value += noncreature_payoff(state, obj, draws);
    } else if types.contains(&CoreType::Creature) && depth == Depth::Top {
        value += creature_ability_payoff(state, obj, draws);
    }
    value
}

/// What a noncreature, nonland card does, on the creature scale of
/// [`intrinsic_value`], from the engine's parse of the card:
///
/// - **Instant or sorcery**: its spell abilities as cast from hand (CR 601.2a;
///   an "instead" branch decided by where it is cast from, as
///   `self_cost::effects_cast_from` reads it). A modal spell does one mode,
///   so it is worth its best (CR 700.2).
/// - **Permanent**: each triggered ability once, what it does when it
///   resolves (an enters-the-battlefield exile, a card each draw step). An
///   Aura harmful to the creature it enchants is removal (CR 303.4a; its
///   static abilities apply in layer 6, CR 613.1f).
/// - **Planeswalker**: the larger of its loyalty (CR 306.5b: it enters with
///   its printed loyalty) and the best loyalty ability that loyalty can pay
///   for (CR 606.3: one activation a turn; CR 606.6: a negative cost needs
///   that many counters), a minus line with the loyalty it leaves and her
///   triggers only if she survives it (CR 704.5i), [`planeswalker_payoff`].
///
/// Each effect is priced by [`effect_payoff`]; anything else it does is 0,
/// which is the old pricing.
fn noncreature_payoff(state: &GameState, obj: &GameObject, draws: DrawPricing) -> f64 {
    let mut payoff = 0.0;

    let spells: Vec<&AbilityDefinition> = obj
        .abilities
        .iter()
        .filter(|ability| ability.kind == AbilityKind::Spell)
        .collect();
    if !spells.is_empty() {
        let values = spells
            .iter()
            .map(|ability| definition_payoff(state, obj, ability, draws));
        payoff += if obj.modal.is_some() {
            values.fold(0.0, f64::max)
        } else {
            values.sum()
        };
    }

    payoff += planeswalker_payoff(
        state,
        obj,
        draws,
        trigger_payoff(state, obj, draws, Triggers::All),
    );

    if aura_polarity(obj) == EffectPolarity::Harmful {
        if let Some(Keyword::Enchant(filter)) = obj
            .keywords
            .iter()
            .find(|keyword| matches!(keyword, Keyword::Enchant(_)))
        {
            if filter_admits_creature(filter) && !filter_names_only_own_permanents(filter) {
                payoff += answer_value(state, obj, None, filter, ANSWER_FLOOR, |_| true);
            }
        }
    }

    payoff
}

/// Which of a permanent's triggered abilities [`trigger_payoff`] prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Triggers {
    /// Every trigger once: the card as it would be cast.
    All,
    /// The permanent as it stands on the battlefield: its own
    /// enters-the-battlefield trigger has already happened (CR 603.6a), so
    /// only what it can still do is priced.
    StillToFire,
}

/// Each triggered ability of `obj` once, what it does when it resolves.
fn trigger_payoff(state: &GameState, obj: &GameObject, draws: DrawPricing, which: Triggers) -> f64 {
    obj.trigger_definitions
        .iter_unchecked()
        .filter(|trigger| which == Triggers::All || !is_own_etb(&trigger.definition))
        .filter_map(|trigger| trigger.definition.execute.as_deref())
        .map(|execute| definition_payoff(state, obj, execute, draws))
        .sum()
}

/// CR 603.6a: "When this permanent enters": the trigger of its own entering.
fn is_own_etb(trigger: &engine::types::ability::TriggerDefinition) -> bool {
    trigger.mode == engine::types::triggers::TriggerMode::ChangesZone
        && trigger.valid_card == Some(TargetFilter::SelfRef)
        && trigger.destination == Some(Zone::Battlefield)
}

/// A permanent's triggers, `triggers` (priced by [`trigger_payoff`]), and for
/// a planeswalker the one line its loyalty buys, priced together because a
/// line that spends her loyalty to 0 also ends her triggers. Anything that is
/// not a planeswalker is its `triggers` as given.
///
/// A planeswalker is the largest of:
/// - her loyalty (CR 306.5b: she enters with her printed loyalty; CR 306.5c:
///   on the battlefield, her loyalty counters) or a plus or zero loyalty
///   ability, whichever is larger, plus her triggers;
/// - a minus loyalty ability her loyalty can pay for (CR 606.6: a negative
///   cost needs that many counters), plus the loyalty left after paying it
///   (CR 606.4: the cost removes that many counters), plus her triggers only
///   when that remainder is above 0. At 0 she is put into her owner's
///   graveyard (CR 704.5i), so a line that takes her there leaves no
///   loyalty and fires no trigger again.
///
/// Not the loyalty plus every payable ability. CR 606.3 allows one loyalty
/// activation a turn, and the loyalty is the budget that pays for the line,
/// not value on top of it; only what a minus line leaves is still hers.
/// Summed, The Theorist at 6 loyalty read 6 + [+1] 2.5 + [-6] 9.0 for every
/// ability at once (`fork-checks/5e66b2a30/JUDGE.md` rows 3 and 6); with her
/// draw trigger credited beside an ultimate that kills her, she read one card
/// high at 6 and the same as at 7 (`fork-checks/7f538589f/JUDGE.md` row 3).
/// A repeatable minus line gets its future through the remainder, not a
/// multiplier. How she spends her loyalty turn by turn is not priced.
///
/// The one rule [`intrinsic_value`] (through [`noncreature_payoff`], every
/// trigger) and the search's leaf credit ([`permanent_card_credit`], the
/// triggers still to fire) both read.
fn planeswalker_payoff(
    state: &GameState,
    obj: &GameObject,
    draws: DrawPricing,
    triggers: f64,
) -> f64 {
    if !obj.card_types.core_types.contains(&CoreType::Planeswalker) {
        return triggers;
    }
    let loyalty = obj.loyalty.unwrap_or(0) as i32;
    obj.abilities
        .iter()
        .filter_map(|ability| match ability.cost {
            Some(AbilityCost::Loyalty { amount }) if amount >= -loyalty => {
                let line = definition_payoff(state, obj, ability, draws);
                Some(if amount < 0 {
                    let left = loyalty + amount;
                    line + f64::from(left) + if left > 0 { triggers } else { 0.0 }
                } else {
                    line + triggers
                })
            }
            _ => None,
        })
        .fold(f64::from(loyalty) + triggers, f64::max)
}

/// What a creature's abilities do, on the creature scale of
/// [`intrinsic_value`], beside its body: the pricing the noncreature and
/// planeswalker paths use ([`definition_payoff`], each ability once), read
/// from the engine's parse of the card. Four terms, each the line the scalar
/// did not count before (the step 0 table,
/// `deck-builder/sims/fra-phase/2026-10-20-creature-abilities/NOTE.md`):
///
/// - **Activated abilities** ([`activated_payoff`]): each once.
/// - **The prepare spell** ([`prepare_spell_payoff`]): while it has one to
///   cast.
/// - **Its enters trigger** (CR 603.6a): what it does on the board it would
///   enter now, for a creature not yet on the battlefield; on the battlefield
///   it has fired and is not priced again (the [`Triggers::StillToFire`]
///   reading).
/// - **Every other trigger** once, with its sign ([`self_mill_cost`]).
///
/// A trigger whose resolution removes the creature itself from the
/// battlefield ([`removes_own_source`]) nets the body it removes, and a
/// trigger whose event its controller does not decide alone
/// ([`controller_decides`]) is priced at [`UNDECIDED_TRIGGER_SHARE`] of what
/// it does ([`creature_trigger_payoff`]).
///
/// Optional parts (a "you may" trigger) are never below 0. Keywords are not
/// priced. The leaf does not read this: [`permanent_card_credit`] credits no
/// creature, and the board sum in `eval` credits its body.
fn creature_ability_payoff(state: &GameState, obj: &GameObject, draws: DrawPricing) -> f64 {
    let on_battlefield = obj.zone == Zone::Battlefield;
    let triggers: f64 = obj
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .filter(|trigger| !(on_battlefield && is_own_etb(trigger)))
        .filter_map(|trigger| trigger.execute.as_deref().map(|execute| (trigger, execute)))
        .map(|(trigger, execute)| creature_trigger_payoff(state, obj, trigger, execute, draws))
        .sum();
    activated_payoff(state, obj, draws) + prepare_spell_payoff(state, obj, draws) + triggers
}

/// One triggered ability of a creature, once (CR 603.2: it triggers when its
/// event happens), with its sign ([`self_mill_cost`]):
///
/// - When its resolution removes the creature itself from the battlefield
///   ([`removes_own_source`]: it is sacrificed, destroyed, bounced, exiled or
///   shuffled away), the creature's body and pips ([`nested_value`]) are
///   what it pays, netted from the payoff, never below 0: a trigger that only
///   removes the creature (the one-shot attacker whose haste and evasion are
///   not priced here) leaves the body as it was.
/// - When its event is not one its controller decides alone
///   ([`controller_decides`]), it is priced at [`UNDECIDED_TRIGGER_SHARE`] of
///   that payoff: what it does happens only if the event does.
///
/// An optional trigger is never below 0.
fn creature_trigger_payoff(
    state: &GameState,
    obj: &GameObject,
    trigger: &TriggerDefinition,
    execute: &AbilityDefinition,
    draws: DrawPricing,
) -> f64 {
    let mut payoff =
        definition_payoff(state, obj, execute, draws) + self_mill_cost(state, obj, execute);
    if removes_own_source(execute) {
        payoff = (payoff - nested_value(state, obj.id)).max(0.0);
    }
    if !controller_decides(trigger) {
        payoff *= UNDECIDED_TRIGGER_SHARE;
    }
    if trigger.optional || execute.optional {
        payoff.max(0.0)
    } else {
        payoff
    }
}

/// The share of its payoff a creature's trigger keeps when its event is not
/// one its controller decides alone ([`controller_decides`]): the chance the
/// event happens. UNTUNED.
pub(crate) const UNDECIDED_TRIGGER_SHARE: f64 = 0.5;

/// Whether a trigger's event is one its controller brings about alone. Not
/// when:
///
/// - it is combat damage (CR 120.2a; any damage mode read with
///   `DamageKindFilter::CombatOnly`): the defending player's blocks (CR
///   509.1) decide whether and where combat damage is dealt as much as the
///   attack does;
/// - its actor is an opponent's (CR 102.2): the object or player the event
///   is about (`valid_card`, `valid_source`, `valid_subject_player`, and for
///   a mode other than damage the acting player in `valid_target`, as "an
///   opponent casts" and "an opponent draws" parse) names only an opponent's;
/// - it can trigger only during an opponent's turn
///   (`TriggerConstraint::OnlyDuringOpponentsTurn`), except a phase or step
///   trigger: that step comes every turn.
///
/// Everything else is its controller's own act or a step that comes anyway
/// (its own cast, its own creature entering, its own attack, an upkeep), and
/// is priced as before.
fn controller_decides(trigger: &TriggerDefinition) -> bool {
    let damage_mode = matches!(
        trigger.mode,
        TriggerMode::DamageDone
            | TriggerMode::DamageDoneOnce
            | TriggerMode::DamageAll
            | TriggerMode::DamageDealtOnce
            | TriggerMode::DamageDoneOnceByController
            | TriggerMode::DamageReceived
            | TriggerMode::ExcessDamage
            | TriggerMode::ExcessDamageAll
    );
    if damage_mode && trigger.damage_kind == DamageKindFilter::CombatOnly {
        return false;
    }
    let actor = [
        trigger.valid_card.as_ref(),
        trigger.valid_source.as_ref(),
        trigger.valid_subject_player.as_ref(),
        trigger.valid_target.as_ref().filter(|_| !damage_mode),
    ];
    if actor.into_iter().flatten().any(filter_names_only_opponents) {
        return false;
    }
    !(trigger.constraint == Some(TriggerConstraint::OnlyDuringOpponentsTurn)
        && trigger.mode != TriggerMode::Phase)
}

/// CR 102.2: whether `filter` admits only opponents or what opponents
/// control.
fn filter_names_only_opponents(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Opponent => true,
        TargetFilter::Typed(typed) => matches!(
            typed.controller,
            Some(ControllerRef::Opponent | ControllerRef::TargetOpponent)
        ),
        TargetFilter::And { filters } => filters.iter().any(filter_names_only_opponents),
        TargetFilter::Or { filters } => {
            !filters.is_empty() && filters.iter().all(filter_names_only_opponents)
        }
        _ => false,
    }
}

/// Whether an ability's resolution, as [`definition_payoff`] reads it
/// ([`priced_effects`]), moves its own source off the battlefield: CR
/// 701.21a sacrifice, CR 701.8a destroy, a return to hand, or a move from
/// the battlefield to any other zone (an exile, a shuffle into a library).
fn removes_own_source(ability: &AbilityDefinition) -> bool {
    priced_effects(ability)
        .into_iter()
        .any(|effect| match effect {
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            }
            | Effect::Destroy {
                target: TargetFilter::SelfRef,
                ..
            }
            | Effect::Bounce {
                target: TargetFilter::SelfRef,
                ..
            } => true,
            Effect::ChangeZone {
                origin: None | Some(Zone::Battlefield),
                destination,
                target: TargetFilter::SelfRef,
                ..
            } => *destination != Zone::Battlefield,
            _ => false,
        })
}

/// CR 602.1: a creature's activated abilities, each priced once at its
/// payoff ([`definition_payoff`]), never below 0 (activating is optional).
/// Read: abilities activated from the battlefield (not from hand or
/// graveyard), not mana abilities (CR 605.1a; a mana source is the keep
/// order's tier), not one whose cost sacrifices (the body it would cost is
/// not netted, so it is left out rather than counted twice), and not one that
/// can be activated only once and already was (CR 702.177a, exhaust;
/// `reanimator::once_and_spent`).
///
/// An ability whose mana is not within [`ABILITY_REACH_LAND_DROPS`] land
/// drops (`mana_colors::cost_within_land_drops`: sources now plus that many
/// lands from hand, each pip its own coloured source) is discounted to
/// [`UNREACHED_ABILITY_SHARE`] of its payoff.
fn activated_payoff(state: &GameState, obj: &GameObject, draws: DrawPricing) -> f64 {
    obj.abilities
        .iter()
        .enumerate()
        .filter(|(index, ability)| {
            ability.kind == AbilityKind::Activated
                && matches!(ability.activation_zone, None | Some(Zone::Battlefield))
                && !is_mana_ability(ability)
                && !ability
                    .cost_categories()
                    .contains(&CostCategory::SacrificesPermanent)
                && !once_and_spent(state, obj.id, *index, ability)
        })
        .map(|(_, ability)| {
            let payoff = definition_payoff(state, obj, ability, draws).max(0.0);
            if payoff == 0.0 {
                return 0.0;
            }
            let in_reach = ability
                .cost
                .as_ref()
                .and_then(activation_mana)
                .is_none_or(|mana| {
                    cost_within_land_drops(state, obj.controller, &mana, ABILITY_REACH_LAND_DROPS)
                });
            if in_reach {
                payoff
            } else {
                payoff * UNREACHED_ABILITY_SHARE
            }
        })
        .sum()
}

/// How many turns' land drops an activated ability's mana may still be away
/// and count at its full payoff (CR 305.2: one land a turn). UNTUNED.
pub(crate) const ABILITY_REACH_LAND_DROPS: usize = 2;

/// The share of its payoff an activated ability keeps when its mana is
/// further away than [`ABILITY_REACH_LAND_DROPS`]. UNTUNED.
pub(crate) const UNREACHED_ABILITY_SHARE: f64 = 0.5;

/// The mana an activation cost asks for, its mana components added together
/// (CR 601.2h: the total cost is paid at once), as `policies::ritual_sink`
/// reads a cost's mana value; `None` for a cost with no fixed mana part (no
/// mana, an X cost, a choice of costs), which is read as in reach.
fn activation_mana(cost: &AbilityCost) -> Option<ManaCost> {
    match cost {
        AbilityCost::Mana { cost } => Some(cost.clone()),
        AbilityCost::Composite { costs } => costs
            .iter()
            .filter_map(activation_mana)
            .reduce(|total, part| total.plus(&part)),
        _ => None,
    }
}

/// CR 722.2a + CR 722.3c: the prepare spell's payoff while the creature has
/// it to cast: on the battlefield while it is prepared (casting the copy
/// unprepares it), and from any other zone when it enters prepared (CR
/// 722.3a, a "~ enters prepared" replacement). The spell is priced as a spell
/// in hand is ([`definition_payoff`] over its spell abilities, its best mode
/// when modal), cast by the creature's controller, never below 0 (the copy
/// "may" be cast). Its mana is not discounted.
fn prepare_spell_payoff(state: &GameState, obj: &GameObject, draws: DrawPricing) -> f64 {
    let Some(face) = prepare_face(obj) else {
        return 0.0;
    };
    let has_it = match obj.zone {
        Zone::Battlefield => obj.prepared.is_some(),
        _ => enters_prepared(obj),
    };
    if !has_it {
        return 0.0;
    }
    let values = face
        .abilities
        .iter()
        .filter(|ability| ability.kind == AbilityKind::Spell)
        .map(|ability| definition_payoff(state, obj, ability, draws));
    let payoff = if face.modal.is_some() {
        values.fold(0.0, f64::max)
    } else {
        values.sum()
    };
    payoff.max(0.0)
}

/// CR 722.2a + CR 722.2b: the prepare spell `obj` has now. The read the
/// engine's `printed_cards::effective_prepare_face` makes (crate-private
/// there): while a Layer 1 copy effect applies, the copy's prepare spell (or
/// none); otherwise its own face of the Prepare layout.
fn prepare_face(obj: &GameObject) -> Option<&BackFaceData> {
    if obj.layer1_copy_effect.is_some() {
        return obj.copied_prepare_face.as_deref();
    }
    obj.back_face
        .as_ref()
        .filter(|face| face.layout_kind == Some(LayoutKind::Prepare))
}

/// CR 722.3a: "This creature enters prepared": a replacement on its own
/// move to the battlefield whose effect prepares it.
fn enters_prepared(obj: &GameObject) -> bool {
    obj.replacement_definitions
        .iter_unchecked()
        .any(|definition| {
            definition.destination_zone == Some(Zone::Battlefield)
                && definition.valid_card == Some(TargetFilter::SelfRef)
                && definition.execute.as_deref().is_some_and(|execute| {
                    matches!(
                        &*execute.effect,
                        Effect::BecomePrepared {
                            target: TargetFilter::SelfRef,
                            ..
                        }
                    )
                })
        })
}

/// CR 701.17a: a trigger that mills its controller's own library, as a cost:
/// [`SELF_MILL_CARD_COST`] a card, unless its controller still has a
/// reanimation to use (`reanimator::has_live_reanimation`, the card's own
/// counted), when the milled cards are fuel and cost nothing. 0 for a trigger
/// that mills nothing of its controller's.
fn self_mill_cost(state: &GameState, obj: &GameObject, execute: &AbilityDefinition) -> f64 {
    let milled: i32 = collect_chain_effects(execute)
        .into_iter()
        .map(|effect| match effect {
            Effect::Mill {
                count,
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            } => resolve_quantity(state, count, obj.controller, obj.id).max(0),
            _ => 0,
        })
        .sum();
    if milled == 0 || has_live_reanimation(state, obj.controller, obj) {
        return 0.0;
    }
    -f64::from(milled) * SELF_MILL_CARD_COST
}

/// What a card milled from its controller's own library costs when nothing
/// can return it: half a point, a pip's worth. UNTUNED.
pub(crate) const SELF_MILL_CARD_COST: f64 = 0.5;

/// What a spell still on the stack is worth to the search's leaf, in cards
/// (`eval::EvalFeatures::card_credit`). CR 405.1 + CR 601.2a: a cast spell is
/// the card, moved from hand to the stack; until it resolves or is countered it
/// is not a card lost. A noncreature permanent spell is valued as resolved
/// ([`permanent_card_credit`], CR 608.3: it becomes that permanent), so a
/// leaf where an opponent could still respond reads the cast as the leaf after
/// it would. That is not discounted for the chance of an answer, which the
/// search explores as the opponent's own move. Any other spell is the card it
/// was: an instant or sorcery has not yet done anything, and a creature spell
/// is priced by its board terms once it resolves. A copy of a spell is put on
/// the stack, not moved there from a hand (CR 707.10), so it earns nothing.
pub(crate) fn stack_spell_card_credit(state: &GameState, obj: &GameObject) -> f64 {
    if obj.is_token || obj.is_copy {
        return 0.0;
    }
    let types = &obj.card_types.core_types;
    let noncreature_permanent = !types.contains(&CoreType::Instant)
        && !types.contains(&CoreType::Sorcery)
        && !types.contains(&CoreType::Creature)
        && !types.contains(&CoreType::Land);
    if noncreature_permanent {
        permanent_card_credit(state, obj).max(1.0)
    } else {
        1.0
    }
}

/// What a resolved noncreature permanent is worth to the search's leaf, in
/// cards: the scale of the hand-size weight that values the card it was in
/// hand (`eval::EvalFeatures::card_credit`).
///
/// - A permanent represented by a card is that card, 1.0, plus what it can
///   still do in expected draws ([`draw_unit`], the unit [`intrinsic_value`]
///   prices a drawn card in): the larger of a planeswalker's loyalty and the
///   best loyalty ability its loyalty pays for, a minus line with the loyalty
///   it leaves, and the triggers it has yet to fire unless that line takes her
///   to 0 ([`planeswalker_payoff`], as [`intrinsic_value`] reads it). So a
///   cast never scores below the card it was, and The Theorist at 4 loyalty
///   is her card plus her loyalty (over her [+1]'s token) and her draw.
/// - CR 121.4 + CR 704.5b: when its recurring mandatory draws leave the
///   controller fewer than six rounds of library
///   (`anti_self_harm::recurring_self_draw_exposure`, the reading behind the
///   cast's deck-out price), its draws are credited at nothing.
/// - CR 111.1 + CR 707.10: a token or a copy is not a card, so it earns no card
///   credit (it keeps the `card_advantage` weight alone). A creature is priced
///   by its board terms, and a land or any other standing mana source (a mana
///   rock, `is_intrinsic_mana_source`) by the mana-development offset, which
///   is sized as a source's whole value against the hand card it costs; all
///   are 0 here, so the rock-versus-body margins `ai_quality` discloses stand.
///
/// An Aura's effect on its host is not read here: the leaf reads the host
/// (`eval::neutralized_creatures`).
pub(crate) fn permanent_card_credit(state: &GameState, obj: &GameObject) -> f64 {
    let types = &obj.card_types.core_types;
    if obj.is_token
        || obj.is_copy
        || types.contains(&CoreType::Creature)
        || types.contains(&CoreType::Land)
        || is_intrinsic_mana_source(obj)
    {
        return 0.0;
    }
    let draws = match crate::policies::anti_self_harm::recurring_self_draw_exposure(
        state,
        obj,
        obj.controller,
    ) {
        Some(exposure) if exposure.is_short() => DrawPricing::Unpriced,
        _ => DrawPricing::LibraryMean,
    };
    let payoff = planeswalker_payoff(
        state,
        obj,
        draws,
        trigger_payoff(state, obj, draws, Triggers::StillToFire),
    );
    if payoff <= 0.0 {
        return 1.0;
    }
    1.0 + payoff / draw_unit(state, obj.controller)
}

/// What a card can still do from its owner's graveyard, in cards: the scale
/// of the hand-size weight that values a card in hand
/// (`eval::EvalFeatures::graveyard_casts`), so the line that keeps a flashback
/// card's graveyard cast and the line that spends it are read on the same
/// cards. Read for a card in the graveyard, and for a card in hand, which
/// carries the same credit beside its one hand card: kept, a flashback card
/// gives its hand cast and then its graveyard cast (CR 702.34a); discarded,
/// it keeps the graveyard cast, so a discard loses the hand card alone (the
/// `fork-checks/96d0bfd2a/JUDGE.md` B1 reading). The leaf therefore reads a
/// flashback card in hand above a plain card of the same hand value.
///
/// - CR 702.34a + CR 702.138a: a card with flashback or escape can be cast
///   from its owner's graveyard. An instant or sorcery is what its spell
///   abilities do cast from there ([`definition_payoff_from`] with the
///   graveyard as origin, its best mode when modal, CR 700.2), in expected
///   draws ([`draw_unit`]); Twinned Vision's flashback draws two. A permanent
///   cast by escape is the card a cast from hand would put on the
///   battlefield, one card, as the leaf reads a card in hand.
/// - CR 702.84a: unearth returns a creature card to the battlefield until the
///   next end step, so it is what the creature does on entering (its own
///   enters triggers, [`creature_trigger_payoff`]), not its body.
///
/// Each card its cost discards (CR 601.2h: Twinned Vision's "Discard a
/// card") is one card less, never below 0. Credited only while the cast is
/// in reach: its mana within [`ABILITY_REACH_LAND_DROPS`] land drops of the
/// owner's sources (`mana_colors::cost_within_land_drops`, CR 305.2), the
/// reading an activated ability's reach takes, and as many other cards in
/// that graveyard as its cost exiles (escape's "Exile N other cards", CR
/// 702.138a). Any card in another zone or without such a keyword, and a
/// token or a copy (CR 111.1 + CR 707.10: not a card), is 0.
pub(crate) fn graveyard_cast_credit(state: &GameState, obj: &GameObject) -> f64 {
    if !matches!(obj.zone, Zone::Graveyard | Zone::Hand)
        || obj.is_token
        || obj.is_copy
        || being_cast(state, obj.id)
    {
        return 0.0;
    }
    let Some(cast) = GraveyardCast::of(state, obj) else {
        return 0.0;
    };
    let caster = obj.owner;
    let mana_in_reach = cast
        .mana
        .as_ref()
        .is_none_or(|mana| cost_within_land_drops(state, caster, mana, ABILITY_REACH_LAND_DROPS));
    let in_graveyard = state.graveyard_of(caster).len();
    let others_in_graveyard = if obj.zone == Zone::Graveyard {
        in_graveyard.saturating_sub(1)
    } else {
        in_graveyard
    };
    if !mana_in_reach || others_in_graveyard < cast.graveyard_exiles {
        return 0.0;
    }
    let types = &obj.card_types.core_types;
    let cards = match cast.kind {
        GraveyardCastKind::Cast
            if types.contains(&CoreType::Instant) || types.contains(&CoreType::Sorcery) =>
        {
            let values = obj
                .abilities
                .iter()
                .filter(|ability| ability.kind == AbilityKind::Spell)
                .map(|ability| {
                    definition_payoff_from(
                        state,
                        obj,
                        ability,
                        DrawPricing::LibraryMean,
                        Zone::Graveyard,
                    )
                });
            let payoff = if obj.modal.is_some() {
                values.fold(0.0, f64::max)
            } else {
                values.sum()
            };
            payoff / draw_unit(state, caster)
        }
        GraveyardCastKind::Cast => 1.0,
        GraveyardCastKind::Unearth => {
            let payoff: f64 = obj
                .trigger_definitions
                .iter_unchecked()
                .map(|entry| &entry.definition)
                .filter(|trigger| is_own_etb(trigger))
                .filter_map(|trigger| trigger.execute.as_deref().map(|execute| (trigger, execute)))
                .map(|(trigger, execute)| {
                    creature_trigger_payoff(state, obj, trigger, execute, DrawPricing::LibraryMean)
                })
                .sum();
            payoff / draw_unit(state, caster)
        }
    };
    (cards - f64::from(cast.discards)).max(0.0)
}

/// CR 601.2a: a spell being cast is put on the stack before its costs are
/// paid; while the cast is underway its card can still sit in its origin
/// zone, and the stack's credit (`stack_spell_card_credit`) is what reads it,
/// not the graveyard's.
fn being_cast(state: &GameState, id: ObjectId) -> bool {
    state.stack.iter().any(|entry| {
        entry.source_id == id
            && matches!(
                entry.kind,
                engine::types::game_state::StackEntryKind::Spell { .. }
            )
    })
}

/// How a card leaves its graveyard by its own recursion keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraveyardCastKind {
    /// CR 702.34a flashback, CR 702.138a escape: cast from the graveyard.
    Cast,
    /// CR 702.84a unearth: an activated ability that returns it to the
    /// battlefield until the next end step.
    Unearth,
}

/// A graveyard card's recursion keyword and what its cost asks for.
struct GraveyardCast {
    kind: GraveyardCastKind,
    /// The mana the cost asks for, `None` for a cost with no fixed mana part
    /// (read as in reach, as [`activation_mana`] reads one).
    mana: Option<ManaCost>,
    /// Cards the cost discards from hand.
    discards: i32,
    /// Other cards the cost exiles from the graveyard.
    graveyard_exiles: usize,
}

impl GraveyardCast {
    /// The first flashback, escape or unearth keyword on `obj`
    /// (`zone_eval::has_recursion_keyword`'s set).
    fn of(state: &GameState, obj: &GameObject) -> Option<Self> {
        obj.keywords.iter().find_map(|keyword| match keyword {
            Keyword::Flashback(FlashbackCost::Mana(mana))
            | Keyword::Escape(EscapeCost::Mana(mana)) => Some(Self {
                kind: GraveyardCastKind::Cast,
                mana: Some(mana.clone()),
                discards: 0,
                graveyard_exiles: 0,
            }),
            Keyword::Flashback(FlashbackCost::NonMana(cost))
            | Keyword::Escape(EscapeCost::NonMana(cost)) => Some(Self {
                kind: GraveyardCastKind::Cast,
                mana: activation_mana(cost),
                discards: cost_discards(state, obj, cost),
                graveyard_exiles: cost_graveyard_exiles(cost),
            }),
            Keyword::Unearth(mana) => Some(Self {
                kind: GraveyardCastKind::Unearth,
                mana: Some(mana.clone()),
                discards: 0,
                graveyard_exiles: 0,
            }),
            _ => None,
        })
    }
}

/// CR 701.9a: the cards a cost discards from its owner's hand.
fn cost_discards(state: &GameState, obj: &GameObject, cost: &AbilityCost) -> i32 {
    match cost {
        AbilityCost::Discard { count, .. } => {
            resolve_quantity(state, count, obj.owner, obj.id).max(0)
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .map(|cost| cost_discards(state, obj, cost))
            .sum(),
        _ => 0,
    }
}

/// CR 702.138a: the other cards a cost exiles from its graveyard.
fn cost_graveyard_exiles(cost: &AbilityCost) -> usize {
    match cost {
        AbilityCost::Exile {
            count,
            zone: Some(Zone::Graveyard),
            ..
        } => *count as usize,
        AbilityCost::Composite { costs } => costs.iter().map(cost_graveyard_exiles).sum(),
        _ => 0,
    }
}

/// One ability's payoff: its best mode when it is modal (CR 700.2), otherwise
/// the effects it performs as cast from hand, or for any other branch shape
/// the unconditional links of its chain.
fn definition_payoff(
    state: &GameState,
    obj: &GameObject,
    ability: &AbilityDefinition,
    draws: DrawPricing,
) -> f64 {
    definition_payoff_from(state, obj, ability, draws, Zone::Hand)
}

/// [`definition_payoff`] for a spell cast from `origin` (CR 601.2a): an
/// "instead" branch its origin decides is read for that origin
/// ([`priced_effects_from`]); Twinned Vision cast from the graveyard draws
/// two.
fn definition_payoff_from(
    state: &GameState,
    obj: &GameObject,
    ability: &AbilityDefinition,
    draws: DrawPricing,
    origin: Zone,
) -> f64 {
    if !ability.mode_abilities.is_empty() {
        return ability
            .mode_abilities
            .iter()
            .map(|mode| definition_payoff_from(state, obj, mode, draws, origin))
            .fold(0.0, f64::max);
    }
    let effects = priced_effects_from(ability, origin);
    chain_payoff(state, obj, &effects, draws, performers(state, obj, ability))
}

/// The effects of a non-modal ability [`definition_payoff`] prices: those it
/// performs as cast from hand, or for any other branch shape the
/// unconditional links of its chain.
fn priced_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    priced_effects_from(ability, Zone::Hand)
}

/// [`priced_effects`] for a spell cast from `origin`
/// (`self_cost::effects_cast_from`).
fn priced_effects_from(ability: &AbilityDefinition, origin: Zone) -> Vec<&Effect> {
    effects_cast_from(ability, origin).unwrap_or_else(|| {
        let mut effects = Vec::new();
        let mut next = Some(ability);
        while let Some(node) = next {
            if node.condition.is_none() {
                effects.push(&*node.effect);
            }
            next = node.sub_ability.as_deref();
        }
        effects
    })
}

/// Who performs a chain's draws and discards (`TargetFilter::Controller`
/// links).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Performers {
    /// Its controller, once.
    Controller,
    /// CR 608.2c + CR 115.10: "each player" / "each opponent" (the root's
    /// `player_scope`): each of `players` performs the chain in turn, as its
    /// "you". `each_may`: each decides for themself whether to (CR 101.4,
    /// "each player may").
    Each {
        players: Vec<PlayerId>,
        each_may: bool,
    },
}

/// The [`Performers`] of `ability`: every living player for
/// `PlayerFilter::All`, the opponents of its controller for
/// `PlayerFilter::Opponent`; any other scope, or none, its controller.
fn performers(state: &GameState, obj: &GameObject, ability: &AbilityDefinition) -> Performers {
    let players = match ability.player_scope {
        Some(PlayerFilter::All) => state
            .seat_order
            .iter()
            .copied()
            .filter(|&player| players::is_alive(state, player))
            .collect(),
        Some(PlayerFilter::Opponent) => players::opponents(state, obj.controller),
        _ => return Performers::Controller,
    };
    Performers::Each {
        players,
        each_may: ability.optional,
    }
}

/// The payoff of one resolving chain. Removal and tokens add up effect by
/// effect. Cards drawn are netted against cards the same chain makes its
/// controller discard (CR 121.1, CR 701.9a: "draw two cards, then discard a
/// card" nets one) and each net card is one expected draw.
///
/// Performed by each of several players ([`Performers::Each`]), the draws and
/// discards are netted per player ([`player_refill`]): the cards the
/// controller gains are its gain, and the cards each opponent gains its loss,
/// both at the controller's expected draw (an opposing card is priced as our
/// own, as `answer_value` prices one we remove). The net can be below 0.
fn chain_payoff(
    state: &GameState,
    obj: &GameObject,
    effects: &[&Effect],
    draws: DrawPricing,
    performers: Performers,
) -> f64 {
    let quantity =
        |count: &QuantityExpr| resolve_quantity(state, count, obj.controller, obj.id).max(0);
    let mut drawn = 0;
    let mut discarded = 0;
    let mut payoff = 0.0;
    for effect in effects {
        match effect {
            Effect::Draw {
                count,
                target: TargetFilter::Controller,
            } => drawn += quantity(count),
            Effect::Discard {
                count,
                target: TargetFilter::Controller,
                ..
            } => discarded += quantity(count),
            _ => payoff += effect_payoff(state, obj, effect, draws),
        }
    }
    let net = match performers {
        Performers::Controller => (drawn - discarded).max(0),
        Performers::Each { players, each_may } => players
            .into_iter()
            .map(|player| {
                let refill = player_refill(state, obj, effects, player, each_may);
                if player == obj.controller {
                    refill
                } else {
                    -refill
                }
            })
            .sum(),
    };
    if net != 0 && draws == DrawPricing::LibraryMean {
        payoff += f64::from(net) * draw_unit(state, obj.owner);
    }
    payoff
}

/// The cards `player` gains performing a chain's draws and discards as its
/// "you" (CR 608.2c), in chain order, never below 0:
///
/// - its hand is the hand it holds when the chain resolves, so without the
///   card being priced while that card is in `player`'s hand (it is cast, or
///   for a prepare spell its creature is cast, before the chain resolves);
///   a discard takes at most the cards in hand (CR 701.9a), a draw adds to
///   it (CR 121.1);
/// - when each player may (`each_may`, CR 101.4), `player` performs it only
///   when it gains cards and its library holds every card it would draw:
///   drawing from an empty library loses the game (CR 121.4 + CR 704.5b).
fn player_refill(
    state: &GameState,
    obj: &GameObject,
    effects: &[&Effect],
    player: PlayerId,
    each_may: bool,
) -> i32 {
    let Some(seat) = state.players.iter().find(|p| p.id == player) else {
        return 0;
    };
    let mut hand = seat.hand.len() as i32 - i32::from(seat.hand.contains(&obj.id));
    let quantity = |count: &QuantityExpr| resolve_quantity(state, count, player, obj.id).max(0);
    let mut drawn = 0;
    let mut discarded = 0;
    for effect in effects {
        match effect {
            Effect::Draw {
                count,
                target: TargetFilter::Controller,
            } => {
                let cards = quantity(count);
                drawn += cards;
                hand += cards;
            }
            Effect::Discard {
                count,
                target: TargetFilter::Controller,
                ..
            } => {
                let cards = quantity(count).min(hand.max(0));
                discarded += cards;
                hand -= cards;
            }
            _ => {}
        }
    }
    let gained = drawn - discarded;
    let declines = gained <= 0 || seat.library.len() < drawn as usize;
    if each_may && declines {
        0
    } else {
        gained.max(0)
    }
}

/// One effect other than a draw or a discard, on the creature scale:
///
/// - a creature token its controller creates: its body, `1.5 * power +
///   toughness` per token (CR 111.1);
/// - removal: [`answer_value`] over the opposing creatures it would kill,
///   exile or make their controller sacrifice; mass removal its net over the
///   board ([`sweep_value`]); a counterspell the creature it has yet to meet,
///   [`ANSWER_FLOOR`] (CR 701.6a).
///
/// A bounce, a pump, a tap, protection, life gain and everything else is 0:
/// not removal, and priced by its pips alone as before.
///
/// A creature's own abilities (`obj` a creature) are priced on the current
/// board, since they happen when it enters or is used, not when an answer
/// finds a target later: removal is worth what it removes now, nothing when
/// nothing is there (no [`ANSWER_FLOOR`]), and a destroy or exile also
/// reaches an opposing noncreature, nonland permanent; and the shapes only a
/// creature's abilities are priced for ([`creature_effect_payoff`]: a
/// reanimation, life gain, a fight). A noncreature card's effects are priced
/// as before.
fn effect_payoff(state: &GameState, obj: &GameObject, effect: &Effect, draws: DrawPricing) -> f64 {
    let creature_source = obj.card_types.core_types.contains(&CoreType::Creature);
    if creature_source {
        if let Some(payoff) = creature_effect_payoff(state, obj, effect, draws) {
            return payoff;
        }
    }
    let floor = if creature_source { 0.0 } else { ANSWER_FLOOR };
    match effect {
        Effect::Token {
            power: PtValue::Fixed(power),
            toughness: PtValue::Fixed(toughness),
            types,
            count,
            owner: TargetFilter::Controller,
            ..
        } if types.iter().any(|t| t == "Creature") => {
            let tokens = resolve_quantity(state, count, obj.controller, obj.id).max(0);
            f64::from(tokens) * (f64::from(*power) * 1.5 + f64::from(*toughness))
        }
        Effect::Counter { .. } => ANSWER_FLOOR,
        Effect::DestroyAll { target, .. } => {
            sweep_value(state, obj, target, floor, |id| !indestructible(state, id))
        }
        Effect::DamageAll { target, .. } | Effect::PumpAll { target, .. }
            if effect_polarity(effect) == EffectPolarity::Harmful =>
        {
            sweep_value(state, obj, target, floor, |id| {
                lethal_to_creature(state, id, &[effect]) == Some(true)
            })
        }
        _ => {
            let creature_answer = match spot_kill(effect) {
                Some((filter, kills)) => {
                    answer_value(state, obj, Some(effect), filter, floor, |id| {
                        kills.kills(state, id, effect)
                    })
                }
                None => 0.0,
            };
            if creature_source {
                creature_answer.max(noncreature_permanent_answer(state, obj, effect))
            } else {
                creature_answer
            }
        }
    }
}

/// The payoff of an effect only a creature's own abilities are priced for,
/// or `None` to price it as any card's effect ([`effect_payoff`]):
///
/// - CR 404.1 + CR 110.1: a reanimation (`reanimator::reanimation_target`)
///   is the best card it can return from a graveyard now, at its own price
///   ([`nested_value`]); nothing when no legal card is there. The creature
///   itself is not a target of its own ability.
/// - CR 119.3: life its controller gains, at [`LIFE_CARD_SHARE_PER_POINT`]
///   of one expected draw a point.
/// - CR 701.14a: "it fights" (the creature itself fights a target): the
///   best exchange it can take ([`fight_exchange`]).
fn creature_effect_payoff(
    state: &GameState,
    obj: &GameObject,
    effect: &Effect,
    draws: DrawPricing,
) -> Option<f64> {
    if let Some(filter) = reanimation_target(effect) {
        let filter_ctx = FilterContext::from_source(state, obj.id);
        return Some(
            state
                .players
                .iter()
                .flat_map(|player| player.graveyard.iter().copied())
                .filter(|&id| id != obj.id && matches_target_filter(state, id, filter, &filter_ctx))
                .map(|id| nested_value(state, id))
                .fold(0.0, f64::max),
        );
    }
    match effect {
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        } => Some(match draws {
            DrawPricing::LibraryMean => {
                let life = resolve_quantity(state, amount, obj.controller, obj.id).max(0);
                f64::from(life) * LIFE_CARD_SHARE_PER_POINT * draw_unit(state, obj.controller)
            }
            DrawPricing::Unpriced => 0.0,
        }),
        Effect::Fight {
            target,
            subject: TargetFilter::SelfRef,
        } => Some(fight_exchange(state, obj, effect, target)),
        _ => None,
    }
}

/// Life on the card scale: the share of one card a point of life is worth,
/// the existing life weight (`PolicyPenalties::mode_choice_damage_per_point`'s
/// default, the price a modal choice puts on a point of damage). UNTUNED.
pub(crate) const LIFE_CARD_SHARE_PER_POINT: f64 = 0.15;

/// CR 701.14a: each fighter deals damage equal to its power to the other.
/// The best exchange `fighter` can take against an opposing creature
/// `filter` admits: the target's price ([`nested_value`]) if the damage
/// destroys it, less the fighter's own price if the target's damage destroys
/// it. Lethal damage is toughness less damage already marked (CR 704.5g),
/// any damage from a deathtouch source (CR 702.2b), and an indestructible
/// creature survives it (CR 702.12b). Never below 0: a losing fight is not
/// taken (an "up to one" fight takes no target; the 2026-10-20 engine fix
/// made that answer reachable). A fight its controller must take into a
/// loss is read as 0 too.
fn fight_exchange(
    state: &GameState,
    fighter: &GameObject,
    effect: &Effect,
    filter: &TargetFilter,
) -> f64 {
    let destroys = |source: &GameObject, victim: &GameObject| {
        let power = source.power.unwrap_or(0).max(0);
        let left = victim.toughness.unwrap_or(0) - victim.damage_marked as i32;
        power > 0
            && !victim.has_keyword(&Keyword::Indestructible)
            && (power >= left || source.has_keyword(&Keyword::Deathtouch))
    };
    let own_price = nested_value(state, fighter.id);
    opposing_creatures(state, fighter, Some(effect), filter)
        .into_iter()
        .filter_map(|id| state.objects.get(&id))
        .map(|target| {
            let gained = if destroys(fighter, target) {
                nested_value(state, target.id)
            } else {
                0.0
            };
            let lost = if destroys(target, fighter) {
                own_price
            } else {
                0.0
            };
            gained - lost
        })
        .fold(0.0, f64::max)
}

/// CR 701.8a + CR 406.1: what a destroy or an exile from the battlefield
/// removes among the opposing noncreature, nonland permanents its filter
/// admits, at the permanent's price ([`nested_value`]); 0 with none there or
/// for any other effect. A creature target is [`spot_kill`]'s, a land is not
/// removal on this scale.
fn noncreature_permanent_answer(state: &GameState, obj: &GameObject, effect: &Effect) -> f64 {
    let (filter, kill) = match effect {
        Effect::Destroy { target, .. } => (target, SpotKill::Destroy),
        Effect::ChangeZone {
            origin: None | Some(Zone::Battlefield),
            destination: Zone::Exile | Zone::Graveyard | Zone::Library,
            target,
            ..
        } => (target, SpotKill::Removes),
        _ => return 0.0,
    };
    if filter_names_only_own_permanents(filter) {
        return 0.0;
    }
    opposing_permanents(state, obj, Some(effect), filter, |o| {
        !o.card_types.core_types.contains(&CoreType::Creature)
            && !o.card_types.core_types.contains(&CoreType::Land)
    })
    .into_iter()
    .filter(|&id| kill != SpotKill::Destroy || !indestructible(state, id))
    .map(|id| nested_value(state, id))
    .fold(0.0, f64::max)
}

/// How a single-target removal effect ends a creature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpotKill {
    /// CR 701.8a destroy (indestructible survives it, CR 702.12b).
    Destroy,
    /// CR 406.1 exile, CR 701.21a an opponent's sacrifice: no toughness test.
    Removes,
    /// CR 704.5f + CR 704.5g: damage, a shrink, -1/-1 counters; lethal by
    /// `effect_classify::lethal_to_creature`.
    Lethal,
    /// CR 613.4b + CR 704.5f: a base toughness set to a value the creature's
    /// other modifications leave at 0 or less.
    BaseToughness(i32),
}

impl SpotKill {
    fn kills(self, state: &GameState, id: ObjectId, effect: &Effect) -> bool {
        match self {
            Self::Destroy => !indestructible(state, id),
            Self::Removes => true,
            Self::Lethal => lethal_to_creature(state, id, &[effect]) == Some(true),
            Self::BaseToughness(value) => state.objects.get(&id).is_some_and(|object| {
                let base = object
                    .layer_base_toughness
                    .or(object.base_toughness)
                    .unwrap_or(0);
                value + object.toughness.unwrap_or(0) - base <= 0
            }),
        }
    }
}

/// The removal class of a single-target effect and the filter of what it can
/// reach, or `None` for an effect that is not removal: one that cannot end a
/// creature at all (a -5/-0, a bounce, a pump), or whose filter names only
/// its caster's own permanents.
fn spot_kill(effect: &Effect) -> Option<(&TargetFilter, SpotKill)> {
    let (filter, kill) = match effect {
        Effect::Destroy { target, .. } => (target, SpotKill::Destroy),
        Effect::ChangeZone {
            origin: None | Some(Zone::Battlefield),
            destination: Zone::Exile | Zone::Graveyard | Zone::Library,
            target,
            ..
        } => (target, SpotKill::Removes),
        Effect::Sacrifice { target, .. } => (target, SpotKill::Removes),
        Effect::DealDamage { target, .. } => (target, SpotKill::Lethal),
        Effect::Pump {
            toughness, target, ..
        } if effect_polarity(effect) == EffectPolarity::Harmful
            && !matches!(toughness, PtValue::Fixed(t) if *t >= 0) =>
        {
            (target, SpotKill::Lethal)
        }
        Effect::PutCounter {
            counter_type,
            target,
            ..
        } if counter_type
            .power_toughness_delta()
            .is_some_and(|(_, toughness)| toughness < 0) =>
        {
            (target, SpotKill::Lethal)
        }
        Effect::GenericEffect {
            static_abilities,
            target: Some(target),
            ..
        } => {
            let value = static_abilities
                .iter()
                .filter(|definition| {
                    matches!(definition.mode, StaticMode::Continuous)
                        && matches!(definition.affected, Some(TargetFilter::ParentTarget))
                })
                .flat_map(|definition| &definition.modifications)
                .find_map(|modification| match modification {
                    ContinuousModification::SetToughness { value } => Some(*value),
                    _ => None,
                })?;
            (target, SpotKill::BaseToughness(value))
        }
        _ => return None,
    };
    (filter_admits_creature(filter) && !filter_names_only_own_permanents(filter))
        .then_some((filter, kill))
}

/// What removal is worth when no opposing creature it answers is on the
/// battlefield: the creature the opponent has yet to play, priced as a
/// vanilla 2/2 for two on the creature scale (`1.5 * 2 + 2 + 0.5 * 2`). Above
/// a basic land's 3.0, so a removal spell is never the first card a keep
/// order gives up while the opponent still has creatures to cast.
pub(crate) const ANSWER_FLOOR: f64 = 6.0;

/// Removal's value: the most valuable opposing creature on the battlefield
/// that `filter` admits and `kills` ends, at its body ([`nested_value`]),
/// never less than `floor` ([`ANSWER_FLOOR`] for a card held to answer a
/// creature later, 0 for a creature's own ability, which acts on the board
/// it enters).
fn answer_value(
    state: &GameState,
    obj: &GameObject,
    effect: Option<&Effect>,
    filter: &TargetFilter,
    floor: f64,
    kills: impl Fn(ObjectId) -> bool,
) -> f64 {
    opposing_creatures(state, obj, effect, filter)
        .into_iter()
        .filter(|&id| kills(id))
        .map(|id| nested_value(state, id))
        .fold(floor, f64::max)
}

/// Mass removal's value: the opposing creatures it ends less the caster's
/// own, each at its body ([`nested_value`]), never less than `floor` (as
/// [`answer_value`]'s).
fn sweep_value(
    state: &GameState,
    obj: &GameObject,
    filter: &TargetFilter,
    floor: f64,
    kills: impl Fn(ObjectId) -> bool,
) -> f64 {
    let filter_ctx = FilterContext::from_source(state, obj.id);
    let net: f64 = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Creature))
                && matches_target_filter(state, id, filter, &filter_ctx)
                && kills(id)
        })
        .map(|id| {
            let value = nested_value(state, id);
            if state.objects[&id].controller == obj.controller {
                -value
            } else {
                value
            }
        })
        .sum();
    net.max(floor)
}

/// The opposing creatures on the battlefield that `filter` admits, read from
/// the card's own view. Each opponent is read as the player the card would
/// target (CR 115.1) when `effect` is given, so a filter naming "target
/// opponent" ("target opponent sacrifices a creature with the greatest mana
/// value among creatures they control") binds to that opponent's creatures.
/// An Aura's enchant filter names no player and is read from the card alone.
fn opposing_creatures(
    state: &GameState,
    obj: &GameObject,
    effect: Option<&Effect>,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    opposing_permanents(state, obj, effect, filter, |o| {
        o.card_types.core_types.contains(&CoreType::Creature)
    })
}

/// [`opposing_creatures`] for any opposing permanent `kind` admits.
fn opposing_permanents(
    state: &GameState,
    obj: &GameObject,
    effect: Option<&Effect>,
    filter: &TargetFilter,
    kind: impl Fn(&GameObject) -> bool,
) -> Vec<ObjectId> {
    let mut found = Vec::new();
    for opponent in players::opponents(state, obj.controller) {
        let bound = effect.map(|effect| {
            ResolvedAbility::new(
                effect.clone(),
                vec![TargetRef::Player(opponent)],
                obj.id,
                obj.controller,
            )
        });
        let filter_ctx = match &bound {
            Some(bound) => FilterContext::from_ability(bound),
            None => FilterContext::from_source(state, obj.id),
        };
        found.extend(state.battlefield.iter().copied().filter(|&id| {
            state
                .objects
                .get(&id)
                .is_some_and(|o| o.controller == opponent && kind(o))
                && matches_target_filter(state, id, filter, &filter_ctx)
        }));
    }
    found
}

fn indestructible(state: &GameState, id: ObjectId) -> bool {
    state
        .objects
        .get(&id)
        .is_some_and(|o| o.has_keyword(&Keyword::Indestructible))
}

/// CR 121.1: one card drawn from `player`'s library, on the
/// [`intrinsic_value`] scale: the library's mean with drawn cards unpriced
/// ([`DrawPricing::Unpriced`]), floored at [`EXPECTED_DRAW_FLOOR`]. The
/// library's order is hidden, so a draw is an expected card, as
/// [`expected_draw_value`] reads it. Library creatures are their bodies
/// ([`Depth::Nested`]): the unit every draw is priced in does not move with
/// creature abilities, and a creature's life-gain or draw term never asks for
/// the mean it sits in.
fn draw_unit(state: &GameState, player: PlayerId) -> f64 {
    let Some(library) = state.players.get(player.0 as usize).map(|p| &p.library) else {
        return EXPECTED_DRAW_FLOOR;
    };
    if library.is_empty() {
        return EXPECTED_DRAW_FLOOR;
    }
    let total: f64 = library
        .iter()
        .map(|&id| value_with(state, id, DrawPricing::Unpriced, Depth::Nested))
        .sum();
    (total / library.len() as f64).max(EXPECTED_DRAW_FLOOR)
}

/// Cost-side card valuation for every `PayCostKind` that spends a card from a
/// non-battlefield zone **except `Discard`**, which is priced in keep order by
/// [`keep_prices`]. Diverges from [`intrinsic_value`] on two axes, both
/// deliberate and both currently load-bearing for shipped behaviour:
///
/// 1. **Mana-cost proxy.** This function uses `ManaCost::mana_value()`, which
///    follows CR 202.3e ({X} = 0 off the stack) and CR 202.3f (hybrid uses the
///    largest component). `intrinsic_value` instead counts pips
///    (`shards.len() + generic`). Example: `{2/B}{2/B}{2/B}` prices at 3.0 here
///    and 1.5 there; `{X}{R}` prices at 0.5 here and 1.0 there.
/// 2. **Power/toughness reads.** This function falls back to `base_power` and
///    clamps at zero; `intrinsic_value` does neither. That matters because this
///    function is also called on graveyard and battlefield objects, where
///    `power` can be `None` or negative.
///
/// **OPEN DECISION — do not "fix" this without reading it.** Which proxy is the
/// better AI *heuristic* is unresolved. Neither is a rules violation: this is
/// `phase-ai`, and neither function claims to compute an object's mana value —
/// `intrinsic_value`'s own comment calls its mana term a "proxy for power", and
/// counting `{2/B}` as one pip is arguably the better proxy for a card you will
/// cast for `{B}`. Converging the two is a **declared behaviour change** across
/// 11 production `intrinsic_value` call sites (10 in `search.rs`, 1 here in
/// [`keep_key`]; 3 further hits in `search.rs` are test assertions) and 6
/// `PayCostKind` arms spanning 12 `cost_card_value` call sites, with a named
/// regression:
/// the `DigChoice` `up_to` path tests `value < 0.1`, and under `mana_value()` a
/// bare `{X}` spell scores 0.0 and would select nothing where it currently
/// selects the card. Any convergence needs its own regression tests and a
/// `scripts/ai-gate.sh` run with the paired-seed report attached.
pub(crate) fn cost_card_value(state: &GameState, obj_id: ObjectId) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };

    let mut value = 0.0;
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        let power = obj.power.unwrap_or(obj.base_power.unwrap_or(0)).max(0) as f64;
        let toughness = obj
            .toughness
            .unwrap_or(obj.base_toughness.unwrap_or(0))
            .max(0) as f64;
        value += power * 1.5 + toughness;
    }
    if obj.card_types.core_types.contains(&CoreType::Land) {
        value += 3.0;
    }
    value + obj.mana_cost.mana_value() as f64 * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    };
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaColor;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const P0: PlayerId = PlayerId(0);

    fn card_in_hand(state: &mut GameState, name: &str) -> ObjectId {
        create_object(
            state,
            CardId(state.next_object_id),
            P0,
            name.to_string(),
            Zone::Hand,
        )
    }

    fn land_card(state: &mut GameState) -> ObjectId {
        let id = card_in_hand(state, "Swamp");
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    /// An MV-3 artifact with an untargeted activated `Effect::Mana` ability —
    /// the Commander's Sphere shape.
    fn rock_card(state: &mut GameState) -> ObjectId {
        let id = card_in_hand(state, "Rock");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Black],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn plain_card(state: &mut GameState) -> ObjectId {
        card_in_hand(state, "Bolt")
    }

    fn behind(lands: i8, mana: i8) -> Option<PlanState> {
        Some(PlanState {
            lands_behind: lands,
            mana_behind: mana,
            ..PlanState::default()
        })
    }

    /// The tier a single deficit value implies, as the truth table reads it.
    fn tier_of(deficit: i8) -> KeepTier {
        match deficit.cmp(&0) {
            Ordering::Greater => KeepTier::NeededManaSource,
            Ordering::Less => KeepTier::Surplus,
            Ordering::Equal => KeepTier::Ordinary,
        }
    }

    /// F10 — the full `ManaRole` x plan cross product, asserting the truth
    /// table exactly. The plan axis is now two-dimensional, so this is
    /// 3 roles x (3 land signs x 3 mana signs + the `None` plan) = 30 cases,
    /// no wildcards.
    ///
    /// The off-diagonal cells are the point: a land reads `lands_behind` and an
    /// accelerant reads `mana_behind`, so the six cells where the two axes
    /// disagree assert *different* tiers for the land and the rock in the same
    /// plan. Before this round both read `lands_behind` and every off-diagonal
    /// cell was wrong.
    #[test]
    fn keep_tier_truth_table_is_exhaustive() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let rock = rock_card(&mut state);
        let plain = plain_card(&mut state);

        assert_eq!(mana_role(&state, land), ManaRole::LandDrop);
        assert_eq!(mana_role(&state, rock), ManaRole::Accelerant);
        assert_eq!(mana_role(&state, plain), ManaRole::None);

        for lands_behind in [4_i8, 0, -6] {
            for mana_behind in [3_i8, 0, -2] {
                let plan = behind(lands_behind, mana_behind);
                assert_eq!(
                    keep_tier(&state, land, plan),
                    tier_of(lands_behind),
                    "a land must read lands_behind ({lands_behind}), not \
                     mana_behind ({mana_behind})"
                );
                // The rock is a pure rock (nothing beyond its mana ability),
                // so past the mana target it is Surplus like a land past its
                // target (judge P1 on c3a3a59a6); a rock or creature with
                // other value is Ordinary there (the mana-creature row).
                assert_eq!(
                    keep_tier(&state, rock, plan),
                    tier_of(mana_behind),
                    "an accelerant must read mana_behind ({mana_behind}), not \
                     lands_behind ({lands_behind})"
                );
                assert_eq!(
                    keep_tier(&state, plain, plan),
                    KeepTier::Ordinary,
                    "a non-source is Ordinary on every plan"
                );
            }
        }

        // No plan authority at all: every card is Ordinary, whatever its role.
        for id in [land, rock, plain] {
            assert_eq!(keep_tier(&state, id, None), KeepTier::Ordinary);
        }
    }

    /// CR 305.2: only a land is played as a land, one a turn, so only a land
    /// past the plan's land target is `Surplus`. A mana creature (a 0/1 with
    /// "{T}: Add {B}") reads the mana schedule: below the mana target it is
    /// `NeededManaSource`; past it, its mana role adds nothing and it keeps
    /// the tier its body earns, `Ordinary`, with its intrinsic value as the
    /// scalar. A land past its target is still `Surplus` on the same plan.
    #[test]
    fn a_mana_creature_is_needed_below_the_mana_target_and_ordinary_past_it() {
        let mut state = GameState::new_two_player(7);
        let dork = rock_card(&mut state);
        {
            let obj = state.objects.get_mut(&dork).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.power = Some(0);
            obj.toughness = Some(1);
        }
        let land = land_card(&mut state);
        assert_eq!(mana_role(&state, dork), ManaRole::Accelerant);
        assert_eq!(
            keep_tier(&state, dork, behind(-3, 2)),
            KeepTier::NeededManaSource
        );
        assert_eq!(keep_tier(&state, dork, behind(-3, -3)), KeepTier::Ordinary);
        assert_eq!(keep_tier(&state, land, behind(-3, -3)), KeepTier::Surplus);
    }

    #[test]
    fn one_shot_mana_source_is_not_an_accelerant() {
        let mut state = GameState::new_two_player(7);
        let one_shot = rock_card(&mut state);
        let ability =
            &mut Arc::make_mut(&mut state.objects.get_mut(&one_shot).unwrap().abilities)[0];
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(engine::types::ability::SacrificeCost::count(
                    engine::types::ability::TargetFilter::SelfRef,
                    1,
                )),
            ],
        });

        assert_eq!(mana_role(&state, one_shot), ManaRole::None);
        assert_eq!(
            keep_tier(&state, one_shot, behind(0, 3)),
            KeepTier::Ordinary
        );
    }

    /// The reported failure, at the tier level: the four-land deficit that a
    /// rock can never close must not promote the rock.
    ///
    /// Turn-10 Commander, two lands plus Sol Ring / Arcane Signet / Talisman.
    /// Land schedule 6 - 2 lands = **+4 behind**; mana schedule 6 - 5 sources =
    /// **+1 behind**, and once a fifth source lands it is 0. Under the old
    /// single-axis rule the rock read `NeededManaSource` off the +4 and the
    /// cleanup pitched a castable spell to keep a redundant source.
    #[test]
    fn an_accelerant_is_not_promoted_by_a_deficit_only_lands_can_close() {
        let mut state = GameState::new_two_player(7);
        let rock = rock_card(&mut state);
        let land = land_card(&mut state);

        // Four lands behind, but the manabase is complete.
        let plan = behind(4, 0);
        assert_eq!(
            keep_tier(&state, rock, plan),
            KeepTier::Ordinary,
            "a spare rock is Ordinary once the mana schedule is met, however \
             many LANDS the player is short"
        );
        assert_eq!(
            keep_tier(&state, land, plan),
            KeepTier::NeededManaSource,
            "positive reach-guard: the same plan still promotes a LAND, so the \
             fixture is not simply plan-blind"
        );
    }

    /// `Ord` on `KeepTier` IS the give-up specification: declaration order is
    /// surrender order, so a reordering of the variants is a test failure.
    #[test]
    fn keep_tier_order_is_surrender_order() {
        assert!(KeepTier::Surplus < KeepTier::Ordinary);
        assert!(KeepTier::Ordinary < KeepTier::NeededManaSource);
    }

    /// The fail-safe property, proved rather than asserted: when every card is
    /// `Ordinary` the tuple comparator agrees with the bare scalar comparator.
    #[test]
    fn cmp_keep_degenerates_to_scalar_when_all_ordinary() {
        for a in [0.0_f64, 0.5, 3.0, 15.5] {
            for b in [0.0_f64, 0.5, 3.0, 15.5] {
                assert_eq!(
                    cmp_keep(&(KeepTier::Ordinary, a), &(KeepTier::Ordinary, b)),
                    a.partial_cmp(&b).unwrap(),
                    "({a}, {b}) must order identically to the scalar comparator"
                );
            }
        }
    }

    /// Tier dominates the scalar: a needed mana source is kept even though its
    /// intrinsic value is far below an `Ordinary` card's.
    #[test]
    fn cmp_keep_tier_dominates_scalar() {
        assert_eq!(
            cmp_keep(
                &(KeepTier::NeededManaSource, 1.5),
                &(KeepTier::Ordinary, 15.5)
            ),
            Ordering::Greater
        );
        assert_eq!(
            cmp_keep(&(KeepTier::Surplus, 3.0), &(KeepTier::Ordinary, 0.5)),
            Ordering::Less
        );
    }

    fn spell_card(state: &mut GameState, name: &str, generic: u32) -> ObjectId {
        let id = card_in_hand(state, name);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.mana_cost = engine::types::mana::ManaCost::generic(generic);
        id
    }

    fn price_of(prices: &[(ObjectId, f64)], id: ObjectId) -> f64 {
        prices.iter().find(|(card, _)| *card == id).unwrap().1
    }

    fn discard(cards: &[ObjectId]) -> GameAction {
        GameAction::SelectCards {
            cards: cards.to_vec(),
        }
    }

    /// KT1, the band's edge: a land past the plan's target (Surplus) and a
    /// spell. The selector picked the spell; a land within
    /// `DISCARD_TIE_BAND` of it is played instead, one just outside it is
    /// not (the search's pick stands).
    #[test]
    fn keep_order_tie_break_reads_the_band() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let spell = spell_card(&mut state, "Spell", 2);
        let plan = behind(-1, 0);
        assert_eq!(keep_tier(&state, land, plan), KeepTier::Surplus);
        for (gap, expected) in [
            (0.0, land),
            (DISCARD_TIE_BAND * 0.9, land),
            (DISCARD_TIE_BAND * 1.1, spell),
            (0.5, spell),
        ] {
            let scored = vec![(discard(&[spell]), 10.0), (discard(&[land]), 10.0 - gap)];
            let index = keep_order_tie_break(&state, plan, &scored, 0);
            assert_eq!(
                scored[index].0,
                discard(&[expected]),
                "gap {gap}: expected {expected:?}"
            );
        }
    }

    /// KT2: the chosen candidate is kept when nothing in the band is strictly
    /// earlier in keep order (two lands with equal keys), when the other
    /// candidate discards a different number of cards (an "up to" discard's
    /// empty answer), and when the chosen action is not a selection.
    #[test]
    fn keep_order_tie_break_keeps_the_pick_without_a_strictly_earlier_peer() {
        let mut state = GameState::new_two_player(7);
        let land_a = land_card(&mut state);
        let land_b = land_card(&mut state);
        let spell = spell_card(&mut state, "Spell", 2);
        let plan = behind(-1, 0);

        let equal_keys = vec![(discard(&[land_a]), 4.0), (discard(&[land_b]), 4.0)];
        assert_eq!(keep_order_tie_break(&state, plan, &equal_keys, 1), 1);
        assert_eq!(keep_order_tie_break(&state, plan, &equal_keys, 0), 0);

        let sizes = vec![(discard(&[]), 4.0), (discard(&[spell]), 4.0)];
        assert_eq!(keep_order_tie_break(&state, plan, &sizes, 0), 0);
        let sizes = vec![(discard(&[spell]), 4.0), (discard(&[land_a, land_b]), 4.0)];
        assert_eq!(keep_order_tie_break(&state, plan, &sizes, 0), 0);

        let not_a_selection = vec![(GameAction::PassPriority, 4.0), (discard(&[land_a]), 4.0)];
        assert_eq!(keep_order_tie_break(&state, plan, &not_a_selection, 0), 0);
    }

    /// KT3: a discard of two compares the selections card by card in keep
    /// order, as the cleanup arm's rank vector does: {surplus land, spell}
    /// goes before {spell, spell}, and a needed land (behind the plan) is the
    /// last card given up, so {spell, spell} goes before {needed land, spell}.
    #[test]
    fn keep_order_tie_break_orders_multi_card_discards_card_by_card() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let one = spell_card(&mut state, "One", 2);
        let two = spell_card(&mut state, "Two", 2);
        let scored = vec![(discard(&[one, two]), 1.0), (discard(&[land, two]), 1.0)];

        let flooded = behind(-1, 0);
        assert_eq!(keep_order_tie_break(&state, flooded, &scored, 0), 1);

        let short = behind(2, 0);
        assert_eq!(keep_tier(&state, land, short), KeepTier::NeededManaSource);
        assert_eq!(keep_order_tie_break(&state, short, &scored, 1), 0);
    }

    /// Order: `keep_prices` ranks a mixed pool exactly as `cmp_keep` sorts it,
    /// on every plan sign, and the scale property holds (the prices are the
    /// pool's own values rearranged, so the minimum is the pool's minimum).
    #[test]
    fn keep_prices_rank_a_pool_exactly_as_cmp_keep_sorts_it() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let rock = rock_card(&mut state);
        let one = spell_card(&mut state, "One", 2);
        let five = spell_card(&mut state, "Five", 10);
        let pool = [land, rock, one, five];

        for (lands_behind, mana_behind) in [(-2_i8, -2_i8), (0, 0), (2, 2), (-1, 3), (3, -1)] {
            let plan = behind(lands_behind, mana_behind);
            let prices = keep_prices(&state, &pool, plan);
            for &a in &pool {
                for &b in &pool {
                    let by_key = cmp_keep(&keep_key(&state, a, plan), &keep_key(&state, b, plan));
                    let by_price = price_of(&prices, a)
                        .partial_cmp(&price_of(&prices, b))
                        .unwrap();
                    assert_eq!(
                        by_key, by_price,
                        "plan ({lands_behind}, {mana_behind}): price order must be keep order"
                    );
                }
            }
            let lowest = pool
                .iter()
                .map(|&id| intrinsic_value(&state, id))
                .fold(f64::INFINITY, f64::min);
            let cheapest = prices.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
            assert_eq!(
                cheapest, lowest,
                "the cheapest give-up costs the pool minimum"
            );
        }
    }

    /// Fail-safe: with no plan authority every card is `Ordinary`, and each
    /// card's price is its own `intrinsic_value`, ties included.
    #[test]
    fn keep_prices_are_intrinsic_values_when_every_card_is_ordinary() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let second_land = land_card(&mut state);
        let six = spell_card(&mut state, "Six", 6);
        let one = spell_card(&mut state, "One", 2);
        let pool = [land, second_land, six, one];
        let prices = keep_prices(&state, &pool, None);
        for &id in &pool {
            assert_eq!(price_of(&prices, id), intrinsic_value(&state, id));
        }
    }

    /// A tie in value across a tier boundary still orders: the surplus land
    /// (3.0) and a six-mana spell (3.0) are a step apart.
    #[test]
    fn keep_prices_order_a_value_tie_across_tiers() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let six = spell_card(&mut state, "Six", 6);
        let prices = keep_prices(&state, &[land, six], behind(-1, 0));
        assert_eq!(price_of(&prices, land), 3.0);
        assert_eq!(price_of(&prices, six), 3.0 + KEEP_PRICE_STEP);
    }

    /// A basic land of `subtype` (so `mana_colors::land_produced_color_types`
    /// reads its colour) in `zone`, owned by P0.
    fn basic_land(state: &mut GameState, subtype: &str, zone: Zone) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            P0,
            subtype.to_string(),
            zone,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push(subtype.to_string());
        id
    }

    /// A `power`/`power` creature in hand costing `{generic}` plus `shards`.
    fn creature_card(
        state: &mut GameState,
        power: i32,
        shards: Vec<engine::types::mana::ManaCostShard>,
        generic: u32,
    ) -> ObjectId {
        let id = card_in_hand(state, "Creature");
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(power);
        obj.mana_cost = engine::types::mana::ManaCost::Cost { shards, generic };
        id
    }

    fn surrender_order(
        state: &GameState,
        pool: &[ObjectId],
        plan: Option<PlanState>,
    ) -> Vec<ObjectId> {
        let mut order = pool.to_vec();
        order.sort_by(|a, b| cmp_keep(&keep_key(state, *a, plan), &keep_key(state, *b, plan)));
        order
    }

    /// CR 305.2: a player normally plays one land a turn, so below the land
    /// target only the land that makes the next land drop is needed. A second
    /// (or third) land in hand is an `Ordinary` card priced by its value, so it
    /// is given up before a spell worth more (the 3/3, 9.0 against 3.0) and the
    /// needed land is given up last. Holds however far behind the board is: the
    /// deficit says lands are wanted, the land drop says one at a time.
    #[test]
    fn below_the_land_target_only_the_next_land_drop_is_needed() {
        for (lands_behind, lands_in_hand) in [(1_i8, 2_usize), (2, 2), (4, 3)] {
            let mut state = GameState::new_two_player(7);
            let lands: Vec<_> = (0..lands_in_hand).map(|_| land_card(&mut state)).collect();
            let three_three = creature_card(
                &mut state,
                3,
                vec![engine::types::mana::ManaCostShard::Black],
                2,
            );
            let plan = behind(lands_behind, 0);

            let needed: Vec<_> = lands
                .iter()
                .copied()
                .filter(|&land| keep_tier(&state, land, plan) == KeepTier::NeededManaSource)
                .collect();
            assert_eq!(
                needed.len(),
                1,
                "{lands_behind} behind with {lands_in_hand} lands in hand: exactly one land \
                 (the next drop) is needed"
            );
            for &land in lands.iter().filter(|land| !needed.contains(land)) {
                assert_eq!(
                    keep_tier(&state, land, plan),
                    KeepTier::Ordinary,
                    "a land past the next drop is an ordinary card, not surplus"
                );
            }

            let mut pool = lands.clone();
            pool.push(three_three);
            let order = surrender_order(&state, &pool, plan);
            assert_eq!(
                order.last(),
                Some(&needed[0]),
                "the needed land is surrendered last"
            );
            assert_eq!(
                order[order.len() - 2],
                three_three,
                "the 3/3 (9.0) is kept over every land past the next drop (3.0)"
            );
            let prices = keep_prices(&state, &pool, plan);
            for &spare in lands.iter().filter(|land| !needed.contains(land)) {
                assert!(
                    price_of(&prices, spare) < price_of(&prices, three_three),
                    "the discard cost gives up the spare land before the 3/3"
                );
            }
        }
    }

    /// The lone land below the target is still the next land drop and is kept
    /// over any spell, however valuable (a 5/5, 15.5 against 3.0).
    #[test]
    fn below_the_land_target_a_lone_land_is_still_needed() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let fatty = creature_card(
            &mut state,
            5,
            vec![engine::types::mana::ManaCostShard::Black],
            5,
        );
        let plan = behind(2, 0);
        assert_eq!(keep_tier(&state, land, plan), KeepTier::NeededManaSource);
        assert_eq!(
            surrender_order(&state, &[land, fatty], plan),
            vec![fatty, land]
        );
        let prices = keep_prices(&state, &[land, fatty], plan);
        assert!(price_of(&prices, fatty) < price_of(&prices, land));
    }

    /// The needed land is the one whose colour the hand's spells are missing
    /// (the judge's row R3): five Islands in play, a Forest and an Island in
    /// hand, a {2}{G} 3/3. The Forest is the next drop; the Island is an
    /// ordinary card. Both creation orders, so no id tie-break can pass it.
    #[test]
    fn the_needed_land_is_the_one_whose_colour_the_hand_needs() {
        for forest_first in [false, true] {
            let mut state = GameState::new_two_player(7);
            for _ in 0..5 {
                basic_land(&mut state, "Island", Zone::Battlefield);
            }
            let (forest, island) = if forest_first {
                let forest = basic_land(&mut state, "Forest", Zone::Hand);
                (forest, basic_land(&mut state, "Island", Zone::Hand))
            } else {
                let island = basic_land(&mut state, "Island", Zone::Hand);
                (basic_land(&mut state, "Forest", Zone::Hand), island)
            };
            let green = creature_card(
                &mut state,
                3,
                vec![engine::types::mana::ManaCostShard::Green],
                2,
            );
            let plan = behind(1, 0);

            assert_eq!(
                keep_tier(&state, forest, plan),
                KeepTier::NeededManaSource,
                "the Forest makes the green 3/3 castable: it is the next drop"
            );
            assert_eq!(
                keep_tier(&state, island, plan),
                KeepTier::Ordinary,
                "a sixth Island adds no colour the hand needs"
            );
            assert_eq!(
                surrender_order(&state, &[forest, island, green], plan),
                vec![island, green, forest]
            );
        }
    }

    /// Scope guard: the next-drop rule reads the hand. A land on the
    /// battlefield (the sacrifice-cost gate's object) is still needed while the
    /// board is behind, whatever the hand holds.
    #[test]
    fn a_battlefield_land_is_needed_below_the_target_whatever_the_hand_holds() {
        let mut state = GameState::new_two_player(7);
        let in_play = basic_land(&mut state, "Swamp", Zone::Battlefield);
        land_card(&mut state);
        land_card(&mut state);
        assert_eq!(
            keep_tier(&state, in_play, behind(2, 0)),
            KeepTier::NeededManaSource
        );
    }

    /// A missing object is not a mana source and never sorts ahead of a real card.
    #[test]
    fn missing_object_is_not_a_mana_source() {
        let state = GameState::new_two_player(7);
        assert_eq!(mana_role(&state, ObjectId(9999)), ManaRole::None);
        assert_eq!(
            keep_tier(&state, ObjectId(9999), behind(3, 3)),
            KeepTier::Ordinary
        );
    }
    // --- given_up_value / expected_draw_value -----------------------------

    #[test]
    fn given_up_value_is_the_keep_orders_first_cards_at_their_own_value() {
        let mut state = GameState::new_two_player(7);
        let land = land_card(&mut state);
        let three = spell_card(&mut state, "Three", 3);
        let creature = creature_card(&mut state, 3, Vec::new(), 3);
        let pool = [creature, land, three];
        // No plan: Ordinary throughout, value order {3} spell 1.5, land 3.0,
        // 3/3 9.0.
        assert_eq!(given_up_value(&state, &pool, 1, None), 1.5);
        assert_eq!(given_up_value(&state, &pool, 2, None), 4.5);
        // Lands behind: the lone land is the next drop (NeededManaSource) and
        // is kept, so two cards give up the spell and the 3/3 at 1.5 + 9.0,
        // not at the pool's two lowest values.
        assert_eq!(given_up_value(&state, &pool, 2, behind(2, 0)), 10.5);
        // Past the land target the land is Surplus: it goes first, and costs
        // nothing (`worth_drawing` calls it not worth drawing), not 3.0.
        assert_eq!(given_up_value(&state, &pool, 1, behind(-2, 0)), 0.0);
        assert_eq!(given_up_value(&state, &pool, 2, behind(-2, 0)), 1.5);
        let order = surrender_order(&state, &pool, behind(-2, 0));
        assert_eq!(order[0], land, "the surplus land is the card given up");
    }

    #[test]
    fn expected_draw_value_is_the_library_mean_floored_and_none_when_empty() {
        let mut state = GameState::new_two_player(7);
        assert_eq!(expected_draw_value(&state, P0, None), None, "empty library");
        let blank = card_in_hand(&mut state, "Blank");
        move_to_library(&mut state, blank);
        assert_eq!(
            expected_draw_value(&state, P0, None),
            Some(EXPECTED_DRAW_FLOOR),
            "a worthless library is floored"
        );
        let land = land_card(&mut state);
        move_to_library(&mut state, land);
        let creature = creature_card(&mut state, 3, Vec::new(), 3);
        move_to_library(&mut state, creature);
        assert_eq!(
            expected_draw_value(&state, P0, None),
            Some((0.0 + 3.0 + 9.0) / 3.0)
        );
        // Past the land target the library land is Surplus and worth 0.
        assert_eq!(
            expected_draw_value(&state, P0, behind(-2, 0)),
            Some((0.0 + 0.0 + 9.0) / 3.0)
        );
    }

    fn move_to_library(state: &mut GameState, id: ObjectId) {
        engine::game::zones::move_to_zone(state, id, Zone::Library, &mut Vec::new());
    }
}
