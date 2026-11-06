//! Print export: the map on paper at a true scale (one inch per square by
//! default), cut into pages. A small hand-written PDF writer (images as JPEG,
//! one Helvetica label per page) so printing needs no extra dependency, and
//! each page is a seamless crop of the same render the screen uses.

use image::{codecs::jpeg::JpegEncoder, ExtendedColorType};

pub const LETTER: (f32, f32) = (612.0, 792.0);
pub const A4: (f32, f32) = (595.276, 841.89);
pub const MAX_PAGES: usize = 80;

pub fn paper(name: &str) -> Option<(f32, f32)> {
    match name {
        "letter" => Some(LETTER),
        "a4" => Some(A4),
        _ => None,
    }
}

/// One page of the map: a block of cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub cx: u32,
    pub cy: u32,
    pub cols: u32,
    pub rows: u32,
    pub page: usize,
    pub of: usize,
    /// 1-based position on the sheet-grid, for the label ("row 2, column 3").
    pub row: u32,
    pub col: u32,
}

/// Cut `cols` x `rows` cells into page-sized blocks at `inch` inches a square.
pub fn plan(
    cols: u32,
    rows: u32,
    inch: f32,
    paper: (f32, f32),
    margin: f32,
) -> Result<Vec<Region>, String> {
    if !(0.25..=3.0).contains(&inch) {
        return Err("the scale must be 0.25 to 3 inches per square".into());
    }
    let square = inch * 72.0;
    let per_x = (((paper.0 - 2.0 * margin) / square).floor() as u32).max(1);
    let per_y = (((paper.1 - 2.0 * margin) / square).floor() as u32).max(1);
    let (nx, ny) = (cols.div_ceil(per_x), rows.div_ceil(per_y));
    let of = (nx * ny) as usize;
    if of > MAX_PAGES {
        return Err(format!(
            "that would take {of} pages (the limit is {MAX_PAGES}); use a smaller scale or print one floor at a time"
        ));
    }
    let mut out = Vec::with_capacity(of);
    for r in 0..ny {
        for c in 0..nx {
            let (cx, cy) = (c * per_x, r * per_y);
            out.push(Region {
                cx,
                cy,
                cols: per_x.min(cols - cx),
                rows: per_y.min(rows - cy),
                page: out.len() + 1,
                of,
                row: r + 1,
                col: c + 1,
            });
        }
    }
    Ok(out)
}

pub struct Page {
    pub rgb: Vec<u8>,
    pub px: (u32, u32),
    /// The size the picture is drawn at, in points.
    pub size_pt: (f32, f32),
    pub label: String,
}

fn jpeg(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 88)
        .encode(rgb, w, h, ExtendedColorType::Rgb8)
        .map_err(|e| format!("cannot encode a page: {e}"))?;
    Ok(out)
}

/// Escape a string for a PDF literal.
fn esc(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii() && !c.is_control())
        .flat_map(|c| match c {
            '(' | ')' | '\\' => vec!['\\', c],
            _ => vec![c],
        })
        .collect()
}

/// A PDF with one page per `Page`, the picture at the top-left of the printable
/// area, corner ticks in the margin, and the label at the bottom.
pub fn build(
    paper: (f32, f32),
    margin: f32,
    title: &str,
    pages: &[Page],
) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    // Object numbers: 1 catalog, 2 pages, 3 font, 4 info, then per page: page, content, image.
    let n = pages.len();
    let page_obj = |i: usize| 5 + i * 3;
    let mut obj = |out: &mut Vec<u8>, id: usize, body: &[u8]| {
        while offsets.len() < id {
            offsets.push(0);
        }
        offsets[id - 1] = out.len();
        out.extend_from_slice(format!("{id} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    obj(&mut out, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: String = (0..n).map(|i| format!("{} 0 R ", page_obj(i))).collect();
    obj(
        &mut out,
        2,
        format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>").as_bytes(),
    );
    obj(
        &mut out,
        3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    );
    obj(
        &mut out,
        4,
        format!("<< /Title ({}) /Producer (ndcart) >>", esc(title)).as_bytes(),
    );
    for (i, p) in pages.iter().enumerate() {
        let (po, co, io) = (page_obj(i), page_obj(i) + 1, page_obj(i) + 2);
        let (w, h) = p.size_pt;
        let (x, y) = (margin, paper.1 - margin - h);
        let tick = 8.0;
        let mut c = String::new();
        c.push_str(&format!("q {w:.3} 0 0 {h:.3} {x:.3} {y:.3} cm /Im0 Do Q\n"));
        // Corner ticks just outside the picture, so pages can be trimmed and butted together.
        c.push_str("0.4 w 0 G\n");
        for (cx, cy, dx, dy) in [
            (x, y + h, -1.0, 1.0),
            (x + w, y + h, 1.0, 1.0),
            (x, y, -1.0, -1.0),
            (x + w, y, 1.0, -1.0),
        ] {
            c.push_str(&format!(
                "{:.3} {cy:.3} m {:.3} {cy:.3} l S\n",
                cx,
                cx + dx * tick
            ));
            c.push_str(&format!(
                "{cx:.3} {:.3} m {cx:.3} {:.3} l S\n",
                cy,
                cy + dy * tick
            ));
        }
        c.push_str(&format!(
            "BT /F1 8 Tf {:.3} {:.3} Td ({}) Tj ET\n",
            margin,
            margin * 0.45,
            esc(&p.label)
        ));
        obj(
            &mut out,
            po,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.3} {:.3}] /Resources << /Font << /F1 3 0 R >> /XObject << /Im0 {io} 0 R >> >> /Contents {co} 0 R >>",
                paper.0, paper.1
            )
            .as_bytes(),
        );
        obj(
            &mut out,
            co,
            format!("<< /Length {} >>\nstream\n{c}endstream", c.len()).as_bytes(),
        );
        let data = jpeg(&p.rgb, p.px.0, p.px.1)?;
        let mut body = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            p.px.0,
            p.px.1,
            data.len()
        )
        .into_bytes();
        body.extend_from_slice(&data);
        body.extend_from_slice(b"\nendstream");
        obj(&mut out, io, &body);
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_is_cut_into_pages_that_cover_it_exactly_once() {
        // Letter with half-inch margins: 7.5 x 10 in printable = 7 x 10 squares at 1 inch.
        let pages = plan(20, 25, 1.0, LETTER, 36.0).unwrap();
        assert_eq!(pages.len(), 3 * 3);
        assert_eq!((pages[0].cols, pages[0].rows), (7, 10));
        let last = pages.last().unwrap();
        assert_eq!(
            (last.cx, last.cy, last.cols, last.rows, last.row, last.col, last.page, last.of),
            (14, 20, 6, 5, 3, 3, 9, 9)
        );
        let covered: u32 = pages.iter().map(|p| p.cols * p.rows).sum();
        assert_eq!(covered, 20 * 25);
        // Half-inch squares fit more per page; absurd scales and huge maps are refused.
        assert_eq!(plan(20, 25, 0.5, LETTER, 36.0).unwrap()[0].cols, 15);
        assert!(plan(20, 25, 0.1, LETTER, 36.0).is_err());
        assert!(plan(400, 400, 1.0, A4, 36.0).unwrap_err().contains("pages"));
        assert_eq!(paper("a4"), Some(A4));
        assert!(paper("tabloid").is_none());
    }

    #[test]
    fn the_pdf_has_a_valid_cross_reference_and_the_right_pages() {
        let page = |label: &str| Page {
            rgb: vec![200; 8 * 6 * 3],
            px: (8, 6),
            size_pt: (144.0, 108.0),
            label: label.into(),
        };
        let pdf = build(
            LETTER,
            36.0,
            "Keep (ground)",
            &[page("Page 1 of 2"), page("Page 2 of 2")],
        )
        .unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4") && text.trim_end().ends_with("%%EOF"));
        assert!(
            text.contains("/Count 2")
                && text.contains("Keep \\(ground\\)")
                && text.contains("(Page 2 of 2) Tj")
        );
        // Every xref entry points at "N 0 obj". (Work on bytes: the JPEGs are not UTF-8.)
        let find_last = |needle: &[u8]| {
            pdf.windows(needle.len())
                .rposition(|w| w == needle)
                .unwrap()
        };
        let xref = find_last(b"\nxref\n") + 1;
        let tail = String::from_utf8_lossy(&pdf[xref..]).to_string();
        let entries: Vec<&str> = tail
            .lines()
            .skip(3)
            .take_while(|l| l.ends_with(" n "))
            .collect();
        assert_eq!(entries.len(), 4 + 2 * 3);
        for (i, e) in entries.iter().enumerate() {
            let off: usize = e[..10].parse().unwrap();
            assert!(
                pdf[off..].starts_with(format!("{} 0 obj", i + 1).as_bytes()),
                "object {} at {off}",
                i + 1
            );
        }
        let start: usize = tail
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(start, xref);
        // The JPEG streams are real JPEGs.
        assert_eq!(
            pdf.windows(3).filter(|w| *w == [0xFF, 0xD8, 0xFF]).count(),
            2
        );
    }
}
