//! Making a stack that fails a check as small as it will go, so what's
//! wrong is easy to see and to keep as a test.

use super::{Kind, Spec, chain, valid};

/// The first move that leaves a stack that still fails, if any does.
fn step(specs: &[Spec], fails: &impl Fn(&[Spec]) -> bool) -> Option<Vec<Spec>> {
    // 1. Take out a group with everything in it.
    for g in specs.iter().filter(|s| s.kind == Kind::Group) {
        let cand: Vec<Spec> = specs.iter().filter(|s| s.id != g.id && !chain(specs, s).contains(&g.id)).cloned().collect();
        if valid(&cand) && fails(&cand) {
            return Some(cand);
        }
    }
    // 2. Take out a group alone: direct children get the group's parent.
    for g in specs.iter().filter(|s| s.kind == Kind::Group) {
        let cand: Vec<Spec> = specs
            .iter()
            .filter(|s| s.id != g.id)
            .cloned()
            .map(|mut s| {
                if s.parent == Some(g.id) {
                    s.parent = g.parent;
                }
                s
            })
            .collect();
        if valid(&cand) && fails(&cand) {
            return Some(cand);
        }
    }
    // 3. Take out one layer that isn't a group.
    for (i, _) in specs.iter().enumerate().filter(|(_, s)| s.kind != Kind::Group) {
        let mut cand = specs.to_vec();
        cand.remove(i);
        if valid(&cand) && fails(&cand) {
            return Some(cand);
        }
    }
    // 4. Set one field of one spec to plain, if it isn't already.
    for (i, s) in specs.iter().enumerate() {
        let plain = Spec::new(s.id, s.kind);
        let try_field = |apply: fn(&mut Spec, &Spec)| -> Option<Vec<Spec>> {
            let mut cand = specs.to_vec();
            apply(&mut cand[i], &plain);
            (cand[i] != *s && valid(&cand) && fails(&cand)).then_some(cand)
        };
        if let Some(cand) = try_field(|c, p| c.clipped = p.clipped)
            .or_else(|| try_field(|c, p| c.visible = p.visible))
            .or_else(|| try_field(|c, p| c.blend = p.blend))
            .or_else(|| try_field(|c, p| c.opacity = p.opacity))
            .or_else(|| try_field(|c, p| c.mask = p.mask))
            .or_else(|| try_field(|c, p| c.blend_if = p.blend_if))
        {
            return Some(cand);
        }
    }
    None
}

/// The smallest stack found that still `fails`, from one that does.
pub fn shrink(mut specs: Vec<Spec>, fails: impl Fn(&[Spec]) -> bool) -> Vec<Spec> {
    while let Some(next) = step(&specs, &fails) {
        specs = next;
    }
    specs
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use omapix_engine::BlendMode;

    use super::*;
    use crate::fuzz::{MaskSpec, specs};

    fn diff_and_mask_stack(seed: u64) -> (Vec<Spec>, u64, Kind) {
        let mut stack = specs(seed);
        let mid = stack.len() / 2;
        stack[mid].blend = BlendMode::Difference;
        stack[mid].mask = MaskSpec::On;
        (stack.clone(), stack[mid].id, stack[mid].kind)
    }

    fn has_diff_and_mask(s: &[Spec]) -> bool {
        s.iter().any(|l| l.blend == BlendMode::Difference && l.mask == MaskSpec::On)
    }

    fn has_group_pair(s: &[Spec]) -> bool {
        s.iter()
            .any(|g| g.kind == Kind::Group && s.iter().filter(|l| l.parent == Some(g.id)).count() >= 2)
    }

    #[test]
    fn one_layer_is_left_with_only_what_fails() {
        for seed in 0..200 {
            let (stack, id, kind) = diff_and_mask_stack(seed);
            let candidates: Vec<(u64, Kind)> = stack
                .iter()
                .filter(|s| has_diff_and_mask(std::slice::from_ref(s)))
                .map(|s| (s.id, s.kind))
                .collect();
            let shrunk = shrink(stack, has_diff_and_mask);
            assert_eq!(shrunk.len(), 1, "seed {seed}");
            let (kept_id, kept_kind) = (shrunk[0].id, shrunk[0].kind);
            if candidates.len() == 1 {
                assert_eq!((kept_id, kept_kind), (id, kind), "seed {seed}");
            } else {
                assert!(candidates.contains(&(kept_id, kept_kind)), "seed {seed}");
            }
            assert_eq!(
                shrunk,
                vec![Spec {
                    blend: BlendMode::Difference,
                    mask: MaskSpec::On,
                    ..Spec::new(kept_id, kept_kind)
                }],
                "seed {seed}"
            );
        }
    }

    #[test]
    fn a_group_keeps_the_two_layers_that_fail_with_it() {
        let mut qualified = 0;
        for seed in 0..500 {
            let stack = specs(seed);
            if !has_group_pair(&stack) {
                continue;
            }
            qualified += 1;
            let shrunk = shrink(stack, has_group_pair);
            assert_eq!(shrunk.len(), 3, "seed {seed}");
            assert_eq!(shrunk[2].kind, Kind::Group, "seed {seed}");
            let gid = shrunk[2].id;
            assert_eq!(shrunk[0].parent, Some(gid), "seed {seed}");
            assert_eq!(shrunk[1].parent, Some(gid), "seed {seed}");
            for s in &shrunk {
                assert_eq!(
                    *s,
                    Spec {
                        parent: s.parent,
                        ..Spec::new(s.id, s.kind)
                    },
                    "seed {seed}"
                );
            }
        }
        assert!(qualified >= 50, "only {qualified} seeds qualified");
    }

    #[test]
    fn a_stack_that_fails_only_whole_comes_back_as_it_was() {
        for seed in 0..50 {
            let orig = specs(seed);
            assert_eq!(shrink(orig.clone(), |s| s == orig.as_slice()), orig, "seed {seed}");
        }
    }

    #[test]
    fn only_valid_stacks_are_tried_and_not_too_many() {
        for seed in 0..200 {
            let (stack, _, _) = diff_and_mask_stack(seed);
            let calls = Cell::new(0usize);
            shrink(stack, |s| {
                calls.set(calls.get() + 1);
                assert!(valid(s), "seed {seed}: invalid candidate");
                has_diff_and_mask(s)
            });
            assert!(calls.get() < 2000, "seed {seed}: {} calls", calls.get());
        }
    }

    #[test]
    fn a_shrunk_stack_shrinks_no_further() {
        for seed in 0..200 {
            let (stack, _, _) = diff_and_mask_stack(seed);
            let shrunk = shrink(stack, has_diff_and_mask);
            assert_eq!(shrink(shrunk.clone(), has_diff_and_mask), shrunk, "seed {seed}");
        }
        for seed in 0..500 {
            let stack = specs(seed);
            if has_group_pair(&stack) {
                let shrunk = shrink(stack, has_group_pair);
                assert_eq!(shrink(shrunk.clone(), has_group_pair), shrunk, "seed {seed}");
            }
        }
    }
}
