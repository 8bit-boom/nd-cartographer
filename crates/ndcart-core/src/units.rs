//! One coordinate transform (plan §22.4): a document's cell lattice to
//! picture pixels, with the inverse. Every exporter derives from this
//! instead of its own multiply, so origins, crops and scale stay provably
//! consistent - the blueprint's "coordinate conversion must not become
//! implicit magic constants".

use crate::error::{CoreError, CoreResult};

/// Cells (f64, the document lattice) to picture pixels and back, with the
/// optional crop: pixel = (cell - crop_origin) * ppc (+ padding, applied by
/// the encoder, not here - cropping is a subtraction in cell space).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
    /// Pixels per grid square (the picture's `cell_size`).
    pub ppc: f64,
    /// The crop's top-left corner in cells (0 for an uncropped export).
    pub crop: [f64; 2],
}

impl Transform {
    pub fn new(ppc: f64) -> Self {
        Transform {
            ppc,
            crop: [0.0, 0.0],
        }
    }

    pub fn cropped(ppc: f64, crop: [f64; 2]) -> Self {
        Transform { ppc, crop }
    }

    fn checked(&self, v: f64, what: &str) -> CoreResult<f64> {
        if v.is_finite() {
            Ok(v)
        } else {
            Err(CoreError::Invalid(format!("{what} must be finite")))
        }
    }

    /// A cell coordinate to picture pixels.
    pub fn cell_to_px(&self, cell: [f64; 2]) -> CoreResult<[f64; 2]> {
        Ok([
            self.checked((cell[0] - self.crop[0]) * self.ppc, "cell x")?,
            self.checked((cell[1] - self.crop[1]) * self.ppc, "cell y")?,
        ])
    }

    /// Picture pixels back to cells (the inverse; rounding is the caller's).
    pub fn px_to_cell(&self, px: [f64; 2]) -> CoreResult<[f64; 2]> {
        Ok([
            self.checked(px[0] / self.ppc + self.crop[0], "px x")?,
            self.checked(px[1] / self.ppc + self.crop[1], "px y")?,
        ])
    }

    /// A picture size for `cells` cells across.
    pub fn px_size(&self, cells: [i64; 2]) -> CoreResult<[i64; 2]> {
        let dim = |n: i64| -> CoreResult<i64> {
            let v = (n as f64) * self.ppc;
            if !v.is_finite() || v > 9.0e15 {
                return Err(CoreError::Invalid("picture too large".into()));
            }
            Ok(v.round() as i64)
        };
        Ok([dim(cells[0])?, dim(cells[1])?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_and_px_round_trip_with_and_without_a_crop() {
        let plain = Transform::new(32.0);
        let px = plain.cell_to_px([12.0, -7.5]).unwrap();
        assert_eq!(px, [384.0, -240.0]);
        let back = plain.px_to_cell(px).unwrap();
        assert_eq!(back, [12.0, -7.5]);

        // A crop shifts both directions consistently.
        let cropped = Transform::cropped(32.0, [10.0, 20.0]);
        let px = cropped.cell_to_px([12.0, 22.0]).unwrap();
        assert_eq!(px, [64.0, 64.0]);
        assert_eq!(cropped.px_to_cell(px).unwrap(), [12.0, 22.0]);
    }

    #[test]
    fn non_finite_input_is_refused() {
        let t = Transform::new(32.0);
        assert!(t.cell_to_px([f64::NAN, 0.0]).is_err());
        assert!(t.px_to_cell([f64::INFINITY, 0.0]).is_err());
    }

    #[test]
    fn sizes_round_and_refuse_absurd_results() {
        let t = Transform::new(16.0);
        assert_eq!(t.px_size([100, 70]).unwrap(), [1600, 1120]);
        assert!(t.px_size([i64::MAX, 1]).is_err());
    }
}
