//! Shared mana-color extraction: which colors a land can produce.
//!
//! One building block used by both draft fixing-land evaluation
//! (`draft_eval::produced_color_count`) and the mulligan land-count keepables
//! (`policies::mulligan::keepables_by_land_count`). Operates on *parts*
//! (`subtypes` + `abilities`) so a `GameObject` view and a `CardFace` view share
//! a single implementation, mirroring the `*_parts` pattern in `features`.
//!
//! [`UnmetColors`] is the colour gap between a hand and a battlefield, shared
//! by land-play sequencing and the keep order's choice of the next land drop.

use engine::ai_support::CandidateAction;
use engine::game::mana_payment::{
    land_subtype_to_mana_type, mana_type_to_demand_index, outer_cost_color_demand, ColorDemand,
};
use engine::game::mana_sources::mana_color_to_type;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CostCategory, Effect, ManaProduction,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType};
use engine::types::player::PlayerId;

/// Distinct colored-mana types a land can produce, unioning (a) intrinsic mana
/// from its basic land subtypes (a typed dual like "Land — Plains Island" makes
/// W and U with no printed `Effect::Mana`) and (b) the colors of every activated
/// `Effect::Mana` ability (painlands, filter lands, etc.). Colorless never counts
/// as a color, so the length is the count of *colored* sources — `>= 2` marks a
/// fixing land.
pub fn land_produced_color_types(
    subtypes: &[String],
    abilities: &[AbilityDefinition],
) -> Vec<ManaType> {
    let mut colors = Vec::new();
    for subtype in subtypes {
        if let Some(mana_type) = land_subtype_to_mana_type(subtype) {
            push_color(&mut colors, mana_type);
        }
    }
    for ability in abilities {
        if ability.kind != AbilityKind::Activated {
            continue;
        }
        let Effect::Mana { produced, .. } = &*ability.effect else {
            continue;
        };
        collect_mana_production_colors(&mut colors, produced);
    }
    colors
}

/// The colour gap between `player`'s near-term hand and their battlefield
/// lands: per colour, how many sources one castable hand card needs
/// ([`hand_color_demand`]) against how many of their lands produce it.
///
/// One measure, two readers: `policies::land_sequencing` credits a `PlayLand`
/// candidate with the colours it covers, and `card_value::keep_tier` uses the
/// same count to pick which land in hand is the next land drop, so the land
/// the keep order protects is the land the AI would play.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UnmetColors {
    supply: ColorDemand,
    demand: ColorDemand,
}

impl UnmetColors {
    pub(crate) fn for_player(state: &GameState, player: PlayerId) -> Self {
        let (supply, land_count) = land_color_supply(state, player);
        Self {
            supply,
            demand: hand_color_demand(state, player, land_count),
        }
    }

    /// How many of the colours in `produced` the hand demands and the
    /// battlefield cannot yet produce. Uncapped; a caller caps it if it needs to.
    pub(crate) fn covered(&self, produced: &[ManaType]) -> usize {
        produced
            .iter()
            .copied()
            .filter_map(mana_type_to_demand_index)
            .filter(|&index| self.demand[index] > self.supply[index])
            .count()
    }
}

/// [`UnmetColors::covered`] for one producer. Ordered so a colourless producer
/// pays for nothing: `produced` is read first, and only a land that produces at
/// least one colour triggers the battlefield and hand scans.
pub(crate) fn unmet_hand_colors_covered(
    state: &GameState,
    player: PlayerId,
    produced: &[ManaType],
) -> usize {
    if produced.is_empty() {
        return 0;
    }
    UnmetColors::for_player(state, player).covered(produced)
}

/// CR 305.2 + CR 107.4 + CR 202.3: whether `player` could pay `cost` once at
/// most `drops` more lands from their hand are played, one a turn: the mana
/// sources they control now plus up to `drops` lands from hand cover the mana
/// value, and every coloured pip has a source of its own that produces one of
/// its colours (a source pays one pip).
///
/// A mana source is a land or a permanent with a renewable mana ability
/// (`zone_eval::is_intrinsic_mana_source`), read by the colours it can
/// produce ([`land_produced_color_types`]); whether it is tapped now is not
/// read, since this asks about a later turn. A shard payable another way
/// ({2/U}, {U/P}) is read as needing its colour, the conservative reading.
/// A cost with no mana symbols to pay (`NoCost`, a self-referential cost) is
/// in reach.
pub(crate) fn cost_within_land_drops(
    state: &GameState,
    player: PlayerId,
    cost: &ManaCost,
    drops: usize,
) -> bool {
    let ManaCost::Cost { shards, .. } = cost else {
        return true;
    };
    let colours_of = |id: &ObjectId| -> Option<ColorDemand> {
        let obj = state.objects.get(id)?;
        let mut colours = [0u32; 5];
        for produced in land_produced_color_types(&obj.card_types.subtypes, &obj.abilities) {
            if let Some(index) = mana_type_to_demand_index(produced) {
                colours[index] = 1;
            }
        }
        Some(colours)
    };
    let in_play: Vec<ColorDemand> = state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == player && crate::zone_eval::is_intrinsic_mana_source(obj)
            })
        })
        .filter_map(colours_of)
        .collect();
    let in_hand: Vec<ColorDemand> = state
        .players
        .get(player.0 as usize)
        .map(|p| {
            p.hand
                .iter()
                .filter(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Land))
                })
                .filter_map(colours_of)
                .collect()
        })
        .unwrap_or_default();
    let played = drops.min(in_hand.len());
    if in_play.len() + played < cost.mana_value() as usize {
        return false;
    }
    let pips: Vec<ColorDemand> = shards
        .iter()
        .map(|&shard| {
            outer_cost_color_demand(&ManaCost::Cost {
                shards: vec![shard],
                generic: 0,
            })
        })
        .filter(|demand| demand.iter().any(|&n| n > 0))
        .collect();
    let mut chosen = Vec::with_capacity(played);
    any_land_choice(&in_hand, played, 0, &mut chosen, &mut |chosen| {
        let sources: Vec<ColorDemand> = in_play.iter().chain(chosen.iter()).copied().collect();
        pips_have_own_sources(&pips, &sources)
    })
}

/// Whether some choice of `count` lands from `lands[from..]`, added to
/// `chosen`, satisfies `accept`.
fn any_land_choice(
    lands: &[ColorDemand],
    count: usize,
    from: usize,
    chosen: &mut Vec<ColorDemand>,
    accept: &mut impl FnMut(&[ColorDemand]) -> bool,
) -> bool {
    if chosen.len() == count {
        return accept(chosen);
    }
    for index in from..lands.len() {
        chosen.push(lands[index]);
        let found = any_land_choice(lands, count, index + 1, chosen, accept);
        chosen.pop();
        if found {
            return true;
        }
    }
    false
}

/// CR 107.4: each coloured pip is paid by one mana, so each needs a source of
/// its own. A bipartite matching of pips to sources (augmenting paths), so a
/// dual land is spent on one pip, never counted for two.
fn pips_have_own_sources(pips: &[ColorDemand], sources: &[ColorDemand]) -> bool {
    fn assign(
        pip: usize,
        pips: &[ColorDemand],
        sources: &[ColorDemand],
        seen: &mut [bool],
        owner: &mut [Option<usize>],
    ) -> bool {
        for (source, colours) in sources.iter().enumerate() {
            let pays = pips[pip]
                .iter()
                .zip(colours)
                .any(|(&need, &has)| need > 0 && has > 0);
            if !pays || seen[source] {
                continue;
            }
            seen[source] = true;
            if owner[source].is_none_or(|other| assign(other, pips, sources, seen, owner)) {
                owner[source] = Some(pip);
                return true;
            }
        }
        false
    }
    let mut owner = vec![None; sources.len()];
    (0..pips.len()).all(|pip| {
        let mut seen = vec![false; sources.len()];
        assign(pip, pips, sources, &mut seen, &mut owner)
    })
}

/// One battlefield walk yielding both halves of the color-demand comparison:
/// how many of `player`'s lands produce each color, and how many lands they
/// control (the castability threshold the hand scan uses).
fn land_color_supply(state: &GameState, player: PlayerId) -> (ColorDemand, u32) {
    let mut supply = [0u32; 5];
    let mut lands = 0;
    for obj in state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
    {
        if obj.controller != player || !obj.card_types.core_types.contains(&CoreType::Land) {
            continue;
        }
        lands += 1;
        for color in land_produced_color_types(&obj.card_types.subtypes, &obj.abilities) {
            if let Some(index) = mana_type_to_demand_index(color) {
                supply[index] += 1;
            }
        }
    }
    (supply, lands)
}

/// CR 107.4b: element-wise MAX colored-pip demand over the nonland cards in
/// `player`'s hand that are within one land drop of being castable.
///
/// MAX, not sum: demand asks "how many sources of this color does one castable
/// card need," which is what a land drop can answer this turn — summing would
/// let a hand of three one-pip green cards claim three unmet green sources.
/// Castability is the same count-based check `ramp_timing` uses (CR 202.3 mana
/// value against lands controlled plus this turn's drop) — deliberately never an
/// affordability sweep, which is far too expensive per candidate.
fn hand_color_demand(state: &GameState, player: PlayerId, land_count: u32) -> ColorDemand {
    let mut demand = [0u32; 5];
    let Some(hand) = state.players.get(player.0 as usize).map(|p| &p.hand) else {
        return demand;
    };
    for obj in hand.iter().filter_map(|id| state.objects.get(id)) {
        if obj.card_types.core_types.contains(&CoreType::Land)
            || obj.mana_cost.mana_value() > land_count + 1
        {
            continue;
        }
        for (slot, needed) in demand
            .iter_mut()
            .zip(outer_cost_color_demand(&obj.mana_cost))
        {
            *slot = (*slot).max(needed);
        }
    }
    demand
}

/// Union the colors of a single `ManaProduction` into `colors` (deduplicated,
/// colorless excluded). Exhaustive over every `ManaProduction` variant: the
/// statically-known producers (Fixed/Mixed/AnyOneColor/AnyCombination, and the
/// filter-land `ChoiceAmongCombinations`) contribute their colors; the dynamic
/// producers (chosen/opponent/commander-identity/etc.) and pure Colorless
/// contribute nothing, since their colors aren't known from the card alone.
pub(crate) fn collect_mana_production_colors(
    colors: &mut Vec<ManaType>,
    produced: &ManaProduction,
) {
    match produced {
        ManaProduction::Fixed {
            colors: produced, ..
        }
        | ManaProduction::Mixed {
            colors: produced, ..
        }
        | ManaProduction::AnyOneColor {
            color_options: produced,
            ..
        }
        | ManaProduction::AnyCombination {
            color_options: produced,
            ..
        } => {
            for color in produced {
                push_color(colors, mana_color_to_type(color));
            }
        }
        ManaProduction::ChoiceAmongCombinations { options } => {
            for option in options {
                for color in option {
                    push_color(colors, mana_color_to_type(color));
                }
            }
        }
        ManaProduction::Colorless { .. }
        | ManaProduction::ChosenColor { .. }
        | ManaProduction::NotedType { .. }
        | ManaProduction::OpponentLandColors { .. }
        | ManaProduction::AnyTypeProduceableBy { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::AnyInCommandersColorIdentity { .. }
        | ManaProduction::DistinctColorsAmongPermanents { .. }
        | ManaProduction::AnyOneColorAmongPermanents { .. }
        // CR 202.2c: Omnath, Locus of All — colors come from a target object
        // resolved at trigger time, not known from the card alone.
        | ManaProduction::AnyCombinationOfObjectColors { .. }
        | ManaProduction::TriggerEventManaType => {}
    }
}

fn push_color(colors: &mut Vec<ManaType>, mana_type: ManaType) {
    if mana_type != ManaType::Colorless && !colors.contains(&mana_type) {
        colors.push(mana_type);
    }
}

/// Whether `mana_type` satisfies a colored pip the in-flight cost still demands.
/// WUBRG demand slot per color; Colorless has no slot, so it never satisfies a
/// colored pip.
fn color_is_demanded(demand: ColorDemand, mana_type: ManaType) -> bool {
    match mana_type {
        ManaType::White => demand[0] > 0,
        ManaType::Blue => demand[1] > 0,
        ManaType::Black => demand[2] > 0,
        ManaType::Red => demand[3] > 0,
        ManaType::Green => demand[4] > 0,
        ManaType::Colorless => false,
    }
}

/// CR 702.51a (Convoke) / CR 702.126a (Improvise) / Waterbend: whether tapping
/// `object_id` for its Colorless convoke-family marker should be rejected
/// because a currently-legal sibling candidate at this exact `ManaPayment`
/// decision lets `object_id` instead pay a colored pip the pending cast still
/// demands, via its own native mana ability.
///
/// This is zero-cost dominance, not a preference: both actions spend the SAME
/// single tap on the SAME permanent, but the native ability can still cover
/// the trailing generic slot once colored demand clears (or pay the colored
/// pip directly), while the Colorless marker can never retroactively produce
/// a stranded color. A dual-purpose permanent (e.g. an artifact land that
/// also taps for a color) could otherwise be spent via the marker first,
/// permanently stranding a colored pip and dead-ending `ManaPayment`.
pub(crate) fn convoke_native_tap_still_demanded(
    state: &GameState,
    candidates: &[CandidateAction],
    object_id: ObjectId,
) -> bool {
    let Some(pending_cast) = state.pending_cast.as_deref() else {
        return false;
    };
    let demand = outer_cost_color_demand(&pending_cast.cost);
    if demand == [0u32; 5] {
        return false;
    }
    candidates
        .iter()
        .any(|c| sibling_native_tap_pays_demand(state, &c.action, object_id, demand))
}

fn sibling_native_tap_pays_demand(
    state: &GameState,
    action: &GameAction,
    object_id: ObjectId,
    demand: ColorDemand,
) -> bool {
    match action {
        GameAction::TapLandForMana { selection } => {
            selection.source.object_id == object_id
                && color_is_demanded(demand, selection.mana_type)
        }
        // Only a tap-cost native ability actually competes for this same tap:
        // a tapless ability (e.g. a sacrifice-based mana ability) can still be
        // activated AFTER paying the Colorless marker, so it never strands a
        // colored pip and must not gate the Colorless action. Use the cost's
        // own category classification (CR 118) rather than re-matching cost
        // shapes by hand -- it already flattens Composite costs correctly.
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } if *source_id == object_id => state
            .objects
            .get(source_id)
            .and_then(|obj| obj.abilities.get(*ability_index))
            .is_some_and(|ability| {
                let taps_self = ability
                    .cost
                    .as_ref()
                    .is_some_and(|cost| cost.categories().contains(&CostCategory::TapsSelf));
                if !taps_self {
                    return false;
                }
                let mut colors = Vec::new();
                if let Effect::Mana { produced, .. } = &*ability.effect {
                    collect_mana_production_colors(&mut colors, produced);
                }
                colors.iter().any(|&c| color_is_demanded(demand, c))
            }),
        // CR 702.51a: Convoke (unlike Improvise/Waterbend) offers a colored
        // marker per color the creature has, alongside the Colorless one --
        // `mana_payment_actions` emits both for the same object. A colored
        // `TapForConvoke` on the SAME object is just as dominating a sibling
        // as a native land/ability tap: it pays a matching colored pip, so
        // the Colorless marker is never the only way to spend this creature.
        GameAction::TapForConvoke {
            object_id: sibling_id,
            mana_type,
        } if *sibling_id == object_id => color_is_demanded(demand, *mana_type),
        _ => false,
    }
}
