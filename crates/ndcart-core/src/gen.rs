//! Battle-map generators (plan §10): dungeon, cave, building, deck and city
//! district. Each is a pure function of `(kind, seed, size)` that returns
//! ordinary [`Op`]s, so a generated map is one undoable step and can be edited
//! like anything else. No floats in the layout and a self-contained generator
//! (splitmix64), so the same seed gives the same map on every platform.

use crate::doc::Label;
use crate::error::{CoreError, CoreResult};
use crate::grid::{CellKey, Edge, EdgeKey, Mark, MarkKind, Space, SpaceKind};
use crate::journal::Op;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIN_SIDE: i32 = 16;
pub const MAX_SIDE: i32 = 120;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum GenKind {
    Dungeon,
    Cave,
    Building,
    Deck,
    District,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GenParams {
    pub kind: GenKind,
    pub seed: u64,
    pub width: i32,
    pub height: i32,
}

pub(crate) struct Rng(pub(crate) u64);
impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `lo..=hi`.
    pub(crate) fn range(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next() % (hi - lo + 1) as u64) as i32
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, items: &'a [&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

pub(crate) const PALETTE: [&str; 8] = [
    "#7f5539", "#6d597a", "#457b9d", "#8d6a3f", "#5e7c5a", "#9a5b5b", "#4f6d7a", "#7a6c4f",
];

/// A drawing surface: one space index per cell (0 = nothing).
pub(crate) struct Canvas {
    pub(crate) w: i32,
    pub(crate) h: i32,
    pub(crate) cells: Vec<u16>,
    pub(crate) spaces: Vec<Space>,
    doors: BTreeMap<(i32, i32, u8), MarkKind>,
    /// Room colours, in order (empty = the built-in palette).
    pub(crate) palette: Vec<String>,
    pub(crate) lights: Vec<(i32, i32, f64)>,
    pub(crate) labels: Vec<(f64, f64, String)>,
}

impl Canvas {
    pub(crate) fn new(w: i32, h: i32) -> Self {
        Canvas {
            w,
            h,
            cells: vec![0; (w * h) as usize],
            spaces: Vec::new(),
            doors: BTreeMap::new(),
            palette: Vec::new(),
            lights: Vec::new(),
            labels: Vec::new(),
        }
    }
    /// The i-th room colour: the project's palette, else the built-in one.
    pub(crate) fn pal(&self, i: usize) -> String {
        if self.palette.is_empty() {
            PALETTE[i % PALETTE.len()].to_string()
        } else {
            self.palette[i % self.palette.len()].clone()
        }
    }
    pub(crate) fn space(&mut self, name: &str, color: &str, kind: SpaceKind) -> u16 {
        let id = format!("g{}", self.spaces.len() + 1);
        self.spaces.push(Space {
            id,
            name: name.into(),
            color: color.into(),
            kind,
            material: None,
        tint: None,
        });
        self.spaces.len() as u16
    }
    pub(crate) fn at(&self, x: i32, y: i32) -> u16 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            0
        } else {
            self.cells[(y * self.w + x) as usize]
        }
    }
    pub(crate) fn set(&mut self, x: i32, y: i32, s: u16) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            self.cells[(y * self.w + x) as usize] = s;
        }
    }
    /// Fill `x0..x1` × `y0..y1` (end exclusive); only empty cells unless `over`.
    pub(crate) fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, s: u16, over: bool) {
        for y in y0..y1 {
            for x in x0..x1 {
                if over || self.at(x, y) == 0 {
                    self.set(x, y, s);
                }
            }
        }
    }
    /// A door on the edge from cell `(x, y)` towards `(tx, ty)`, which must be a neighbour.
    pub(crate) fn door(&mut self, x: i32, y: i32, tx: i32, ty: i32) {
        self.mark(x, y, tx, ty, MarkKind::Door);
    }
    /// Any mark (door, secret door, opening) on that edge.
    pub(crate) fn mark(&mut self, x: i32, y: i32, tx: i32, ty: i32, kind: MarkKind) {
        let e = match (tx - x, ty - y) {
            (1, 0) => 1,
            (-1, 0) => 3,
            (0, 1) => 2,
            _ => 0,
        };
        self.doors.insert((x, y, e), kind);
    }
    pub(crate) fn count(&self, s: u16) -> usize {
        self.cells.iter().filter(|c| **c == s).count()
    }
    pub(crate) fn centre(&self, s: u16) -> Option<(i32, i32)> {
        let mut pts = Vec::new();
        for y in 0..self.h {
            for x in 0..self.w {
                if self.at(x, y) == s {
                    pts.push((x, y));
                }
            }
        }
        let n = pts.len() as i64;
        if n == 0 {
            return None;
        }
        let (cx, cy) = (
            pts.iter().map(|p| p.0 as i64).sum::<i64>() / n,
            pts.iter().map(|p| p.1 as i64).sum::<i64>() / n,
        );
        // The painted cell nearest the mean, so a label never lands in a wall.
        pts.into_iter()
            .min_by_key(|(x, y)| (*x as i64 - cx).pow(2) + (*y as i64 - cy).pow(2))
    }
    pub(crate) fn label_room(&mut self, s: u16, light_over: usize) {
        if let Some((cx, cy)) = self.centre(s) {
            let name = self.spaces[(s - 1) as usize].name.clone();
            self.labels.push((cx as f64 + 0.5, cy as f64 + 0.5, name));
            if self.count(s) >= light_over {
                self.lights.push((cx, cy, 5.0));
            }
        }
    }

    pub(crate) fn into_ops(self) -> Vec<Op> {
        let mut ops = Vec::new();
        let mut used: Vec<u16> = Vec::new();
        for s in 1..=self.spaces.len() as u16 {
            if self.count(s) > 0 {
                used.push(s);
            }
        }
        for s in &used {
            ops.push(Op::UpsertSpace {
                space: self.spaces[(*s - 1) as usize].clone(),
            });
        }
        for s in &used {
            let cells: Vec<CellKey> = (0..self.h)
                .flat_map(|y| (0..self.w).map(move |x| (x, y)))
                .filter(|(x, y)| self.at(*x, *y) == *s)
                .map(|(x, y)| CellKey { x, y })
                .collect();
            ops.push(Op::PaintCells {
                cells,
                space: Some(self.spaces[(*s - 1) as usize].id.clone()),
            });
        }
        for ((x, y, e), kind) in &self.doors {
            let edge = [Edge::North, Edge::East, Edge::South, Edge::West][*e as usize];
            ops.push(Op::SetMark {
                edge: EdgeKey {
                    cell: CellKey { x: *x, y: *y },
                    edge,
                },
                mark: Some(Mark::new(*kind)),
            });
        }
        for (i, (x, y, range)) in self.lights.iter().enumerate() {
            ops.push(Op::AddLight {
                light: crate::doc::Light {
                    id: format!("gl{}", i + 1),
                    x: *x as f64 * 32.0 + 16.0,
                    y: *y as f64 * 32.0 + 16.0,
                    range: *range,
                    color: "#ffb35c".into(),
                    intensity: 0.8,
                    on: true,
                    label: None,
                },
            });
        }
        for (i, (x, y, text)) in self.labels.iter().enumerate() {
            let mut l = Label::new(&format!("gt{}", i + 1), *x, *y, text);
            l.size = 0.6;
            ops.push(Op::UpsertLabel { label: l });
        }
        vec![Op::Batch { ops }]
    }
}

/// Generate a map as one batch op.
pub fn generate(p: &GenParams) -> CoreResult<Vec<Op>> {
    generate_styled(p, &[])
}

/// Like [`generate`], painting rooms from `palette` (the project's style) when given.
pub fn generate_styled(p: &GenParams, palette: &[String]) -> CoreResult<Vec<Op>> {
    if !(MIN_SIDE..=MAX_SIDE).contains(&p.width) || !(MIN_SIDE..=MAX_SIDE).contains(&p.height) {
        return Err(CoreError::Invalid(format!(
            "width and height must be {MIN_SIDE}-{MAX_SIDE} squares"
        )));
    }
    let mut rng = Rng(p.seed ^ 0xD1B5_4A32_D192_ED03);
    let mut c = Canvas::new(p.width, p.height);
    c.palette = palette.to_vec();
    match p.kind {
        GenKind::Dungeon => dungeon(&mut c, &mut rng),
        GenKind::Cave => cave(&mut c, &mut rng),
        GenKind::Building => building(&mut c, &mut rng, false),
        GenKind::Deck => building(&mut c, &mut rng, true),
        GenKind::District => district(&mut c, &mut rng),
    }
    if c.cells.iter().all(|s| *s == 0) {
        return Err(CoreError::Invalid(
            "nothing fitted in that size; try a bigger map".into(),
        ));
    }
    Ok(c.into_ops())
}

const DUNGEON_NAMES: [&str; 12] = [
    "Guard Room",
    "Armory",
    "Crypt",
    "Storeroom",
    "Shrine",
    "Barracks",
    "Cistern",
    "Library",
    "Vault",
    "Kitchen",
    "Throne Room",
    "Cell Block",
];

fn dungeon(c: &mut Canvas, rng: &mut Rng) {
    let want = ((c.w * c.h) / 110).clamp(3, 30);
    let mut rects: Vec<(i32, i32, i32, i32)> = Vec::new();
    for _ in 0..want * 12 {
        if rects.len() as i32 >= want {
            break;
        }
        let (w, h) = (rng.range(4, 9), rng.range(4, 8));
        if w + 2 >= c.w || h + 2 >= c.h {
            continue;
        }
        let (x, y) = (rng.range(1, c.w - w - 1), rng.range(1, c.h - h - 1));
        // One empty cell around every room, so corridors can run between them.
        let clash = rects
            .iter()
            .any(|r| x < r.2 + 1 && x + w > r.0 - 1 && y < r.3 + 1 && y + h > r.1 - 1);
        if !clash {
            rects.push((x, y, x + w, y + h));
        }
    }
    rects.sort();
    let room_ids: Vec<u16> = rects
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let name = if i == 0 {
                "Entrance".to_string()
            } else {
                rng.pick(&DUNGEON_NAMES).to_string()
            };
            let s = c.space(&name, &c.pal(i), SpaceKind::Room);
            c.rect(r.0, r.1, r.2, r.3, s, true);
            s
        })
        .collect();
    let corridor = c.space("Corridor", "#4a4e57", SpaceKind::Corridor);
    // Join each room to the next, then a few extra links so there are loops.
    let mut links: Vec<(usize, usize)> = (1..rects.len()).map(|i| (i - 1, i)).collect();
    for _ in 0..rects.len() / 4 {
        let (a, b) = (
            rng.range(0, rects.len() as i32 - 1) as usize,
            rng.range(0, rects.len() as i32 - 1) as usize,
        );
        if a != b {
            links.push((a, b));
        }
    }
    for (a, b) in links {
        let (ra, rb) = (rects[a], rects[b]);
        let (ax, ay) = ((ra.0 + ra.2) / 2, (ra.1 + ra.3) / 2);
        let (bx, by) = ((rb.0 + rb.2) / 2, (rb.1 + rb.3) / 2);
        // Horizontal first, then vertical.
        let mut path = vec![(ax, ay)];
        let (mut x, mut y) = (ax, ay);
        while x != bx {
            x += (bx - x).signum();
            path.push((x, y));
        }
        while y != by {
            y += (by - y).signum();
            path.push((x, y));
        }
        for w in path.windows(2) {
            let ((px, py), (qx, qy)) = (w[0], w[1]);
            let (sp, sq) = (c.at(px, py), c.at(qx, qy));
            let is_room = |s: u16| room_ids.contains(&s);
            if sq == 0 {
                c.set(qx, qy, corridor);
            }
            let (sq2, sp2) = (c.at(qx, qy), sp);
            // Crossing a room boundary (in or out) gets a door.
            if is_room(sp2) != is_room(sq2) || (is_room(sp2) && is_room(sq2) && sp2 != sq2) {
                c.door(px, py, qx, qy);
            }
        }
    }
    for s in room_ids {
        c.label_room(s, 20);
    }
}

fn cave(c: &mut Canvas, rng: &mut Rng) {
    let (w, h) = (c.w, c.h);
    let mut alive = vec![false; (w * h) as usize];
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            alive[(y * w + x) as usize] = !rng.chance(45);
        }
    }
    for _ in 0..5 {
        let mut next = alive.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut n = 0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if alive[((y + dy) * w + x + dx) as usize] {
                            n += 1;
                        }
                    }
                }
                next[(y * w + x) as usize] = n >= 5;
            }
        }
        alive = next;
    }
    // Keep only the biggest connected cavern.
    let mut best: Vec<usize> = Vec::new();
    let mut seen = vec![false; alive.len()];
    for start in 0..alive.len() {
        if !alive[start] || seen[start] {
            continue;
        }
        let mut comp = vec![start];
        seen[start] = true;
        let mut i = 0;
        while i < comp.len() {
            let (x, y) = ((comp[i] as i32) % w, (comp[i] as i32) / w);
            i += 1;
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let k = (ny * w + nx) as usize;
                if alive[k] && !seen[k] {
                    seen[k] = true;
                    comp.push(k);
                }
            }
        }
        if comp.len() > best.len() {
            best = comp;
        }
    }
    let s = c.space("Cavern", "#6b5b4a", SpaceKind::Area);
    for k in &best {
        c.set((*k as i32) % w, (*k as i32) / w, s);
    }
    // A torch-sized light about every 150 squares, deterministic positions.
    for (i, k) in best.iter().enumerate() {
        if i % 150 == 75 {
            c.lights.push(((*k as i32) % w, (*k as i32) / w, 4.0));
        }
    }
}

const BUILDING_NAMES: [&str; 8] = [
    "Office", "Storage", "Kitchen", "Bedroom", "Lounge", "Workshop", "Bathroom", "Study",
];
const DECK_NAMES: [&str; 8] = [
    "Crew Quarters",
    "Cargo Bay",
    "Med Bay",
    "Mess",
    "Armory",
    "Airlock",
    "Lab",
    "Locker Room",
];

/// A building or a ship deck: a main corridor down the middle with rooms on
/// both sides. A deck is tapered at both ends, like a hull.
fn building(c: &mut Canvas, rng: &mut Rng, deck: bool) {
    let (w, h) = (c.w, c.h);
    let (x0, x1) = (2, w - 2);
    let cy = h / 2;
    let hall = c.space(
        if deck { "Main Corridor" } else { "Hall" },
        "#4a4e57",
        SpaceKind::Corridor,
    );
    let hall_h = if deck { 1 } else { 2 };
    let (hall_top, hall_bot) = (cy - hall_h / 2, cy - hall_h / 2 + hall_h); // [top, bot)
                                                                            // How far a room may reach above/below the corridor at column x.
    let reach = |x: i32| -> i32 {
        let full = (h / 2 - 2).max(2);
        if !deck {
            return full;
        }
        let from_end = (x - x0).min(x1 - 1 - x);
        (1 + from_end / 2).min(full)
    };
    c.rect(
        x0 + if deck { 2 } else { 0 },
        hall_top,
        x1 - if deck { 2 } else { 0 },
        hall_bot,
        hall,
        true,
    );
    let names: &[&str] = if deck { &DECK_NAMES } else { &BUILDING_NAMES };
    let mut ids = Vec::new();
    // The same column widths on both sides; a deck is mirrored, a building
    // gets its own random depth per room.
    let mut cols: Vec<(i32, i32)> = Vec::new();
    let mut x = x0 + if deck { 3 } else { 0 };
    let limit = x1 - if deck { 3 } else { 0 };
    while x + 3 <= limit {
        let cw = rng.range(4, 8).min(limit - x);
        if cw < 3 {
            break;
        }
        cols.push((x, cw));
        x += cw;
    }
    for (x, cw) in cols {
        let hull = (x..x + cw).map(reach).min().unwrap_or(0);
        let shared = rng.range(3, (h / 2 - 2).max(3));
        for side in [-1i32, 1] {
            let depth = if deck {
                hull
            } else {
                hull.min(if side < 0 {
                    shared
                } else {
                    rng.range(3, (h / 2 - 2).max(3))
                })
            };
            if depth < 2 {
                continue;
            }
            let (ry0, ry1) = if side < 0 {
                (hall_top - depth, hall_top)
            } else {
                (hall_bot, hall_bot + depth)
            };
            let name = rng.pick(names).to_string();
            let s = c.space(&name, &c.pal(ids.len()), SpaceKind::Room);
            c.rect(x, ry0, x + cw, ry1, s, false);
            // Door on the middle of the wall facing the corridor.
            let dx = x + cw / 2;
            if side < 0 {
                c.door(dx, hall_top - 1, dx, hall_top);
            } else {
                c.door(dx, hall_bot, dx, hall_bot - 1);
            }
            ids.push(s);
        }
    }
    for s in ids {
        c.label_room(s, 14);
    }
    if deck {
        c.labels
            .push((x0 as f64 + 3.0, cy as f64 + 0.5, "Aft".into()));
        c.labels
            .push((x1 as f64 - 3.0, cy as f64 + 0.5, "Fore".into()));
    }
}

const SHOP_NAMES: [&str; 8] = [
    "Noodle Bar",
    "Pawn Shop",
    "Clinic",
    "Arcade",
    "Tea House",
    "Workshop",
    "Apartments",
    "Shrine",
];

fn district(c: &mut Canvas, rng: &mut Rng) {
    let (w, h) = (c.w, c.h);
    let street = c.space("Street", "#555b66", SpaceKind::Area);
    c.rect(0, 0, w, h, street, true);
    let (bw, bh, gap) = (14, 12, 2);
    let mut by = gap;
    let mut n = 0;
    while by + bh <= h - gap {
        let mut bx = gap;
        while bx + bw <= w - gap {
            // Two rows of buildings back to back; each opens onto its own street.
            for row in 0..2 {
                let (y0, y1) = if row == 0 {
                    (by, by + bh / 2)
                } else {
                    (by + bh / 2, by + bh)
                };
                let mut x = bx;
                while x + 3 <= bx + bw {
                    let cw = rng.range(4, 7).min(bx + bw - x);
                    if cw < 3 {
                        break;
                    }
                    let name = rng.pick(&SHOP_NAMES).to_string();
                    let s = c.space(&name, &c.pal(n), SpaceKind::Room);
                    n += 1;
                    c.rect(x, y0, x + cw, y1, s, true);
                    let dx = x + cw / 2;
                    if row == 0 {
                        c.door(dx, y0, dx, y0 - 1);
                    } else {
                        c.door(dx, y1 - 1, dx, y1);
                    }
                    c.label_room(s, usize::MAX);
                    x += cw;
                }
            }
            // A street light at the corner of each block.
            c.lights.push((bx - 1, by - 1, 5.0));
            bx += bw + gap;
        }
        by += bh + gap;
    }
    c.labels.push((w as f64 / 2.0, 0.6, "District".into()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::FloorDoc;
    use crate::journal::{apply_op, Journal};
    use std::collections::BTreeSet;

    fn make(kind: GenKind, seed: u64, w: i32, h: i32) -> (FloorDoc, Vec<Op>) {
        let ops = generate(&GenParams {
            kind,
            seed,
            width: w,
            height: h,
        })
        .unwrap();
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        for op in &ops {
            j.push(&mut f, op).unwrap();
        }
        (f, ops)
    }

    #[test]
    fn same_seed_same_map_different_seed_different_map() {
        for kind in [
            GenKind::Dungeon,
            GenKind::Cave,
            GenKind::Building,
            GenKind::Deck,
            GenKind::District,
        ] {
            let (a, _) = make(kind, 7, 60, 40);
            let (b, _) = make(kind, 7, 60, 40);
            let (c, _) = make(kind, 8, 60, 40);
            assert_eq!(a, b, "{kind:?} is deterministic");
            assert!(a.grid.cell_count() > 100, "{kind:?} made something");
            if !matches!(kind, GenKind::Building | GenKind::Deck | GenKind::District) {
                assert_ne!(a, c, "{kind:?} depends on the seed");
            }
        }
    }

    #[test]
    fn every_generated_map_publishes_and_stays_in_bounds() {
        for kind in [
            GenKind::Dungeon,
            GenKind::Cave,
            GenKind::Building,
            GenKind::Deck,
            GenKind::District,
        ] {
            for seed in 1..6u64 {
                let (f, _) = make(kind, seed, 64, 48);
                let problems = crate::lint::lint(&f, 32.0);
                assert!(
                    !crate::lint::blocks_publish(&problems),
                    "{kind:?} seed {seed}: {problems:?}"
                );
                for (k, _) in f.grid.cells() {
                    assert!(
                        k.x >= 0 && k.y >= 0 && k.x < 64 && k.y < 48,
                        "{kind:?} cell {k:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_generated_dungeon_is_one_connected_map_with_doors() {
        for seed in 1..8u64 {
            let (f, _) = make(GenKind::Dungeon, seed, 64, 48);
            assert!(f.grid.cell_count() > 150);
            assert!(!f.grid.marks.is_empty(), "doors exist");
            // Flood fill over painted cells reaches every painted cell.
            let cells: BTreeSet<(i32, i32)> =
                f.grid.cells().map(|(k, _)| k).map(|k| (k.x, k.y)).collect();
            let start = *cells.iter().next().unwrap();
            let mut seen = BTreeSet::from([start]);
            let mut q = vec![start];
            while let Some((x, y)) = q.pop() {
                for n in [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)] {
                    if cells.contains(&n) && seen.insert(n) {
                        q.push(n);
                    }
                }
            }
            assert_eq!(seen.len(), cells.len(), "seed {seed}: all rooms are joined");
        }
    }

    #[test]
    fn a_deck_is_mirrored_and_its_ends_are_narrower_than_its_middle() {
        let (f, _) = make(GenKind::Deck, 4, 64, 40);
        let cells: BTreeSet<(i32, i32)> = f.grid.cells().map(|(k, _)| (k.x, k.y)).collect();
        let cy = 40 / 2;
        let extent = |x: i32| cells.iter().filter(|c| c.0 == x).count();
        let mid = (28..36).map(extent).max().unwrap();
        let nose = extent(5).max(extent(58));
        assert!(nose < mid, "nose {nose} narrower than middle {mid}");
        // Above and below the corridor line there are as many cells.
        let above = cells.iter().filter(|c| c.1 < cy).count();
        let below = cells.iter().filter(|c| c.1 > cy).count();
        assert_eq!(above, below, "mirrored about the corridor");
    }

    #[test]
    fn generation_is_one_undoable_step_and_sizes_are_checked() {
        let ops = generate(&GenParams {
            kind: GenKind::Dungeon,
            seed: 3,
            width: 40,
            height: 30,
        })
        .unwrap();
        assert_eq!(ops.len(), 1);
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        j.push(&mut f, &ops[0]).unwrap();
        assert!(f.grid.cell_count() > 0);
        j.undo_last(&mut f).unwrap();
        assert_eq!(f.grid.cell_count(), 0);
        assert!(f.labels.is_empty() && f.lights.is_empty());
        for (w, h) in [(5, 40), (40, 500)] {
            assert!(generate(&GenParams {
                kind: GenKind::Cave,
                seed: 1,
                width: w,
                height: h
            })
            .is_err());
        }
        let _ = apply_op;
    }
}
