//! The first-party stamp pack: props defined as vector primitives in a unit
//! box (`-0.5..=0.5`, x right, y down), so they are tiny, resolution-independent
//! and MIT (nothing to license — plan §7.5). The server rasterizes them.

use crate::Rgb;

#[derive(Clone, Copy, Debug)]
pub enum Prim {
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        rgb: Rgb,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rgb: Rgb,
    },
    Poly {
        pts: &'static [[f32; 2]],
        rgb: Rgb,
    },
    Line {
        a: [f32; 2],
        b: [f32; 2],
        w: f32,
        rgb: Rgb,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct StampDef {
    pub id: &'static str,
    pub name: &'static str,
    /// Natural size in cells (width, height).
    pub size: [f32; 2],
    /// How tall the prop is: scales the drop-shadow offset. 0 lies flat (a rug).
    pub height: f32,
    pub prims: &'static [Prim],
}

const WOOD: Rgb = [0x8b, 0x5e, 0x34];
const WOOD_D: Rgb = [0x5e, 0x3e, 0x20];
const WOOD_L: Rgb = [0xa8, 0x7a, 0x4a];
const STONE: Rgb = [0x9a, 0x9a, 0x98];
const STONE_L: Rgb = [0xb8, 0xb8, 0xb4];
const IRON: Rgb = [0x4a, 0x4e, 0x56];

const ROCK_PTS: &[[f32; 2]] = &[
    [-0.42, 0.05],
    [-0.25, -0.35],
    [0.08, -0.45],
    [0.38, -0.25],
    [0.45, 0.15],
    [0.2, 0.42],
    [-0.18, 0.4],
];
const ROCK_HI: &[[f32; 2]] = &[[-0.22, -0.05], [-0.1, -0.28], [0.1, -0.33], [0.05, -0.1]];

const PACK: &[StampDef] = &[
    StampDef {
        id: "crate",
        name: "Crate",
        size: [1.0, 1.0],
        height: 0.6,
        prims: &[
            Prim::Rect {
                x: -0.46,
                y: -0.46,
                w: 0.92,
                h: 0.92,
                rgb: WOOD,
            },
            Prim::Rect {
                x: -0.38,
                y: -0.38,
                w: 0.76,
                h: 0.76,
                rgb: WOOD_L,
            },
            Prim::Line {
                a: [-0.4, -0.4],
                b: [0.4, 0.4],
                w: 0.07,
                rgb: WOOD_D,
            },
            Prim::Line {
                a: [0.4, -0.4],
                b: [-0.4, 0.4],
                w: 0.07,
                rgb: WOOD_D,
            },
        ],
    },
    StampDef {
        id: "barrel",
        name: "Barrel",
        size: [1.0, 1.0],
        height: 0.7,
        prims: &[
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.45,
                ry: 0.45,
                rgb: WOOD_D,
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.38,
                ry: 0.38,
                rgb: WOOD,
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.26,
                ry: 0.26,
                rgb: WOOD_L,
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.07,
                ry: 0.07,
                rgb: WOOD_D,
            },
        ],
    },
    StampDef {
        id: "table",
        name: "Table",
        size: [2.0, 1.0],
        height: 0.5,
        prims: &[
            Prim::Rect {
                x: -0.48,
                y: -0.45,
                w: 0.96,
                h: 0.9,
                rgb: WOOD_D,
            },
            Prim::Rect {
                x: -0.44,
                y: -0.4,
                w: 0.88,
                h: 0.8,
                rgb: WOOD,
            },
            Prim::Line {
                a: [-0.4, -0.12],
                b: [0.4, -0.12],
                w: 0.025,
                rgb: WOOD_D,
            },
            Prim::Line {
                a: [-0.4, 0.12],
                b: [0.4, 0.12],
                w: 0.025,
                rgb: WOOD_D,
            },
        ],
    },
    StampDef {
        id: "chair",
        name: "Chair",
        size: [0.7, 0.7],
        height: 0.4,
        prims: &[
            Prim::Rect {
                x: -0.4,
                y: -0.1,
                w: 0.8,
                h: 0.5,
                rgb: WOOD,
            },
            Prim::Rect {
                x: -0.4,
                y: -0.45,
                w: 0.8,
                h: 0.22,
                rgb: WOOD_D,
            },
        ],
    },
    StampDef {
        id: "pillar",
        name: "Pillar",
        size: [1.0, 1.0],
        height: 1.0,
        prims: &[
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.46,
                ry: 0.46,
                rgb: [0x6e, 0x6e, 0x6c],
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.38,
                ry: 0.38,
                rgb: STONE,
            },
            Prim::Ellipse {
                cx: -0.05,
                cy: -0.05,
                rx: 0.2,
                ry: 0.2,
                rgb: STONE_L,
            },
        ],
    },
    StampDef {
        id: "torch",
        name: "Torch",
        size: [0.6, 0.6],
        height: 0.0,
        prims: &[
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.5,
                ry: 0.5,
                rgb: [0xff, 0xb0, 0x40],
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.32,
                ry: 0.32,
                rgb: [0xff, 0xd0, 0x60],
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.16,
                ry: 0.16,
                rgb: [0xff, 0xf2, 0xb0],
            },
        ],
    },
    StampDef {
        id: "bed",
        name: "Bed",
        size: [1.0, 2.0],
        height: 0.4,
        prims: &[
            Prim::Rect {
                x: -0.48,
                y: -0.49,
                w: 0.96,
                h: 0.98,
                rgb: WOOD_D,
            },
            Prim::Rect {
                x: -0.42,
                y: -0.43,
                w: 0.84,
                h: 0.86,
                rgb: [0xc9, 0xc3, 0xb4],
            },
            Prim::Rect {
                x: -0.42,
                y: -0.05,
                w: 0.84,
                h: 0.48,
                rgb: [0x7a, 0x2e, 0x3a],
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: -0.28,
                rx: 0.3,
                ry: 0.13,
                rgb: [0xf0, 0xec, 0xe0],
            },
        ],
    },
    StampDef {
        id: "rug",
        name: "Rug",
        size: [2.0, 1.4],
        height: 0.0,
        prims: &[
            Prim::Rect {
                x: -0.5,
                y: -0.5,
                w: 1.0,
                h: 1.0,
                rgb: [0x6a, 0x1f, 0x24],
            },
            Prim::Rect {
                x: -0.42,
                y: -0.42,
                w: 0.84,
                h: 0.84,
                rgb: [0xb0, 0x8a, 0x3a],
            },
            Prim::Rect {
                x: -0.36,
                y: -0.36,
                w: 0.72,
                h: 0.72,
                rgb: [0x8a, 0x2b, 0x2e],
            },
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.2,
                ry: 0.2,
                rgb: [0xb0, 0x8a, 0x3a],
            },
        ],
    },
    StampDef {
        id: "chest",
        name: "Chest",
        size: [1.0, 0.7],
        height: 0.5,
        prims: &[
            Prim::Rect {
                x: -0.48,
                y: -0.45,
                w: 0.96,
                h: 0.9,
                rgb: WOOD_D,
            },
            Prim::Rect {
                x: -0.43,
                y: -0.4,
                w: 0.86,
                h: 0.8,
                rgb: WOOD,
            },
            Prim::Rect {
                x: -0.47,
                y: -0.08,
                w: 0.94,
                h: 0.16,
                rgb: IRON,
            },
            Prim::Rect {
                x: -0.07,
                y: -0.16,
                w: 0.14,
                h: 0.28,
                rgb: [0xd0, 0xb0, 0x40],
            },
        ],
    },
    StampDef {
        id: "tree",
        name: "Tree",
        size: [3.0, 3.0],
        height: 1.6,
        prims: &[
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.5,
                ry: 0.5,
                rgb: [0x2c, 0x55, 0x2a],
            },
            Prim::Ellipse {
                cx: -0.06,
                cy: -0.06,
                rx: 0.4,
                ry: 0.4,
                rgb: [0x3a, 0x6e, 0x34],
            },
            Prim::Ellipse {
                cx: -0.12,
                cy: -0.12,
                rx: 0.24,
                ry: 0.24,
                rgb: [0x4c, 0x86, 0x40],
            },
            Prim::Ellipse {
                cx: 0.12,
                cy: 0.1,
                rx: 0.14,
                ry: 0.12,
                rgb: [0x32, 0x60, 0x30],
            },
        ],
    },
    StampDef {
        id: "bush",
        name: "Bush",
        size: [1.2, 1.2],
        height: 0.5,
        prims: &[
            Prim::Ellipse {
                cx: 0.0,
                cy: 0.0,
                rx: 0.46,
                ry: 0.42,
                rgb: [0x34, 0x60, 0x2e],
            },
            Prim::Ellipse {
                cx: -0.1,
                cy: -0.08,
                rx: 0.3,
                ry: 0.28,
                rgb: [0x46, 0x7c, 0x3a],
            },
            Prim::Ellipse {
                cx: 0.16,
                cy: 0.12,
                rx: 0.12,
                ry: 0.1,
                rgb: [0xc0, 0x40, 0x50],
            },
        ],
    },
    StampDef {
        id: "rock",
        name: "Rock",
        size: [1.2, 1.2],
        height: 0.6,
        prims: &[
            Prim::Poly {
                pts: ROCK_PTS,
                rgb: [0x6a, 0x68, 0x66],
            },
            Prim::Poly {
                pts: ROCK_HI,
                rgb: [0x8e, 0x8c, 0x88],
            },
        ],
    },
];

pub fn stamps() -> &'static [StampDef] {
    PACK
}

pub fn find_stamp(id: &str) -> Option<&'static StampDef> {
    PACK.iter().find(|s| s.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_ids_and_sane_geometry() {
        let mut ids: Vec<&str> = stamps().iter().map(|s| s.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n);
        assert!(n >= 12);
        for s in stamps() {
            assert!(
                s.size[0] > 0.0 && s.size[1] > 0.0 && s.height >= 0.0,
                "{}",
                s.id
            );
            assert!(!s.prims.is_empty());
            assert!(find_stamp(s.id).is_some());
            for p in s.prims {
                let pts: Vec<[f32; 2]> = match p {
                    Prim::Ellipse { cx, cy, rx, ry, .. } => {
                        vec![[cx - rx, cy - ry], [cx + rx, cy + ry]]
                    }
                    Prim::Rect { x, y, w, h, .. } => vec![[*x, *y], [x + w, y + h]],
                    Prim::Poly { pts, .. } => pts.to_vec(),
                    Prim::Line { a, b, .. } => vec![*a, *b],
                };
                for q in pts {
                    assert!(
                        q[0].abs() <= 0.5001 && q[1].abs() <= 0.5001,
                        "{} leaves the unit box: {q:?}",
                        s.id
                    );
                }
            }
        }
        assert!(find_stamp("nope").is_none());
    }
}
