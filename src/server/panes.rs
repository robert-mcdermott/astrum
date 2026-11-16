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

    /// Where `target` sits in the split it is a half of: side by side or
    /// stacked, whether it is the first half, the first half's share, and
    /// the panes of the other half.
    fn seat(&self, target: u64) -> Option<Seat> {
        let Node::Split { side_by_side, share, a, b } = self else { return None };
        if **a == Node::Leaf(target) {
            Some(Seat { side_by_side: *side_by_side, first: true, share: *share, beside: b.ids() })
        } else if **b == Node::Leaf(target) {
            Some(Seat { side_by_side: *side_by_side, first: false, share: *share, beside: a.ids() })
        } else {
            a.seat(target).or_else(|| b.seat(target))
        }
    }

    /// Put `new` back in the seat it had: a half of a split whose other
    /// half is the smallest part of the tree that holds all of `beside`
    /// (panes that are all here: the caller sees to it).
    fn seat_again(&mut self, new: u64, seat: &Seat, beside: &[u64]) {
        if let Node::Split { a, b, .. } = self {
            for half in [a, b] {
                let ids = half.ids();
                if beside.iter().all(|id| ids.contains(id)) {
                    return half.seat_again(new, seat, beside);
                }
            }
        }
        let (was, new) = (Box::new(self.clone()), Box::new(Node::Leaf(new)));
        let (a, b) = if seat.first { (new, was) } else { (was, new) };
        *self = Node::Split { side_by_side: seat.side_by_side, share: seat.share.clamp(50, 950), a, b };
    }

    /// The panes `a` and `b` change places.
    fn swap(&mut self, a: u64, b: u64) {
        match self {
            Node::Leaf(id) if *id == a => *id = b,
            Node::Leaf(id) if *id == b => *id = a,
            Node::Leaf(_) => {}
            Node::Split { a: first, b: second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
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
    /// A pane drawn over the others, if the tab has one.
    pub float: Option<Float>,
}

/// How a pane sat in its split: what a pane that floats goes back to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seat {
    /// The panes it shared the space with.
    #[serde(default)]
    pub beside: Vec<u64>,
    /// Side by side with its neighbour, or one above the other.
    pub side_by_side: bool,
    /// The first half (left, or top) or the second.
    pub first: bool,
    /// The first half's share of the space, in thousandths.
    pub share: u16,
}

/// A pane that floats over the tab's own: it is in the list of panes like
/// any other, and in no split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Float {
    pub id: u64,
    /// Where it is, frame and all, in thousandths of the tab's space:
    /// left, top, width, height.
    pub at: (u16, u16, u16, u16),
    /// Drawn, or put away (still running).
    pub shown: bool,
    /// The pane of the splits the keyboard was in before it came here,
    /// and goes back to.
    #[serde(default)]
    pub under: u64,
    /// How it sat in the splits, if it came out of them.
    #[serde(default)]
    pub seat: Option<Seat>,
}

/// Where a floating pane starts: about 60% of the tab, in the middle.
pub const FLOAT_START: (u16, u16, u16, u16) = (200, 180, 600, 640);
/// The smallest a floating pane may be made, frame and all.
const FLOAT_MIN: (u16, u16) = (24, 6);

impl Panes {
    pub fn new(pane: Pane) -> Self {
        let id = pane.id;
        Self { list: vec![pane], layout: Node::Leaf(id), focus: id, agent: id, zoomed: false, float: None }
    }

    /// Panes brought back from a saved session, laid out as they were.
    /// `layout` and `focus` speak of the panes by their old numbers; `ids`
    /// gives the new one for each. A layout that does not account for
    /// exactly these panes is replaced by a plain row of them.
    pub fn restored(list: Vec<Pane>, layout: &Node, ids: &HashMap<u64, u64>, focus: u64, zoomed: bool) -> Self {
        Self::restored_with(list, layout, ids, focus, zoomed, None)
    }

    /// The same, with the pane that floated over them, if one did (spoken
    /// of by its old number, as the others are).
    pub fn restored_with(list: Vec<Pane>, layout: &Node, ids: &HashMap<u64, u64>, focus: u64, zoomed: bool, float: Option<Float>) -> Self {
        // (Only if it is one of these panes, and not the only one.)
        let float = float.and_then(|f| Some(Float { id: *ids.get(&f.id)?, under: ids.get(&f.under).copied().unwrap_or(0), seat: f.seat.map(|s| Seat { beside: s.beside.iter().filter_map(|id| ids.get(id).copied()).collect(), ..s }), ..f })).filter(|f| list.len() > 1 && list.iter().any(|p| p.id == f.id));
        let floating = float.as_ref().map(|f| f.id);
        let mut have: Vec<u64> = list.iter().map(|p| p.id).filter(|id| Some(*id) != floating).collect();
        have.sort_unstable();
        let layout = layout
            .renumbered(ids)
            .filter(|node| {
                let mut used = node.ids();
                used.sort_unstable();
                used == have
            })
            .unwrap_or_else(|| {
                let mut node = Node::Leaf(have[0]);
                for id in &have[1..] {
                    node = Node::Split { side_by_side: true, share: 500, a: Box::new(node), b: Box::new(Node::Leaf(*id)) };
                }
                node
            });
        let tiled = ids.get(&focus).copied().filter(|id| have.contains(id)).unwrap_or(have[0]);
        let zoomed = zoomed && have.len() > 1;
        let float = float.map(|f| Float { under: if have.contains(&f.under) { f.under } else { tiled }, ..f });
        // (The keyboard was in the floating pane, if that was the one named.)
        let focus = match &float {
            Some(f) if f.shown && ids.get(&focus) == Some(&f.id) => f.id,
            _ => tiled,
        };
        Self { list, layout, focus, agent: focus, zoomed, float }
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

    /// Is this the pane that floats?
    pub fn floats(&self, id: u64) -> bool {
        self.float.as_ref().is_some_and(|f| f.id == id)
    }

    /// How many panes are in the splits (all but one that floats).
    pub fn tiled_len(&self) -> usize {
        self.list.len() - self.float.is_some() as usize
    }

    /// The pane of the splits the keyboard is in, or was in before it
    /// went to the floating pane.
    pub fn tiled_focus(&self) -> u64 {
        match &self.float {
            Some(f) if f.id == self.focus => f.under,
            _ => self.focus,
        }
    }

    /// Move the keyboard to a pane: one of the splits, or the floating one
    /// (which then remembers where the keyboard was).
    pub fn set_focus(&mut self, id: u64) {
        let was = self.tiled_focus();
        if let Some(f) = self.float.as_mut().filter(|f| f.id == id) {
            f.under = was;
            f.shown = true;
        }
        self.focus = id;
    }

    /// Where the floating pane is drawn in `area`, frame and all, while it
    /// is shown: kept inside the area, and no smaller than it can be used.
    pub fn float_rect(&self, area: Rect) -> Option<(u64, Rect)> {
        let f = self.float.as_ref().filter(|f| f.shown)?;
        let part = |of: u16, thousandths: u16| ((of as u32 * thousandths as u32 + 500) / 1000) as u16;
        let w = part(area.width, f.at.2).max(FLOAT_MIN.0).min(area.width);
        let h = part(area.height, f.at.3).max(FLOAT_MIN.1).min(area.height);
        let x = area.x + part(area.width, f.at.0).min(area.width - w);
        let y = area.y + part(area.height, f.at.1).min(area.height - h);
        (area.width >= 12 && area.height >= 4).then_some((f.id, Rect::new(x, y, w, h)))
    }

    /// The space inside a floating pane's frame.
    pub fn inside(frame: Rect) -> Rect {
        Rect::new(frame.x + 1, frame.y + 1, frame.width.saturating_sub(2), frame.height.saturating_sub(2))
    }

    /// Put the floating pane's frame at `to` within `area`.
    pub fn place_float(&mut self, area: Rect, to: Rect) {
        let Some(f) = self.float.as_mut() else { return };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let w = to.width.clamp(FLOAT_MIN.0.min(area.width), area.width);
        let h = to.height.clamp(FLOAT_MIN.1.min(area.height), area.height);
        let x = to.x.clamp(area.x, area.x + area.width - w) - area.x;
        let y = to.y.clamp(area.y, area.y + area.height - h) - area.y;
        let of = |n: u16, total: u16| ((n as u32 * 1000 + total as u32 / 2) / total as u32) as u16;
        f.at = (of(x, area.width), of(y, area.height), of(w, area.width), of(h, area.height));
    }

    /// A new pane, floating over the others, with the keyboard in it.
    /// False (and the pane dropped) if the tab has one already.
    pub fn open_float(&mut self, pane: Pane) -> bool {
        if self.float.is_some() {
            return false;
        }
        let id = pane.id;
        self.float = Some(Float { id, at: FLOAT_START, shown: true, under: self.focus, seat: None });
        self.list.push(pane);
        self.focus = id;
        true
    }

    /// The pane of the splits the keyboard is in comes out of them, to
    /// float. False if it is the only one there, or one floats already.
    pub fn float_this(&mut self) -> bool {
        if self.float.is_some() || self.tiled_len() < 2 {
            return false;
        }
        let id = self.focus;
        let seat = self.layout.seat(id);
        let Some(heir) = self.layout.remove(id) else { return false };
        self.float = Some(Float { id, at: FLOAT_START, shown: true, under: heir, seat });
        self.zoomed = false;
        true
    }

    /// The floating pane goes into the splits, beside the pane the
    /// keyboard came from.
    pub fn put_back(&mut self) {
        let Some(f) = self.float.take() else { return };
        // As it was split before it floated, beside whichever of its
        // neighbours are still here; one that began floating, or whose
        // neighbours have all gone, goes to the right of the pane the
        // keyboard came from.
        let here = self.layout.ids();
        let seat = f.seat.map(|s| Seat { beside: s.beside.iter().copied().filter(|id| here.contains(id)).collect(), ..s }).filter(|s| !s.beside.is_empty());
        match seat {
            Some(seat) => self.layout.seat_again(f.id, &seat, &seat.beside),
            None => {
                let beside = if here.contains(&f.under) { f.under } else { here[0] };
                self.layout.split(beside, f.id, true);
            }
        }
        self.focus = f.id;
        self.zoomed = false;
    }

    /// Two panes of the splits change places. False if either is not one.
    pub fn swap(&mut self, a: u64, b: u64) -> bool {
        let here = self.layout.ids();
        if a == b || !here.contains(&a) || !here.contains(&b) {
            return false;
        }
        self.layout.swap(a, b);
        self.zoomed = false;
        true
    }

    /// A pane of the splits is taken out of where it is and given the
    /// half of another's space on the side named. False if either is not
    /// a pane of the splits.
    pub fn move_beside(&mut self, id: u64, target: u64, side: Dir) -> bool {
        let here = self.layout.ids();
        if id == target || !here.contains(&id) || !here.contains(&target) || self.layout.remove(id).is_none() {
            return false;
        }
        let seat = Seat { side_by_side: matches!(side, Dir::Left | Dir::Right), first: matches!(side, Dir::Left | Dir::Up), share: 500, beside: vec![target] };
        self.layout.seat_again(id, &seat, &seat.beside);
        self.zoomed = false;
        true
    }

    /// Would a pane fit in half of `rect`, on that side of it?
    pub fn room_beside(rect: Rect, side: Dir) -> bool {
        Node::halves(rect, matches!(side, Dir::Left | Dir::Right), 500).is_some()
    }

    /// Show the floating pane, with the keyboard in it, or put it away:
    /// it goes on running, and the keyboard goes back where it was.
    pub fn show_float(&mut self, shown: bool) {
        let Some(id) = self.float.as_ref().map(|f| f.id) else { return };
        if shown {
            self.set_focus(id);
        } else {
            let under = self.tiled_focus();
            if let Some(f) = self.float.as_mut() {
                f.shown = false;
            }
            self.focus = under;
        }
    }

    /// Add a pane beside the focused one and move the keyboard to it.
    pub fn add(&mut self, pane: Pane, side_by_side: bool) {
        let id = pane.id;
        // (Beside the one in the splits: a floating pane is in none.)
        self.layout.split(self.tiled_focus(), id, side_by_side);
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
        // The floating pane goes, and the keyboard back where it was.
        if self.floats(id) {
            let under = self.tiled_focus();
            self.float = None;
            self.zoomed = self.zoomed && self.list.len() > 2;
            self.list.retain(|p| p.id != id);
            if self.focus == id {
                self.focus = under;
            }
            if self.agent == id {
                self.agent = self.focus;
            }
            return true;
        }
        // The last pane of the splits goes while one floats: that one
        // takes the tab.
        if self.tiled_len() == 1 {
            let Some(f) = self.float.take() else { return false };
            self.layout = Node::Leaf(f.id);
            self.list.retain(|p| p.id != id);
            self.focus = f.id;
            if self.agent == id {
                self.agent = f.id;
            }
            self.zoomed = false;
            return true;
        }
        let Some(heir) = self.layout.remove(id) else { return false };
        if let Some(f) = self.float.as_mut().filter(|f| f.under == id) {
            f.under = heir;
        }
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
        // (The panes of the splits: one that floats is over them, wherever
        // `float_rect` says.)
        if self.zoomed || self.tiled_len() == 1 {
            return (vec![(self.tiled_focus(), area)], Vec::new());
        }
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        self.layout.place(area, self.tiled_focus(), &mut 0, &mut panes, &mut dividers);
        (panes, dividers)
    }

    /// Would the focused pane still be usable if split this way in `area`?
    pub fn room_to_split(&self, area: Rect, side_by_side: bool) -> bool {
        let tiled = self.tiled_focus();
        let rect = self.place(area).0.into_iter().find(|(id, _)| *id == tiled).map(|(_, r)| r).unwrap_or(area);
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
    fn a_pane_taken_out_goes_back_as_it_was_split() {
        // 1 above (3 beside 2), the top a third.
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, false);
        if let Node::Split { share, .. } = &mut tree {
            *share = 300;
        }
        tree.seat_again(3, &Seat { side_by_side: true, first: true, share: 500, beside: vec![2] }, &[2]);
        let whole = tree.clone();
        for id in [1, 2, 3] {
            let seat = tree.seat(id).unwrap();
            tree.remove(id).unwrap();
            tree.seat_again(id, &seat, &seat.beside);
            assert_eq!(tree, whole, "pane {id}");
        }
        assert_eq!(tree.seat(1), Some(Seat { side_by_side: false, first: true, share: 300, beside: vec![3, 2] }));
        // One of its neighbours has gone meanwhile: beside the other.
        let seat = tree.seat(1).unwrap();
        tree.remove(1).unwrap();
        tree.remove(3).unwrap();
        tree.seat_again(1, &seat, &[2]);
        assert_eq!(tree, Node::Split { side_by_side: false, share: 300, a: Box::new(Node::Leaf(1)), b: Box::new(Node::Leaf(2)) });
    }

    #[test]
    fn panes_change_places_and_are_moved_beside_one_another() {
        let leaf = |id| Box::new(Node::Leaf(id));
        // 1 beside (2 above 3).
        let mut tree = Node::Leaf(1);
        tree.split(1, 2, true);
        tree.split(2, 3, false);
        tree.swap(1, 3);
        assert_eq!(tree.ids(), vec![3, 2, 1]);
        tree.swap(1, 3);
        assert_eq!(tree.ids(), vec![1, 2, 3]);
        // 3 goes to the left of 1: (3 beside 1) beside 2.
        tree.remove(3).unwrap();
        tree.seat_again(3, &Seat { side_by_side: true, first: true, share: 500, beside: vec![1] }, &[1]);
        assert_eq!(tree, Node::Split { side_by_side: true, share: 500, a: Box::new(Node::Split { side_by_side: true, share: 500, a: leaf(3), b: leaf(1) }), b: leaf(2) });
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
