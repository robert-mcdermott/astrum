//! The panes of one tab: the shells themselves and how the tab's space is
//! divided among them.
//!
//! The division is a tree. A leaf is a pane; a branch splits its area in
//! two, side by side or one above the other, with a one-cell divider
//! between the halves.

use crate::pane::Pane;
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A divider between two halves of a split, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    pub rect: Rect,
    /// Which split it belongs to: its position among the tree's splits.
    pub split: usize,
    /// A vertical line (between side-by-side halves) or a horizontal one.
    pub vertical: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Leaf(u64),
    Split {
        /// Side by side (a vertical divider) or stacked (a horizontal one).
        side_by_side: bool,
        /// Share of the space given to the first half, in thousandths.
        share: u16,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// The smallest a pane may get, so that it stays usable.
const MIN_COLS: u16 = 8;
const MIN_ROWS: u16 = 2;

impl Node {
    /// How a split's area divides: first half, divider, second half.
    /// `None` when the area is too small to hold two panes and a divider.
    fn halves(area: Rect, side_by_side: bool, share: u16) -> Option<(Rect, Rect, Rect)> {
        let (total, least) = if side_by_side { (area.width, MIN_COLS) } else { (area.height, MIN_ROWS) };
        if total < 2 * least + 1 {
            return None;
        }
        let first = (((total as u32 - 1) * share as u32 + 500) / 1000) as u16;
        let first = first.clamp(least, total - 1 - least);
        Some(if side_by_side {
            (
                Rect::new(area.x, area.y, first, area.height),
                Rect::new(area.x + first, area.y, 1, area.height),
                Rect::new(area.x + first + 1, area.y, total - first - 1, area.height),
            )
        } else {
            (
                Rect::new(area.x, area.y, area.width, first),
                Rect::new(area.x, area.y + first, area.width, 1),
                Rect::new(area.x, area.y + first + 1, area.width, total - first - 1),
            )
        })
    }

    /// Where each pane and each divider goes within `area`. A split whose
    /// area has become too small shows only one of its halves: the one
    /// with the pane the keyboard is in (`focus`), else the first.
    fn place(&self, area: Rect, focus: u64, next_split: &mut usize, panes: &mut Vec<(u64, Rect)>, dividers: &mut Vec<Divider>) {
        match self {
            Node::Leaf(id) => panes.push((*id, area)),
            Node::Split { side_by_side, share, a, b } => {
                let split = *next_split;
                *next_split += 1;
                match Node::halves(area, *side_by_side, *share) {
                    Some((first, bar, second)) => {
                        a.place(first, focus, next_split, panes, dividers);
                        dividers.push(Divider { rect: bar, split, vertical: *side_by_side });
                        b.place(second, focus, next_split, panes, dividers);
                    }
                    // (Keeping the numbering of the splits that are not shown.)
                    None if b.ids().contains(&focus) => {
                        *next_split += a.splits();
                        b.place(area, focus, next_split, panes, dividers);
                    }
                    None => {
                        a.place(area, focus, next_split, panes, dividers);
                        *next_split += b.splits();
                    }
                }
            }
        }
    }

    fn splits(&self) -> usize {
        match self {
            Node::Leaf(_) => 0,
            Node::Split { a, b, .. } => 1 + a.splits() + b.splits(),
        }
    }

    pub fn ids(&self) -> Vec<u64> {
        match self {
            Node::Leaf(id) => vec![*id],
            Node::Split { a, b, .. } => [a.ids(), b.ids()].concat(),
        }
    }

    /// The same tree with its panes renumbered. `None` if one of them has
    /// no new number.
    fn renumbered(&self, ids: &HashMap<u64, u64>) -> Option<Node> {
        Some(match self {
            Node::Leaf(id) => Node::Leaf(*ids.get(id)?),
            Node::Split { side_by_side, share, a, b } => Node::Split {
                side_by_side: *side_by_side,
                share: (*share).clamp(1, 999),
                a: Box::new(a.renumbered(ids)?),
                b: Box::new(b.renumbered(ids)?),
            },
        })
    }

    /// Put `new` next to `target`, sharing its space equally.
    fn split(&mut self, target: u64, new: u64, side_by_side: bool) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split { side_by_side, share: 500, a: Box::new(Node::Leaf(target)), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new, side_by_side) || b.split(target, new, side_by_side),
        }
    }

    /// Take `target` out; its sibling takes over the space of both. Returns
    /// the pane that now occupies where it was, if `target` was found (and
    /// was not the only pane).
    fn remove(&mut self, target: u64) -> Option<u64> {
        let Node::Split { a, b, .. } = self else { return None };
        let heir = if **a == Node::Leaf(target) {
            Some((**b).clone())
        } else if **b == Node::Leaf(target) {
            Some((**a).clone())
        } else {
            None
        };
        match heir {
            Some(node) => {
                let first = node.ids().first().copied();
                *self = node;
                first
            }
            None => a.remove(target).or_else(|| b.remove(target)),
        }
    }

    /// Move the divider of split number `split` to the cell at (`col`,
    /// `row`), given the area the whole tree occupies.
    fn drag(&mut self, area: Rect, split: usize, col: u16, row: u16, next_split: &mut usize) -> bool {
        let Node::Split { side_by_side, share, a, b } = self else { return false };
        let mine = *next_split;
        *next_split += 1;
        if mine == split {
            let (start, total, at) = if *side_by_side { (area.x, area.width, col) } else { (area.y, area.height, row) };
            if total > 1 {
                let offset = at.saturating_sub(start).min(total - 1) as u32;
                *share = ((offset * 1000 + (total as u32 - 1) / 2) / (total as u32 - 1)).clamp(50, 950) as u16;
            }
            return true;
        }
        match Node::halves(area, *side_by_side, *share) {
            Some((first, _, second)) => a.drag(first, split, col, row, next_split) || b.drag(second, split, col, row, next_split),
            None => a.drag(area, split, col, row, next_split),
        }
    }
}

pub struct Panes {
    list: Vec<Pane>,
    layout: Node,
    /// The pane the keyboard goes to.
    pub focus: u64,
    /// The pane the agent works in: the focused one, except that it stays
    /// put for the length of a turn while the user moves about.
    pub agent: u64,
    /// The focused pane fills the tab; the others are hidden, still running.
    pub zoomed: bool,
}

impl Panes {
    pub fn new(pane: Pane) -> Self {
        let id = pane.id;
        Self { list: vec![pane], layout: Node::Leaf(id), focus: id, agent: id, zoomed: false }
    }

    /// Panes brought back from a saved session, laid out as they were.
    /// `layout` and `focus` speak of the panes by their old numbers; `ids`
    /// gives the new one for each. A layout that does not account for
    /// exactly these panes is replaced by a plain row of them.
    pub fn restored(list: Vec<Pane>, layout: &Node, ids: &HashMap<u64, u64>, focus: u64, zoomed: bool) -> Self {
        let mut have: Vec<u64> = list.iter().map(|p| p.id).collect();
        have.sort_unstable();
        let layout = layout
            .renumbered(ids)
            .filter(|node| {
                let mut used = node.ids();
                used.sort_unstable();
                used == have
            })
            .unwrap_or_else(|| {
                let mut node = Node::Leaf(list[0].id);
                for pane in &list[1..] {
                    node = Node::Split { side_by_side: true, share: 500, a: Box::new(node), b: Box::new(Node::Leaf(pane.id)) };
                }
                node
            });
        let focus = ids.get(&focus).copied().filter(|id| have.contains(id)).unwrap_or(list[0].id);
        let zoomed = zoomed && list.len() > 1;
        Self { list, layout, focus, agent: focus, zoomed }
    }

    pub fn layout(&self) -> &Node {
        &self.layout
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Pane> {
        self.list.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        self.list.iter_mut()
    }

    pub fn get(&self, id: u64) -> Option<&Pane> {
        self.list.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Pane> {
        self.list.iter_mut().find(|p| p.id == id)
    }

    fn index_or_first(&self, id: u64) -> usize {
        self.list.iter().position(|p| p.id == id).unwrap_or(0)
    }

    pub fn focused(&self) -> &Pane {
        &self.list[self.index_or_first(self.focus)]
    }

    pub fn focused_mut(&mut self) -> &mut Pane {
        let i = self.index_or_first(self.focus);
        &mut self.list[i]
    }

    pub fn agent(&self) -> &Pane {
        &self.list[self.index_or_first(self.agent)]
    }

    pub fn agent_mut(&mut self) -> &mut Pane {
        let i = self.index_or_first(self.agent);
        &mut self.list[i]
    }

    /// Add a pane beside the focused one and move the keyboard to it.
    pub fn add(&mut self, pane: Pane, side_by_side: bool) {
        let id = pane.id;
        self.layout.split(self.focus, id, side_by_side);
        self.list.push(pane);
        self.focus = id;
        self.zoomed = false;
    }

    /// Take a pane out (its shell ends when it is dropped). The keyboard,
    /// and the agent, move to the pane that takes over its space. Returns
    /// false if it was not here, or is the last one (close the tab instead).
    pub fn remove(&mut self, id: u64) -> bool {
        if self.list.len() < 2 || self.get(id).is_none() {
            return false;
        }
        let Some(heir) = self.layout.remove(id) else { return false };
        self.list.retain(|p| p.id != id);
        if self.focus == id {
            self.focus = heir;
        }
        if self.agent == id {
            self.agent = self.focus;
        }
        self.zoomed = false;
        true
    }

    /// Where everything goes in `area`: the panes that are shown, and the
    /// dividers between them.
    pub fn place(&self, area: Rect) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        if self.zoomed || self.list.len() == 1 {
            return (vec![(self.focused().id, area)], Vec::new());
        }
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        self.layout.place(area, self.focus, &mut 0, &mut panes, &mut dividers);
        (panes, dividers)
    }

    /// Would the focused pane still be usable if split this way in `area`?
    pub fn room_to_split(&self, area: Rect, side_by_side: bool) -> bool {
        let rect = self.place(area).0.into_iter().find(|(id, _)| *id == self.focus).map(|(_, r)| r).unwrap_or(area);
        Node::halves(rect, side_by_side, 500).is_some()
    }

    /// The pane next to the focused one in a direction: the one sharing
    /// the most of that edge.
    pub fn neighbour(&self, area: Rect, dir: Dir) -> Option<u64> {
        let (panes, _) = self.place(area);
        let from = panes.iter().find(|(id, _)| *id == self.focus)?.1;
        let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a1.min(b1).saturating_sub(a0.max(b0));
        panes
            .iter()
            .filter(|(id, _)| *id != self.focus)
            .filter_map(|(id, r)| {
                // Across the divider, which is one cell wide.
                let (touching, shared) = match dir {
                    Dir::Left => (r.x + r.width + 1 == from.x, overlap(r.y, r.y + r.height, from.y, from.y + from.height)),
                    Dir::Right => (from.x + from.width + 1 == r.x, overlap(r.y, r.y + r.height, from.y, from.y + from.height)),
                    Dir::Up => (r.y + r.height + 1 == from.y, overlap(r.x, r.x + r.width, from.x, from.x + from.width)),
                    Dir::Down => (from.y + from.height + 1 == r.y, overlap(r.x, r.x + r.width, from.x, from.x + from.width)),
                };
                (touching && shared > 0).then_some((*id, shared))
            })
            .max_by_key(|(_, shared)| *shared)
            .map(|(id, _)| id)
    }

    pub fn drag_divider(&mut self, area: Rect, split: usize, col: u16, row: u16) {
        self.layout.drag(area, split, col, row, &mut 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(node: &Node, area: Rect) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        placed_for(node, area, 0)
    }

    fn placed_for(node: &Node, area: Rect, focus: u64) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        node.place(area, focus, &mut 0, &mut panes, &mut dividers);
        (panes, dividers)
    }

    #[test]
    fn a_split_too_small_for_both_shows_the_pane_the_keyboard_is_in() {
        // 1 above (2 beside 3); four rows cannot hold two panes and a divider.
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, false);
        tree.split(2, 3, true);
        let small = Rect::new(0, 1, 40, 4);
        assert_eq!(placed_for(&tree, small, 1).0, vec![(1, small)]);
        // With the keyboard in the lower half, that half is what is shown,
        // and its own divider keeps its number.
        let (panes, dividers) = placed_for(&tree, small, 3);
        assert_eq!(panes.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(dividers.len(), 1);
        assert_eq!(dividers[0].split, 1);
        assert!(dividers[0].vertical);
    }

    #[test]
    fn splitting_shares_the_space_and_leaves_a_divider() {
        let mut tree = Node::Leaf(1);
        assert!(tree.split(1, 2, true));
        let (panes, dividers) = placed(&tree, Rect::new(0, 1, 81, 20));
        assert_eq!(panes, vec![(1, Rect::new(0, 1, 40, 20)), (2, Rect::new(41, 1, 40, 20))]);
        assert_eq!(dividers, vec![Divider { rect: Rect::new(40, 1, 1, 20), split: 0, vertical: true }]);

        // Split the right half again, stacked this time.
        assert!(tree.split(2, 3, false));
        let (panes, dividers) = placed(&tree, Rect::new(0, 1, 81, 21));
        assert_eq!(panes[1], (2, Rect::new(41, 1, 40, 10)));
        assert_eq!(panes[2], (3, Rect::new(41, 12, 40, 10)));
        assert_eq!(dividers[1], Divider { rect: Rect::new(41, 11, 40, 1), split: 1, vertical: false });
        assert_eq!(tree.ids(), vec![1, 2, 3]);
        assert!(!tree.split(9, 4, true));
    }

    #[test]
    fn closing_a_pane_gives_its_space_to_its_sibling() {
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, true);
        tree.split(2, 3, false);
        // Closing 3 leaves 2 with the whole right half.
        assert_eq!(tree.remove(3), Some(2));
        assert_eq!(placed(&tree, Rect::new(0, 0, 81, 20)).0, vec![(1, Rect::new(0, 0, 40, 20)), (2, Rect::new(41, 0, 40, 20))]);
        // Closing 1 leaves 2 with everything.
        assert_eq!(tree.remove(1), Some(2));
        assert_eq!(tree, Node::Leaf(2));
        // The last pane is not removed this way.
        assert_eq!(tree.remove(2), None);
    }

    #[test]
    fn a_divider_follows_the_pointer_within_limits() {
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, true);
        let area = Rect::new(0, 0, 101, 20);
        assert!(tree.drag(area, 0, 25, 5, &mut 0));
        let (panes, dividers) = placed(&tree, area);
        assert_eq!((panes[0].1.width, dividers[0].rect.x), (25, 25));
        // Dragged right off the edge: the other pane keeps a usable width.
        tree.drag(area, 0, 200, 5, &mut 0);
        let (panes, _) = placed(&tree, area);
        assert_eq!(panes[1].1.width, MIN_COLS);
        assert!(!tree.drag(area, 7, 10, 10, &mut 0));

        // In a nested split, each divider moves within its own area.
        tree.drag(area, 0, 50, 5, &mut 0);
        tree.split(2, 3, false);
        assert!(tree.drag(area, 1, 60, 5, &mut 0));
        let (panes, dividers) = placed(&tree, area);
        assert_eq!((panes[1].1.height, dividers[1].rect.y), (5, 5));
        assert_eq!(panes[0].1.width, 50, "the outer divider did not move");
    }

    #[test]
    fn an_area_too_small_to_split_shows_one_pane() {
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, true);
        let (panes, dividers) = placed(&tree, Rect::new(0, 0, 10, 5));
        assert_eq!(panes, vec![(1, Rect::new(0, 0, 10, 5))]);
        assert!(dividers.is_empty());
        assert!(Node::halves(Rect::new(0, 0, 17, 5), true, 500).is_some());
        assert!(Node::halves(Rect::new(0, 0, 16, 5), true, 500).is_none());
        assert!(Node::halves(Rect::new(0, 0, 80, 4), false, 500).is_none());
    }
}
