//! Map overlay items (plan §5.2, §7.2): points of interest, political regions
//! and routes/journeys, in document cells. On a region map these are what the
//! GM lays over the terrain and what publishes to nd-world as markers and
//! regions (plan §9.2 B). One op pair (`put_overlay` / `remove_overlay`) covers
//! all three kinds.

use crate::error::{CoreError, CoreResult, MAX_CELL};
use serde::{Deserialize, Serialize};

pub const MAX_MARKERS: usize = 500; // nd-world's per-list overlay cap
pub const MAX_REGIONS: usize = 500;
pub const MAX_ROUTES: usize = 500;
pub const MAX_OVERLAY_POINTS: usize = 2_000;

fn colour_ok(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn pts_ok(points: &[[f32; 2]], min: usize) -> CoreResult<()> {
    if points.len() < min || points.len() > MAX_OVERLAY_POINTS {
        return Err(CoreError::Invalid(format!(
            "needs {min}..={MAX_OVERLAY_POINTS} points"
        )));
    }
    let lim = MAX_CELL as f32;
    for p in points {
        if !(p[0].is_finite() && p[1].is_finite()) || p[0].abs() > lim || p[1].abs() > lim {
            return Err(CoreError::OutOfBounds("overlay point".into()));
        }
    }
    Ok(())
}

/// A point of interest (a town, a ruin, a battle map to jump to).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Marker {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub label: String,
    #[serde(default)]
    pub note: String,
    #[serde(default = "gold")]
    pub color: String,
    /// The slug of an nd-world battle map this marker opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_schematic: Option<String>,
    /// Stairs, ladders and lifts: the floor this marker leads to, and where
    /// you land there (cells; absent means "the same spot").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links_to: Option<FloorLink>,
}

/// A vertical connection to another floor of the same document.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct FloorLink {
    pub floor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
}

fn gold() -> String {
    "#e8b030".into()
}

/// A political region: a bordered, tinted polygon (nations, provinces, fiefs).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub points: Vec<[f32; 2]>,
    #[serde(default = "red")]
    pub color: String,
    #[serde(default = "quarter")]
    pub opacity: f32,
    /// Diagonal hatching over the fill (disputed or special land).
    #[serde(default)]
    pub hatch: bool,
}

fn red() -> String {
    "#c0504d".into()
}
fn quarter() -> f32 {
    0.25
}

/// A road, trade route or journey: a styled line with waymarks.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Route {
    pub id: String,
    pub name: String,
    pub points: Vec<[f32; 2]>,
    #[serde(default = "brown")]
    pub color: String,
    /// Line width in cells.
    #[serde(default = "tenth")]
    pub width: f32,
    #[serde(default)]
    pub dashed: bool,
    /// Draw a smooth curve through the points instead of straight segments.
    #[serde(default)]
    pub smooth: bool,
}

fn brown() -> String {
    "#6b3f1d".into()
}
fn tenth() -> f32 {
    0.12
}

impl Marker {
    pub fn validate(&self) -> CoreResult<()> {
        if !id_ok(&self.id) {
            return Err(CoreError::Invalid(
                "marker id must be 1-64 chars of a-z, 0-9, _ -".into(),
            ));
        }
        pts_ok(&[[self.x, self.y]], 1)?;
        if self.label.chars().count() > 80 || self.note.chars().count() > 1_000 {
            return Err(CoreError::Invalid(
                "marker label is capped at 80 chars, note at 1000".into(),
            ));
        }
        if !colour_ok(&self.color) {
            return Err(CoreError::Invalid("marker color must be #rrggbb".into()));
        }
        if self
            .link_schematic
            .as_deref()
            .is_some_and(|s| s.len() > 120 || s.contains(['/', '?', '#', ' ']))
        {
            return Err(CoreError::Invalid(
                "link_schematic must be a plain map slug".into(),
            ));
        }
        if let Some(l) = &self.links_to {
            if !id_ok(&l.floor) {
                return Err(CoreError::Invalid(
                    "links_to.floor must be a floor id".into(),
                ));
            }
            match (l.x, l.y) {
                (None, None) => {}
                (Some(x), Some(y)) => pts_ok(&[[x, y]], 1)?,
                _ => {
                    return Err(CoreError::Invalid(
                        "links_to needs both x and y, or neither".into(),
                    ))
                }
            }
        }
        Ok(())
    }
}

impl Region {
    pub fn validate(&self) -> CoreResult<()> {
        if !id_ok(&self.id) {
            return Err(CoreError::Invalid(
                "region id must be 1-64 chars of a-z, 0-9, _ -".into(),
            ));
        }
        if self.name.chars().count() > 80 {
            return Err(CoreError::Invalid(
                "region name is capped at 80 chars".into(),
            ));
        }
        pts_ok(&self.points, 3)?;
        if !colour_ok(&self.color) {
            return Err(CoreError::Invalid("region color must be #rrggbb".into()));
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err(CoreError::Invalid("region opacity must be 0..=1".into()));
        }
        Ok(())
    }
}

/// A smooth curve through `pts` (Catmull-Rom, `steps` samples per segment).
/// Only + - * on f32, so it is the same on every platform. The curve passes
/// through every control point; fewer than 3 points stay as they are.
pub fn smooth_path(pts: &[[f32; 2]], steps: usize) -> Vec<[f32; 2]> {
    if pts.len() < 3 || steps < 2 {
        return pts.to_vec();
    }
    let at = |i: isize| pts[i.clamp(0, pts.len() as isize - 1) as usize];
    let mut out = Vec::with_capacity((pts.len() - 1) * steps + 1);
    for i in 0..pts.len() as isize - 1 {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        for k in 0..steps {
            let t = k as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            out.push([f(p0[0], p1[0], p2[0], p3[0]), f(p0[1], p1[1], p2[1], p3[1])]);
        }
    }
    out.push(pts[pts.len() - 1]);
    out
}

impl Route {
    /// The points as drawn: curved when `smooth`.
    pub fn drawn_points(&self) -> Vec<[f32; 2]> {
        if self.smooth {
            smooth_path(&self.points, 8)
        } else {
            self.points.clone()
        }
    }

    pub fn validate(&self) -> CoreResult<()> {
        if !id_ok(&self.id) {
            return Err(CoreError::Invalid(
                "route id must be 1-64 chars of a-z, 0-9, _ -".into(),
            ));
        }
        if self.name.chars().count() > 80 {
            return Err(CoreError::Invalid(
                "route name is capped at 80 chars".into(),
            ));
        }
        pts_ok(&self.points, 2)?;
        if !colour_ok(&self.color) {
            return Err(CoreError::Invalid("route color must be #rrggbb".into()));
        }
        if !self.width.is_finite() || !(0.02..=2.0).contains(&self.width) {
            return Err(CoreError::Invalid(
                "route width must be 0.02..=2 cells".into(),
            ));
        }
        Ok(())
    }
}

/// Any overlay item, tagged by kind on the wire.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayItem {
    Marker(Marker),
    Region(Region),
    Route(Route),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    Marker,
    Region,
    Route,
}

impl OverlayItem {
    pub fn kind(&self) -> OverlayKind {
        match self {
            OverlayItem::Marker(_) => OverlayKind::Marker,
            OverlayItem::Region(_) => OverlayKind::Region,
            OverlayItem::Route(_) => OverlayKind::Route,
        }
    }
    pub fn id(&self) -> &str {
        match self {
            OverlayItem::Marker(m) => &m.id,
            OverlayItem::Region(r) => &r.id,
            OverlayItem::Route(r) => &r.id,
        }
    }
    pub fn validate(&self) -> CoreResult<()> {
        match self {
            OverlayItem::Marker(m) => m.validate(),
            OverlayItem::Region(r) => r.validate(),
            OverlayItem::Route(r) => r.validate(),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_smooth_path_passes_through_its_points_and_is_deterministic() {
        let pts = [[0.0, 0.0], [4.0, 3.0], [8.0, 0.0], [12.0, 5.0]];
        let a = super::smooth_path(&pts, 8);
        assert_eq!(a, super::smooth_path(&pts, 8));
        assert_eq!(a.len(), 3 * 8 + 1);
        for p in pts {
            assert!(a.contains(&p), "{p:?} on the curve");
        }
        // It bends: it is not the straight polyline.
        assert!(a
            .iter()
            .any(|q| (q[1] - 1.5).abs() > 0.2 && q[0] > 1.0 && q[0] < 3.0));
        assert_eq!(super::smooth_path(&pts[..2], 8), pts[..2].to_vec());
    }

    use super::*;

    pub(crate) fn marker(id: &str) -> Marker {
        Marker {
            id: id.into(),
            x: 4.0,
            y: 5.0,
            label: "Port Neon".into(),
            note: "smugglers".into(),
            color: "#e8b030".into(),
            link_schematic: Some("harbour-map".into()),
            links_to: None,
        }
    }
    pub(crate) fn region(id: &str) -> Region {
        Region {
            id: id.into(),
            name: "Kingdom".into(),
            points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 8.0], [0.0, 6.0]],
            color: "#c0504d".into(),
            opacity: 0.3,
            hatch: false,
        }
    }
    pub(crate) fn route(id: &str) -> Route {
        Route {
            id: id.into(),
            name: "King's Road".into(),
            points: vec![[0.0, 0.0], [5.0, 2.0], [9.0, 3.0]],
            color: "#6b3f1d".into(),
            width: 0.15,
            dashed: true,
            smooth: false,
        }
    }

    #[test]
    fn good_items_validate_and_defaults_fill_in() {
        marker("m").validate().unwrap();
        region("r").validate().unwrap();
        route("t").validate().unwrap();
        let bare: OverlayItem =
            serde_json::from_str(r#"{"kind":"marker","id":"a","x":1,"y":2,"label":"Hi"}"#).unwrap();
        bare.validate().unwrap();
        assert_eq!(bare.kind(), OverlayKind::Marker);
        let r: OverlayItem = serde_json::from_str(
            r#"{"kind":"region","id":"a","name":"N","points":[[0,0],[1,0],[1,1]]}"#,
        )
        .unwrap();
        r.validate().unwrap();
        if let OverlayItem::Region(r) = r {
            assert_eq!((r.opacity, r.hatch), (0.25, false));
        }
    }

    #[test]
    fn hostile_items_are_refused() {
        type Case = Box<dyn Fn() -> OverlayItem>;
        let cases: Vec<(&str, Case)> = vec![
            (
                "marker bad id",
                Box::new(|| {
                    OverlayItem::Marker(Marker {
                        id: "a b".into(),
                        ..marker("x")
                    })
                }),
            ),
            (
                "marker nan",
                Box::new(|| {
                    OverlayItem::Marker(Marker {
                        x: f32::NAN,
                        ..marker("x")
                    })
                }),
            ),
            (
                "marker far",
                Box::new(|| {
                    OverlayItem::Marker(Marker {
                        y: 1e9,
                        ..marker("x")
                    })
                }),
            ),
            (
                "marker long label",
                Box::new(|| {
                    OverlayItem::Marker(Marker {
                        label: "x".repeat(81),
                        ..marker("x")
                    })
                }),
            ),
            (
                "marker colour",
                Box::new(|| {
                    OverlayItem::Marker(Marker {
                        color: "gold".into(),
                        ..marker("x")
                    })
                }),
            ),
            (
                "marker slug with slash",
                Box::new(|| {
                    OverlayItem::Marker(Marker {
                        link_schematic: Some("../x".into()),
                        links_to: None,
                        ..marker("x")
                    })
                }),
            ),
            (
                "region two points",
                Box::new(|| {
                    OverlayItem::Region(Region {
                        points: vec![[0.0, 0.0], [1.0, 1.0]],
                        ..region("x")
                    })
                }),
            ),
            (
                "region opacity",
                Box::new(|| {
                    OverlayItem::Region(Region {
                        opacity: 3.0,
                        ..region("x")
                    })
                }),
            ),
            (
                "region inf",
                Box::new(|| {
                    OverlayItem::Region(Region {
                        points: vec![[0.0, 0.0], [f32::INFINITY, 1.0], [1.0, 1.0]],
                        ..region("x")
                    })
                }),
            ),
            (
                "route one point",
                Box::new(|| {
                    OverlayItem::Route(Route {
                        points: vec![[0.0, 0.0]],
                        ..route("x")
                    })
                }),
            ),
            (
                "route width",
                Box::new(|| {
                    OverlayItem::Route(Route {
                        width: 0.0,
                        ..route("x")
                    })
                }),
            ),
            (
                "route too many",
                Box::new(|| {
                    OverlayItem::Route(Route {
                        points: vec![[0.0, 0.0]; MAX_OVERLAY_POINTS + 1],
                        ..route("x")
                    })
                }),
            ),
        ];
        for (name, f) in cases {
            assert!(f().validate().is_err(), "{name} should be refused");
        }
    }
}
