//! The move-ordering prior steers which child is searched first, never the
//! value a node returns (`simforge/docs/spec-ai-ordering-bonus-out-of-values-2026-10-20.md`;
//! judge S2 and row JB on 19b88154a, `fork-checks/19b88154a/JUDGE.md`).
//!
//! A node's children are ranked by the tactical score (interior beam) or the
//! policy prior (rollout sample). That order decides what is searched first,
//! what the beam keeps and what the rollout samples. The value a node returns
//! SHALL be the best searched child alone: two children whose searched values
//! are equal return the same value whatever their priors, and raising one
//! child's prior SHALL NOT raise the node's value.
//!
//! The table: P0 in precombat main with one Island and two identical
//! "Draw a card." sorceries for {U}. Casting either reaches the same position,
//! so the two casts have equal searched values. A test-only policy raises (or
//! lowers) the prior of one of them; the default registry is not used, so the
//! test policy is the only thing that moves a prior.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::{BeamContinuationPlanner, PlannerServices, SearchBudget};
use crate::config::{create_config, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};

/// Adds `delta` to the prior of casting `favored`, neutral on everything else.
struct FavorCast {
    favored: ObjectId,
    delta: f64,
}

impl TacticalPolicy for FavorCast {
    /// Any stable id; the registry under test holds only this policy.
    fn id(&self) -> PolicyId {
        PolicyId::PaymentSelection
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        _: &crate::features::DeckFeatures,
        _: &GameState,
        _: PlayerId,
    ) -> Option<f32> {
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        match ctx.candidate.action {
            GameAction::CastSpell { object_id, .. } if object_id == self.favored => {
                PolicyVerdict::score(self.delta, PolicyReason::new("favor_cast_test"))
            }
            _ => PolicyVerdict::neutral(PolicyReason::new("favor_cast_test")),
        }
    }
}

/// P0 precombat main, one Island, two "Draw a card." sorceries for {U};
/// both libraries stocked. Returns the table and the two spells.
fn two_identical_draws() -> (GameState, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for _ in 0..20 {
        s.add_land_to_library_top(P0, "Island");
        s.add_land_to_library_top(P1, "Plains");
    }
    s.add_basic_land(P0, ManaColor::Blue);
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    };
    let a = s
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(cost.clone())
        .id();
    let b = s
        .add_spell_to_hand(P0, "Peek", false)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(cost)
        .id();
    (s.build().state().clone(), a, b)
}

fn registry(favor: Option<(ObjectId, f64)>) -> PolicyRegistry {
    let policies: Vec<Box<dyn TacticalPolicy>> = match favor {
        Some((favored, delta)) => vec![Box::new(FavorCast { favored, delta })],
        None => Vec::new(),
    };
    PolicyRegistry::for_tests(policies)
}

/// Alpha-beta value of `state` for P0 at `depth` plies under `policies`.
fn searched_with(state: &GameState, depth: u32, policies: &PolicyRegistry) -> f64 {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let context = AiContext::empty(&config.weights);
    let mut services = PlannerServices::new(P0, &config, policies, context);
    let mut budget = SearchBudget::new(config.search.max_nodes);
    let planner = BeamContinuationPlanner {
        depth,
        rollout_depth: config.search.rollout_depth,
    };
    planner.search_value(
        state,
        depth,
        0,
        f64::NEG_INFINITY,
        f64::INFINITY,
        &mut services,
        &mut budget,
    )
}

/// Rollout value of `state` for P0 at `depth` under `policies`.
fn rollout_with(state: &GameState, depth: u32, policies: &PolicyRegistry) -> f64 {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let context = AiContext::empty(&config.weights);
    let mut services = PlannerServices::new(P0, &config, policies, context);
    services.rollout_estimate_at(None, state, depth)
}

/// The interior beam's ranked score of casting `spell` at `state`.
fn cast_score(state: &GameState, spell: ObjectId, policies: &PolicyRegistry) -> f64 {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let context = AiContext::empty(&config.weights);
    let mut services = PlannerServices::new(P0, &config, policies, context);
    let ctx = services.build_decision_context_at(None, state);
    let candidate = ctx
        .candidates
        .iter()
        .find(|c| matches!(c.action, GameAction::CastSpell { object_id, .. } if object_id == spell))
        .expect("reach-guard: the cast is a candidate")
        .clone();
    services.tactical_score(state, &ctx, &candidate, P0, SearchDepth::Lookahead)
}

/// The prior assignments every row compares: none, either cast raised, one
/// cast lowered.
fn assignments(a: ObjectId, b: ObjectId) -> [(&'static str, Option<(ObjectId, f64)>); 5] {
    [
        ("neutral", None),
        ("A +1.0", Some((a, 1.0))),
        ("B +1.0", Some((b, 1.0))),
        ("A +0.3", Some((a, 0.3))),
        ("A -1.0", Some((a, -1.0))),
    ]
}

/// Interior beam node (`search_value_at`, P0 to act, depth 1 and 2): the two
/// casts have equal searched values and their ranked scores differ by the
/// test policy's delta. The node's value SHALL be the same under every prior
/// assignment (red at 19b88154a: raising a cast's prior by 1.0 raised the
/// node by 0.05).
#[test]
fn interior_node_value_is_independent_of_child_priors() {
    let (st, a, b) = two_identical_draws();
    let neutral = registry(None);
    let favored = registry(Some((a, 1.0)));
    let (sa, sb) = (cast_score(&st, a, &favored), cast_score(&st, b, &favored));
    eprintln!("ordering: ranked score with A +1.0: A {sa:.4} B {sb:.4}");
    assert!(
        (sa - sb - 1.0).abs() < 1e-9,
        "reach-guard: the priors differ by the policy's delta"
    );
    assert!(
        (cast_score(&st, a, &neutral) - cast_score(&st, b, &neutral)).abs() < 1e-9,
        "reach-guard: the two casts score alike without the policy"
    );
    for depth in [1, 2] {
        let base = searched_with(&st, depth, &neutral);
        for (label, favor) in assignments(a, b) {
            let v = searched_with(&st, depth, &registry(favor));
            eprintln!("interior depth {depth} {label}: {v:.6} (neutral {base:.6})");
            assert!(
                (v - base).abs() < 1e-9,
                "depth {depth}, {label}: the node's value SHALL NOT carry the prior ({v:.6} against {base:.6})"
            );
        }
    }
}

/// Rollout node (`rollout_estimate_at`, depth 1 and 2): the sampled
/// continuations are ordered by the policy prior. The node's value SHALL be
/// the same under every prior assignment (red at 19b88154a: the best child
/// carried `+ prior * 0.05`).
#[test]
fn rollout_node_value_is_independent_of_child_priors() {
    let (st, a, b) = two_identical_draws();
    let neutral = registry(None);
    for depth in [1, 2] {
        let base = rollout_with(&st, depth, &neutral);
        for (label, favor) in assignments(a, b) {
            let v = rollout_with(&st, depth, &registry(favor));
            eprintln!("rollout depth {depth} {label}: {v:.6} (neutral {base:.6})");
            assert!(
                (v - base).abs() < 1e-9,
                "depth {depth}, {label}: the rollout's value SHALL NOT carry the prior ({v:.6} against {base:.6})"
            );
        }
    }
}

/// The node's value SHALL equal the searched value of its best child: at
/// depth 1 the children of a cast are searched at depth 0 (the rollout), so
/// the node SHALL equal the larger of the cast's and the pass's searched
/// values, with no offset, under the raised prior.
#[test]
fn interior_node_value_is_its_best_childs_searched_value() {
    let (st, a, _b) = two_identical_draws();
    let favored = registry(Some((a, 1.0)));
    let config = create_config(AiDifficulty::VeryHard, Platform::Native).into_measurement(42);
    let mut children = Vec::new();
    for action in engine::ai_support::legal_actions(&st) {
        let mut sim = st.clone();
        if engine::game::engine::apply_as_current_for_simulation(&mut sim, action.clone()).is_err()
        {
            continue;
        }
        let v = rollout_with(&sim, config.search.rollout_depth, &favored);
        eprintln!("child {action:?}: {v:.6}");
        children.push(v);
    }
    let node = searched_with(&st, 1, &favored);
    let best = children.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    eprintln!("node {node:.6} best child {best:.6}");
    assert!(children.len() >= 2, "reach-guard: a cast and the pass");
    assert!(
        (node - best).abs() < 1e-9,
        "the node SHALL return its best child's searched value ({node:.6} against {best:.6})"
    );
}
