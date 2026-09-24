//! Layer groups: folders in the layer stack.
//!
//! The stack stays one flat list, bottom first, as in a PSD file. A group
//! is a layer with `is_group` set, and what's in it (including nested
//! groups) sits directly below it, each naming it as `parent`. So a layer
//! and everything in it always occupy one contiguous range of indices,
//! its *span*, with the layer itself on top. The edits here keep it that
//! way.

use std::collections::HashMap;
use std::ops::Range;

use crate::Document;
use crate::layer::Layer;

/// Where to put a layer (with everything in it) in the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Just above this layer, in the same group.
    Above(u64),
    /// Just below this layer (and everything in it), in the same group.
    Below(u64),
    /// At the top of this group's contents.
    IntoTop(u64),
    /// At the bottom of this group's contents.
    IntoBottom(u64),
}

impl Document {
    /// The indices of the layer at `index` and, for a group, everything in
    /// it, which sits directly below.
    pub fn span(&self, index: usize) -> Range<usize> {
        let id = self.layers[index].id;
        let mut start = index;
        while start > 0 && self.is_inside(self.layers[start - 1].id, id) {
            start -= 1;
        }
        start..index + 1
    }

    /// Whether layer `id` is in group `group`, directly or in a nested group.
    pub fn is_inside(&self, id: u64, group: u64) -> bool {
        let mut parent = self.layer(id).and_then(|l| l.parent);
        while let Some(p) = parent {
            if p == group {
                return true;
            }
            parent = self.layer(p).and_then(|l| l.parent);
        }
        false
    }

    /// How many groups deep layer `id` is (0 for the top level).
    pub fn depth(&self, id: u64) -> usize {
        let mut depth = 0;
        let mut parent = self.layer(id).and_then(|l| l.parent);
        while let Some(p) = parent {
            depth += 1;
            parent = self.layer(p).and_then(|l| l.parent);
        }
        depth
    }

    /// Insert a new layer above the one at `index`, or at the top of it if
    /// it's a group, as Photoshop does. Returns where it went. Inserted into
    /// a clipping mask, just above or below a clipped layer, it's clipped
    /// too.
    pub fn insert_above(&mut self, index: usize, mut layer: Layer) -> usize {
        let below = &self.layers[index];
        let at = if below.is_group {
            layer.parent = Some(below.id);
            index
        } else {
            layer.parent = below.parent;
            index + 1
        };
        self.layers.insert(at, layer);
        let below = self.sibling_below(at).map(|i| &self.layers[i]);
        let in_run = below.is_some_and(|l| l.clipped)
            || (below.is_some() && self.sibling_above(at).is_some_and(|l| l.clipped));
        if !self.layers[at].is_group && in_run {
            self.layers[at].clipped = true;
        }
        at
    }

    /// The next layer up in the same group as the one at `index`.
    fn sibling_above(&self, index: usize) -> Option<&Layer> {
        self.sibling_above_index(index).map(|i| &self.layers[i])
    }

    /// The index of the next layer up in the same group as the one at
    /// `index`.
    fn sibling_above_index(&self, index: usize) -> Option<usize> {
        let parent = self.layers[index].parent;
        let after = self.layers[index + 1..]
            .iter()
            .take_while(|l| Some(l.id) != parent)
            .position(|l| l.parent == parent)?;
        Some(index + 1 + after)
    }

    /// The next layer down in the same group as the one at `index`.
    fn sibling_below(&self, index: usize) -> Option<usize> {
        let below = self.span(index).start.checked_sub(1)?;
        (self.layers[below].parent == self.layers[index].parent).then_some(below)
    }

    /// Whether the layer at `index` can be clipped: there's a layer below it
    /// in its group.
    pub fn can_clip(&self, index: usize) -> bool {
        self.sibling_below(index).is_some()
    }

    /// The layer that layer `id` is clipped to: the first unclipped layer
    /// below it in its group. `None` if it isn't clipped, or has nothing to
    /// clip to (it then shows as if it weren't clipped).
    pub fn clip_base(&self, id: u64) -> Option<u64> {
        let mut index = self.index_of(id)?;
        if !self.layers[index].clipped {
            return None;
        }
        loop {
            index = self.sibling_below(index)?;
            if !self.layers[index].clipped {
                return Some(self.layers[index].id);
            }
        }
    }

    /// Whether layers are clipped to layer `id` (Photoshop underlines its
    /// name).
    pub fn is_clip_base(&self, id: u64) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let above = self.sibling_above(index);
        above.is_some_and(|l| self.clip_base(l.id) == Some(id))
    }

    /// Photoshop's Create/Release Clipping Mask (Ctrl+Alt+G) on layer `id`:
    /// clip it to the layer below, or if it's clipped, release it and the
    /// clipped layers above it. Returns false if there's nothing to clip to.
    pub fn toggle_clipping(&mut self, id: u64) -> bool {
        let Some(mut index) = self.index_of(id) else {
            return false;
        };
        if !self.layers[index].clipped {
            if !self.can_clip(index) {
                return false;
            }
            self.layers[index].clipped = true;
            return true;
        }
        loop {
            self.layers[index].clipped = false;
            let parent = self.layers[index].parent;
            let above = self.layers[index + 1..]
                .iter()
                .take_while(|l| Some(l.id) != parent)
                .position(|l| l.parent == parent);
            match above.map(|p| index + 1 + p) {
                Some(i) if self.layers[i].clipped => index = i,
                _ => return true,
            }
        }
    }

    /// Move layer `id`, with everything in it, to `place`. Returns false if
    /// it can't go there: into itself, or into something that isn't a group.
    pub fn move_layer(&mut self, id: u64, place: Place) -> bool {
        let (target, parent) = match place {
            Place::Above(t) | Place::Below(t) => (t, self.layer(t).map(|l| l.parent)),
            Place::IntoTop(g) | Place::IntoBottom(g) => (
                g,
                self.layer(g).filter(|l| l.is_group).map(|_| Some(g)),
            ),
        };
        let (Some(index), Some(parent)) = (self.index_of(id), parent) else {
            return false;
        };
        if target == id || self.is_inside(target, id) {
            return false;
        }
        let span = self.span(index);
        let mut moving: Vec<Layer> = self.layers.drain(span).collect();
        moving.last_mut().expect("a span holds its layer").parent = parent;
        let t = self.index_of(target).expect("target is outside what moved");
        let at = match place {
            Place::Above(_) => t + 1,
            Place::Below(_) | Place::IntoBottom(_) => self.span(t).start,
            Place::IntoTop(_) => t,
        };
        self.layers.splice(at..at, moving);
        true
    }

    /// Where Bring Forward (Ctrl+]) takes layer `id`: above the layer above
    /// it, into the bottom of a group above it, or out of the top of its
    /// own group. `None` at the top of the stack.
    pub fn raise_place(&self, id: u64) -> Option<Place> {
        let index = self.index_of(id)?;
        let parent = self.layers[index].parent;
        let above = self.layers.get(self.span(index).end)?;
        if Some(above.id) == parent {
            return Some(Place::Above(above.id));
        }
        // What's directly above may be deep inside the group above.
        let mut sibling = above;
        while sibling.parent != parent {
            sibling = self.layer(sibling.parent?)?;
        }
        Some(if sibling.is_group {
            Place::IntoBottom(sibling.id)
        } else {
            Place::Above(sibling.id)
        })
    }

    /// Where Send Backward (Ctrl+[) takes layer `id`: below the layer below
    /// it, into the top of a group below it, or out of the bottom of its
    /// own group. `None` at the bottom of the stack.
    pub fn lower_place(&self, id: u64) -> Option<Place> {
        let index = self.index_of(id)?;
        let parent = self.layers[index].parent;
        let below = self.layers.get(self.span(index).start.checked_sub(1)?)?;
        if below.parent != parent {
            return Some(Place::Below(parent?));
        }
        Some(if below.is_group {
            Place::IntoTop(below.id)
        } else {
            Place::Below(below.id)
        })
    }

    /// Where Bring to Front (Ctrl+Shift+]) takes layer `id`: to the top of
    /// its group (or root stack). `None` if it is already at the top.
    pub fn front_place(&self, id: u64) -> Option<Place> {
        self.front_place_layers(&[id], id)
    }

    /// Where Send to Back (Ctrl+Shift+[) takes layer `id`: to the bottom of
    /// its group (or root stack). `None` if it is already at the bottom.
    pub fn back_place(&self, id: u64) -> Option<Place> {
        self.back_place_layers(&[id], id)
    }

    /// Where Bring to Front takes several layers: to the top of the group
    /// containing `active`. `None` if all selected siblings are already at
    /// the top.
    pub fn front_place_layers(&self, ids: &[u64], active: u64) -> Option<Place> {
        let parent = self.layer(active)?.parent;
        let outermost = self.outermost(ids);
        match parent {
            Some(g) => {
                let g_index = self.index_of(g)?;
                let span = self.span(g_index);
                let siblings: Vec<u64> = self.layers[span.start..g_index]
                    .iter()
                    .rev()
                    .filter(|l| l.parent == Some(g))
                    .map(|l| l.id)
                    .collect();
                let count = outermost.iter().filter(|id| siblings.contains(id)).count();
                if count == 0 {
                    return None;
                }
                let already_top = siblings.iter().take(count).all(|id| outermost.contains(id));
                if already_top {
                    None
                } else {
                    Some(Place::IntoTop(g))
                }
            }
            None => {
                let top = self.layers.last()?;
                let roots: Vec<u64> = self.layers
                    .iter()
                    .rev()
                    .filter(|l| l.parent.is_none())
                    .map(|l| l.id)
                    .collect();
                let count = outermost.iter().filter(|id| roots.contains(id)).count();
                if count == 0 {
                    return None;
                }
                let already_top = roots.iter().take(count).all(|id| outermost.contains(id));
                if already_top {
                    None
                } else {
                    Some(Place::Above(top.id))
                }
            }
        }
    }

    /// Where Send to Back takes several layers: to the bottom of the group
    /// containing `active`. `None` if all selected siblings are already at
    /// the bottom.
    pub fn back_place_layers(&self, ids: &[u64], active: u64) -> Option<Place> {
        let parent = self.layer(active)?.parent;
        let outermost = self.outermost(ids);
        match parent {
            Some(g) => {
                let g_index = self.index_of(g)?;
                let span = self.span(g_index);
                let siblings: Vec<u64> = self.layers[span.start..g_index]
                    .iter()
                    .filter(|l| l.parent == Some(g))
                    .map(|l| l.id)
                    .collect();
                let count = outermost.iter().filter(|id| siblings.contains(id)).count();
                if count == 0 {
                    return None;
                }
                let already_bottom = siblings.iter().take(count).all(|id| outermost.contains(id));
                if already_bottom {
                    None
                } else {
                    Some(Place::IntoBottom(g))
                }
            }
            None => {
                let bottom = self.layers.iter().find(|l| l.parent.is_none())?;
                let roots: Vec<u64> = self.layers
                    .iter()
                    .filter(|l| l.parent.is_none())
                    .map(|l| l.id)
                    .collect();
                let count = outermost.iter().filter(|id| roots.contains(id)).count();
                if count == 0 {
                    return None;
                }
                let already_bottom = roots.iter().take(count).all(|id| outermost.contains(id));
                if already_bottom {
                    None
                } else {
                    Some(Place::Below(bottom.id))
                }
            }
        }
    }

    /// Remove the layer at `index` and everything in it.
    pub fn remove_layer(&mut self, index: usize) -> Vec<Layer> {
        let span = self.span(index);
        self.layers.drain(span).collect()
    }

    /// Duplicate the layer at `index`, with everything in it, just above
    /// it. Returns the copy's id.
    pub fn duplicate_layer(&mut self, index: usize) -> u64 {
        let span = self.span(index);
        let mut copies = self.layers[span.clone()].to_vec();
        let ids: HashMap<u64, u64> = copies
            .iter()
            .map(|l| (l.id, self.next_layer_id()))
            .collect();
        for copy in &mut copies {
            copy.id = ids[&copy.id];
            if let Some(p) = copy.parent.and_then(|p| ids.get(&p)) {
                copy.parent = Some(*p);
            }
        }
        let top = copies.last_mut().expect("a span holds its layer");
        top.name = format!("{} copy", top.name);
        let id = top.id;
        self.layers.splice(span.end..span.end, copies);
        id
    }

    /// Put the layer at `index` in a new group of its own (Ctrl+G).
    /// Returns the group's id.
    pub fn group_layer(&mut self, index: usize) -> u64 {
        let span = self.span(index);
        let id = self.next_layer_id();
        let name = self.unused_name("Group");
        let mut group = Layer::group(id, name, self.width, self.height);
        group.parent = self.layers[index].parent;
        self.layers[index].parent = Some(id);
        self.layers.insert(span.end, group);
        id
    }

    /// An empty group above the layer at `index` (or at the top of it, if
    /// it's a group). Returns the group's id.
    pub fn new_group(&mut self, index: usize) -> u64 {
        let id = self.next_layer_id();
        let name = self.unused_name("Group");
        let group = Layer::group(id, name, self.width, self.height);
        self.insert_above(index, group);
        id
    }

    /// Of several selected layers, those not inside another of them (what's
    /// in a selected group goes with it anyway), bottom first. Leaves out
    /// ids that don't exist.
    pub fn outermost(&self, ids: &[u64]) -> Vec<u64> {
        let mut out: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|&id| self.layer(id).is_some())
            .filter(|&id| !ids.iter().any(|&g| g != id && self.is_inside(id, g)))
            .collect();
        out.sort_by_key(|&id| self.index_of(id));
        out.dedup();
        out
    }

    /// Move several layers (see [`Self::outermost`]) to `place`, keeping
    /// their order, as one block. Returns false if `place` is into one of
    /// them.
    pub fn move_layers(&mut self, ids: &[u64], place: Place) -> bool {
        let ids = self.outermost(ids);
        let (Place::Above(t) | Place::Below(t) | Place::IntoTop(t) | Place::IntoBottom(t)) = place;
        if let (Some(k), Place::Above(_) | Place::Below(_)) =
            (ids.iter().position(|&id| id == t), place)
        {
            // Just above or below one of them, they go there among the
            // layers that stay: above the next one down in its group, or
            // below the next one up.
            let stays = |i: &usize| !ids.contains(&self.layers[*i].id);
            let index = self.index_of(t).expect("outermost layers exist");
            let below = std::iter::successors(self.sibling_below(index), |&i| self.sibling_below(i))
                .find(stays);
            let above = std::iter::successors(self.sibling_above_index(index), |&i| {
                self.sibling_above_index(i)
            })
            .find(stays);
            match (below, above) {
                (Some(b), _) => return self.move_layers(&ids, Place::Above(self.layers[b].id)),
                (None, Some(a)) => return self.move_layers(&ids, Place::Below(self.layers[a].id)),
                _ => {}
            }
            // Nothing else is in their group: they gather round this one.
            for i in k + 1..ids.len() {
                self.move_layer(ids[i], Place::Above(ids[i - 1]));
            }
            for i in (0..k).rev() {
                self.move_layer(ids[i], Place::Below(ids[i + 1]));
            }
            return true;
        }
        if ids.iter().any(|&id| t == id || self.is_inside(t, id)) {
            return false;
        }
        // The top one goes to `place`, and each of the others below the one
        // above it.
        let mut place = place;
        for &id in ids.iter().rev() {
            if !self.move_layer(id, place) {
                return false;
            }
            place = Place::Below(id);
        }
        true
    }

    /// Put several layers (see [`Self::outermost`]) in one new group,
    /// keeping their order, where the top one was (Ctrl+G with several
    /// selected). Returns the group's id, or `None` if there are none.
    pub fn group_layers(&mut self, ids: &[u64]) -> Option<u64> {
        let ids = self.outermost(ids);
        let (&top, rest) = ids.split_last()?;
        let group = self.group_layer(self.index_of(top)?);
        let mut place = Place::Below(top);
        for &id in rest.iter().rev() {
            self.move_layer(id, place);
            place = Place::Below(id);
        }
        Some(group)
    }

    /// Duplicate several layers (see [`Self::outermost`]), each just above
    /// itself. Returns the copies' ids, bottom first.
    pub fn duplicate_layers(&mut self, ids: &[u64]) -> Vec<u64> {
        let ids = self.outermost(ids);
        // From the top down, so the indices below stay put.
        let mut copies = Vec::with_capacity(ids.len());
        for &id in ids.iter().rev() {
            if let Some(index) = self.index_of(id) {
                copies.push(self.duplicate_layer(index));
            }
        }
        copies.reverse();
        copies
    }

    /// Remove several layers (see [`Self::outermost`]) and everything in
    /// them. Returns the index just below where the lowest one was.
    pub fn remove_layers(&mut self, ids: &[u64]) -> Option<usize> {
        let ids = self.outermost(ids);
        let lowest = self.span(self.index_of(*ids.first()?)?).start;
        for &id in ids.iter().rev() {
            if let Some(index) = self.index_of(id) {
                self.remove_layer(index);
            }
        }
        Some(lowest.saturating_sub(1))
    }

    /// How many layers removing several (see [`Self::remove_layers`])
    /// would remove.
    pub fn removed_count(&self, ids: &[u64]) -> usize {
        self.outermost(ids)
            .iter()
            .filter_map(|&id| self.index_of(id))
            .map(|index| self.span(index).len())
            .sum()
    }

    /// Take everything out of the group at `index` and delete the group
    /// (Ctrl+Shift+G). Returns the id of the layer to select: the top one
    /// that was in the group, or the layer below if it was empty.
    pub fn ungroup(&mut self, index: usize) -> Option<u64> {
        let group = self.layers.get(index).filter(|l| l.is_group)?;
        let (id, parent) = (group.id, group.parent);
        for layer in &mut self.layers {
            if layer.parent == Some(id) {
                layer.parent = parent;
            }
        }
        self.layers.remove(index);
        let below = index.checked_sub(1).or((!self.layers.is_empty()).then_some(0))?;
        self.layers.get(below).map(|l| l.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorProfile, Raster};

    /// A document of empty layers with these names, bottom first.
    fn plain(names: &[&str]) -> Document {
        let raster = Raster::new(4, 4, vec![[0; 4]; 16]);
        let mut doc = Document::from_image("t.tif".into(), &raster, ColorProfile::srgb(), 16);
        doc.layers.clear();
        for name in names {
            let id = doc.next_layer_id();
            doc.layers.push(Layer::empty(id, *name, 4, 4));
        }
        doc
    }

    /// Bottom first: bg, then a and b in group g, then c. Built with the
    /// operations themselves.
    fn grouped() -> Document {
        let mut doc = plain(&["bg", "a", "b", "c"]);
        let b = doc.index_of(id(&doc, "b")).unwrap();
        let g = doc.group_layer(b);
        doc.layer_mut(g).unwrap().name = "g".into();
        let a = id(&doc, "a");
        assert!(doc.move_layer(a, Place::IntoBottom(g)));
        doc
    }

    fn id(doc: &Document, name: &str) -> u64 {
        doc.layers.iter().find(|l| l.name == name).unwrap().id
    }

    /// Names bottom first, with each layer's group in brackets.
    fn names(doc: &Document) -> Vec<String> {
        doc.layers
            .iter()
            .map(|l| match l.parent.and_then(|p| doc.layer(p)) {
                Some(p) => format!("{}({})", l.name, p.name),
                None => l.name.clone(),
            })
            .collect()
    }

    /// Every group's contents sit directly below it.
    fn check(doc: &Document) {
        for (i, layer) in doc.layers.iter().enumerate() {
            if let Some(p) = layer.parent {
                let g = doc.index_of(p).expect("parent exists");
                assert!(doc.layers[g].is_group, "{} is in a non-group", layer.name);
                assert!(doc.span(g).contains(&i), "{} is outside its group", layer.name);
            }
        }
    }

    #[test]
    fn grouping_puts_the_group_where_the_layer_was() {
        let doc = grouped();
        check(&doc);
        assert_eq!(names(&doc), ["bg", "a(g)", "b(g)", "g", "c"]);
        let g = doc.index_of(id(&doc, "g")).unwrap();
        assert_eq!(doc.span(g), 1..4);
        assert_eq!(doc.depth(id(&doc, "a")), 1);
        assert!(doc.is_inside(id(&doc, "b"), id(&doc, "g")));
        assert!(!doc.is_inside(id(&doc, "c"), id(&doc, "g")));
    }

    #[test]
    fn groups_nest() {
        let mut doc = grouped();
        let g = doc.index_of(id(&doc, "g")).unwrap();
        let outer = doc.group_layer(g);
        check(&doc);
        assert_eq!(doc.layer(outer).unwrap().name, "Group 1");
        assert_eq!(names(&doc), ["bg", "a(g)", "b(g)", "g(Group 1)", "Group 1", "c"]);
        assert_eq!(doc.span(doc.index_of(outer).unwrap()), 1..5);
        assert_eq!(doc.depth(id(&doc, "a")), 2);
        // A group can't go inside itself.
        assert!(!doc.move_layer(outer, Place::IntoTop(id(&doc, "g"))));
        assert!(!doc.move_layer(outer, Place::Above(id(&doc, "a"))));
    }

    #[test]
    fn moving_a_group_takes_its_contents() {
        let mut doc = grouped();
        let g = id(&doc, "g");
        assert!(doc.move_layer(g, Place::Below(id(&doc, "bg"))));
        check(&doc);
        assert_eq!(names(&doc), ["a(g)", "b(g)", "g", "bg", "c"]);
        assert!(doc.move_layer(g, Place::Above(id(&doc, "c"))));
        assert_eq!(names(&doc), ["bg", "c", "a(g)", "b(g)", "g"]);
        // Into a group, and back out.
        let c = id(&doc, "c");
        assert!(doc.move_layer(c, Place::IntoTop(g)));
        assert_eq!(names(&doc), ["bg", "a(g)", "b(g)", "c(g)", "g"]);
        assert!(!doc.move_layer(c, Place::IntoTop(id(&doc, "bg"))), "not a group");
        assert!(doc.move_layer(c, Place::Below(g)));
        assert_eq!(names(&doc), ["bg", "c", "a(g)", "b(g)", "g"]);
        check(&doc);
    }

    #[test]
    fn raising_and_lowering_step_in_and_out_of_groups() {
        let mut doc = grouped();
        let step = |doc: &mut Document, name: &str, up: bool| {
            let id = id(doc, name);
            let place = if up { doc.raise_place(id) } else { doc.lower_place(id) };
            assert!(doc.move_layer(id, place.expect("can move")));
            check(doc);
            names(doc)
        };
        // Up through the group: into its bottom, past a layer, out the top.
        assert_eq!(step(&mut doc, "bg", true), ["bg(g)", "a(g)", "b(g)", "g", "c"]);
        assert_eq!(step(&mut doc, "bg", true), ["a(g)", "bg(g)", "b(g)", "g", "c"]);
        step(&mut doc, "bg", true);
        assert_eq!(step(&mut doc, "bg", true), ["a(g)", "b(g)", "g", "bg", "c"]);
        // And back down.
        assert_eq!(step(&mut doc, "bg", false), ["a(g)", "b(g)", "bg(g)", "g", "c"]);
        // A group moves past a layer as a whole.
        assert_eq!(step(&mut doc, "g", true), ["c", "a(g)", "b(g)", "bg(g)", "g"]);
        assert_eq!(doc.raise_place(id(&doc, "g")), None);
        assert_eq!(step(&mut doc, "a", false), ["c", "a", "b(g)", "bg(g)", "g"]);
        assert_eq!(doc.lower_place(id(&doc, "c")), None);
    }

    #[test]
    fn bring_to_front_and_send_to_back_stay_within_group() {
        let mut doc = grouped();
        let c = id(&doc, "c");
        let bg = id(&doc, "bg");
        let g = id(&doc, "g");
        let a = id(&doc, "a");
        let b = id(&doc, "b");

        assert_eq!(doc.front_place(c), None, "c is already at top of root");
        assert_eq!(doc.back_place(bg), None, "bg is already at bottom of root");

        // Send "c" to back of root.
        assert_eq!(doc.back_place(c), Some(Place::Below(bg)));
        assert!(doc.move_layer(c, doc.back_place(c).unwrap()));
        check(&doc);
        assert_eq!(names(&doc), ["c", "bg", "a(g)", "b(g)", "g"]);

        // Bring "c" back to front of root.
        assert_eq!(doc.front_place(c), Some(Place::Above(g)));
        assert!(doc.move_layer(c, doc.front_place(c).unwrap()));
        check(&doc);
        assert_eq!(names(&doc), ["bg", "a(g)", "b(g)", "g", "c"]);

        // Send "g" to back of root.
        assert_eq!(doc.back_place(g), Some(Place::Below(bg)));
        assert!(doc.move_layer(g, doc.back_place(g).unwrap()));
        check(&doc);
        assert_eq!(names(&doc), ["a(g)", "b(g)", "g", "bg", "c"]);

        // Bring "g" to front of root.
        assert_eq!(doc.front_place(g), Some(Place::Above(c)));
        assert!(doc.move_layer(g, doc.front_place(g).unwrap()));
        check(&doc);
        assert_eq!(names(&doc), ["bg", "c", "a(g)", "b(g)", "g"]);

        // Inside group "g":
        // Currently inside "g": "a" (bottom), "b" (top).
        assert_eq!(doc.front_place(b), None, "b is already at top of g");
        assert_eq!(doc.back_place(a), None, "a is already at bottom of g");

        // Bring "a" to front of group.
        assert_eq!(doc.front_place(a), Some(Place::IntoTop(g)));
        assert!(doc.move_layer(a, doc.front_place(a).unwrap()));
        check(&doc);
        assert_eq!(names(&doc), ["bg", "c", "b(g)", "a(g)", "g"]);

        // Send "a" back to bottom of group.
        assert_eq!(doc.back_place(a), Some(Place::IntoBottom(g)));
        assert!(doc.move_layer(a, doc.back_place(a).unwrap()));
        check(&doc);
        assert_eq!(names(&doc), ["bg", "c", "a(g)", "b(g)", "g"]);

        // Several layers:
        // Move "g" back between "bg" and "c":
        assert!(doc.move_layer(g, Place::Below(c)));
        assert_eq!(names(&doc), ["bg", "a(g)", "b(g)", "g", "c"]);

        // Several in root: select "bg" and "c", bring to front.
        let place = doc.front_place_layers(&[bg, c], bg).unwrap();
        assert_eq!(place, Place::Above(c));
        assert!(doc.move_layers(&[bg, c], place));
        check(&doc);
        assert_eq!(names(&doc), ["a(g)", "b(g)", "g", "bg", "c"]);
        assert_eq!(doc.front_place_layers(&[bg, c], bg), None);

        // Send them both to back:
        let place = doc.back_place_layers(&[bg, c], bg).unwrap();
        assert_eq!(place, Place::Below(g));
        assert!(doc.move_layers(&[bg, c], place));
        check(&doc);
        assert_eq!(names(&doc), ["bg", "c", "a(g)", "b(g)", "g"]);
        assert_eq!(doc.back_place_layers(&[bg, c], bg), None);
    }

    #[test]
    fn new_layers_go_in_a_selected_group() {
        let mut doc = grouped();
        let g = doc.index_of(id(&doc, "g")).unwrap();
        let at = doc.insert_above(g, Layer::empty(99, "new", 4, 4));
        assert_eq!(at, 3);
        let b = doc.index_of(id(&doc, "b")).unwrap();
        doc.insert_above(b, Layer::empty(98, "next", 4, 4));
        check(&doc);
        assert_eq!(names(&doc), ["bg", "a(g)", "b(g)", "next(g)", "new(g)", "g", "c"]);
    }

    #[test]
    fn duplicating_a_group_copies_its_contents() {
        let mut doc = grouped();
        let g = doc.index_of(id(&doc, "g")).unwrap();
        let copy = doc.duplicate_layer(g);
        check(&doc);
        assert_eq!(doc.layer(copy).unwrap().name, "g copy");
        assert_eq!(
            names(&doc),
            ["bg", "a(g)", "b(g)", "g", "a(g copy)", "b(g copy)", "g copy", "c"]
        );
        let ids: std::collections::HashSet<u64> = doc.layers.iter().map(|l| l.id).collect();
        assert_eq!(ids.len(), doc.layers.len(), "every copy has a new id");
    }

    #[test]
    fn deleting_a_group_deletes_its_contents_and_ungrouping_keeps_them() {
        let mut doc = grouped();
        let g = doc.index_of(id(&doc, "g")).unwrap();
        let removed = doc.remove_layer(g);
        assert_eq!(removed.len(), 3);
        assert_eq!(names(&doc), ["bg", "c"]);

        let mut doc = grouped();
        let g = doc.index_of(id(&doc, "g")).unwrap();
        let outer = doc.group_layer(g);
        let top = doc.ungroup(doc.index_of(id(&doc, "g")).unwrap());
        check(&doc);
        assert_eq!(top, Some(id(&doc, "b")));
        assert_eq!(names(&doc), ["bg", "a(Group 1)", "b(Group 1)", "Group 1", "c"]);
        assert_eq!(doc.ungroup(doc.index_of(id(&doc, "a")).unwrap()), None, "not a group");
        doc.ungroup(doc.index_of(outer).unwrap());
        assert_eq!(names(&doc), ["bg", "a", "b", "c"]);
    }

    #[test]
    fn new_group_is_empty_and_selected_layers_can_move_in() {
        let mut doc = plain(&["bg", "a"]);
        let g = doc.new_group(1);
        assert_eq!(names(&doc), ["bg", "a", "Group 1"]);
        assert_eq!(doc.span(2), 2..3);
        assert!(doc.move_layer(id(&doc, "a"), doc.raise_place(id(&doc, "a")).unwrap()));
        assert_eq!(names(&doc), ["bg", "a(Group 1)", "Group 1"]);
        assert!(doc.layer(g).unwrap().is_group);
        check(&doc);
    }

    #[test]
    fn clipping_toggles_and_finds_its_base() {
        let mut doc = plain(&["bg", "a", "b", "c"]);
        let [bg, a, b, c] = ["bg", "a", "b", "c"].map(|n| id(&doc, n));
        assert!(!doc.toggle_clipping(bg), "nothing below to clip to");
        assert!(doc.toggle_clipping(b));
        assert!(doc.toggle_clipping(c));
        assert_eq!(doc.clip_base(c), Some(a));
        assert_eq!(doc.clip_base(b), Some(a));
        assert_eq!(doc.clip_base(a), None);
        assert!(doc.is_clip_base(a) && !doc.is_clip_base(b) && !doc.is_clip_base(bg));

        // A new layer inside the run is clipped too; below it, it isn't.
        let b_index = doc.index_of(b).unwrap();
        doc.insert_above(b_index, Layer::empty(50, "new", 4, 4));
        assert_eq!(doc.clip_base(50), Some(a));
        doc.insert_above(0, Layer::empty(51, "low", 4, 4));
        assert!(!doc.layer(51).unwrap().clipped);

        // Releasing one releases those above it, as in Photoshop.
        assert!(doc.toggle_clipping(50));
        assert!(doc.layer(b).unwrap().clipped);
        assert!(!doc.layer(50).unwrap().clipped && !doc.layer(c).unwrap().clipped);
    }

    #[test]
    fn clipping_stays_within_a_group() {
        let mut doc = grouped();
        let [a, b, g, c] = ["a", "b", "g", "c"].map(|n| id(&doc, n));
        assert!(!doc.toggle_clipping(a), "bottom of its group");
        assert!(doc.toggle_clipping(b));
        assert_eq!(doc.clip_base(b), Some(a));
        // Above a group, it clips to the group.
        assert!(doc.toggle_clipping(c));
        assert_eq!(doc.clip_base(c), Some(g));
        assert!(doc.is_clip_base(g) && doc.is_clip_base(a));
    }

    #[test]
    fn several_layers_group_move_duplicate_and_delete_together() {
        let mut doc = grouped();
        let [bg, a, g, c] = ["bg", "a", "g", "c"].map(|n| id(&doc, n));
        // A layer inside a selected group goes with the group.
        assert_eq!(doc.outermost(&[c, a, bg, g, 999]), [bg, g, c]);

        // Grouped in stack order, where the top one was.
        let mut grouping = doc.clone();
        let outer = grouping.group_layers(&[c, bg]).unwrap();
        check(&grouping);
        assert_eq!(grouping.layer(outer).unwrap().name, "Group 1");
        assert_eq!(names(&grouping), ["a(g)", "b(g)", "g", "bg(Group 1)", "c(Group 1)", "Group 1"]);
        // From inside a group and outside it, the group goes where the top
        // one was: at the top level.
        let mut grouping = doc.clone();
        grouping.group_layers(&[a, c]).unwrap();
        check(&grouping);
        assert_eq!(names(&grouping), ["bg", "b(g)", "g", "a(Group 1)", "c(Group 1)", "Group 1"]);

        // Moved as a block, keeping their order.
        let mut moving = doc.clone();
        assert!(moving.move_layers(&[bg, c], Place::IntoTop(g)));
        check(&moving);
        assert_eq!(names(&moving), ["a(g)", "b(g)", "bg(g)", "c(g)", "g"]);
        assert!(!moving.move_layers(&[bg, g], Place::Above(a)), "not into itself");
        assert!(moving.move_layers(&[a, c], Place::Below(g)));
        assert_eq!(names(&moving), ["a", "c", "b(g)", "bg(g)", "g"]);
        // Next to one of them, the others gather round it.
        assert!(moving.move_layers(&[a, id(&moving, "b")], Place::Above(a)));
        check(&moving);
        assert_eq!(names(&moving), ["a", "b", "c", "bg(g)", "g"]);

        let copies = doc.duplicate_layers(&[bg, g]);
        check(&doc);
        assert_eq!(copies.len(), 2);
        assert_eq!(
            names(&doc),
            ["bg", "bg copy", "a(g)", "b(g)", "g", "a(g copy)", "b(g copy)", "g copy", "c"]
        );
        assert_eq!(doc.layer(copies[1]).unwrap().name, "g copy");

        assert_eq!(doc.removed_count(&[g, a, copies[1]]), 6);
        assert_eq!(doc.remove_layers(&[g, copies[1]]), Some(1));
        assert_eq!(names(&doc), ["bg", "bg copy", "c"]);
    }
}
