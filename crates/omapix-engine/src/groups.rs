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
    /// it's a group, as Photoshop does. Returns where it went.
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
        at
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
}
