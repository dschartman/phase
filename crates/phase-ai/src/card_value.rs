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
//! investigation-discard-signal.md`). It has one further consumer, which is a
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
//! deterministic arm at all; they fall through to the policy/eval scorer, so
//! the tier does NOT apply to them. That division is deliberate: the eval path
//! carries its own mana-source signal, and widening the tier to every discard
//! family without evidence would be speculative. "The discard bug is fixed"
//! means the cleanup family and the discard cost, not all discard.

use std::cmp::Ordering;

use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;

use crate::plan::PlanState;
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
    /// A mana source held while the plan says the player is already flooded.
    Surplus,
    /// Everything else. Also the tier for every card when no plan authority
    /// exists, and for every mana source while exactly on curve. When all cards
    /// land here, the sort key `(Ordinary, intrinsic)` orders identically to
    /// `intrinsic` alone.
    Ordinary,
    /// A mana source held while behind the plan's land schedule.
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
/// schedule for an accelerant (see [`ManaRole`]) — and demoted only while that
/// axis says they are past it. On curve — and whenever no plan authority exists
/// — every card is `Ordinary`, which reproduces the pure-intrinsic ordering
/// exactly.
pub(crate) fn keep_tier(state: &GameState, obj_id: ObjectId, plan: Option<PlanState>) -> KeepTier {
    let deficit = match mana_role(state, obj_id) {
        ManaRole::None => return KeepTier::Ordinary,
        ManaRole::LandDrop => plan.map(|p| p.lands_behind),
        ManaRole::Accelerant => plan.map(|p| p.mana_behind),
    };
    match deficit.map(|d| d.cmp(&0)) {
        Some(Ordering::Greater) => KeepTier::NeededManaSource,
        Some(Ordering::Less) => KeepTier::Surplus,
        Some(Ordering::Equal) | None => KeepTier::Ordinary,
    }
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
///   excess the same way.
///
/// `plan == None` (no plan authority, as on the rollout path) leaves every card
/// `Ordinary`, so the scalar alone decides.
pub(crate) fn worth_drawing(state: &GameState, obj_id: ObjectId, plan: Option<PlanState>) -> bool {
    keep_tier(state, obj_id, plan) != KeepTier::Surplus
        && intrinsic_value(state, obj_id) >= KEEP_VALUE_FLOOR
}

/// Evaluate a card's value for scry/dig/surveil decisions.
/// Higher values mean the card is more desirable to keep/draw.
pub(crate) fn intrinsic_value(state: &GameState, obj_id: ObjectId) -> f64 {
    let obj = match state.objects.get(&obj_id) {
        Some(o) => o,
        None => return 0.0,
    };

    let mut value = 0.0;

    // Creatures: value based on power + toughness
    if obj.card_types.core_types.contains(&CoreType::Creature) {
        let power = obj.power.unwrap_or(0) as f64;
        let toughness = obj.toughness.unwrap_or(0) as f64;
        value += power * 1.5 + toughness;
    }

    // Lands: moderate value (mana development)
    if obj.card_types.core_types.contains(&CoreType::Land) {
        value += 3.0;
    }

    // Instants/Sorceries: base value from mana cost (proxy for power)
    if let engine::types::mana::ManaCost::Cost { shards, generic } = &obj.mana_cost {
        let total_mana = shards.len() as f64 + *generic as f64;
        value += total_mana * 0.5;
    }

    value
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
}
