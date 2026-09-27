//! Usage Heatmap drawings: a 100x100 key and a 200x100 dial strip, each a
//! row (7 days) or a 4x7 grid (4 weeks) of heat cells.

use crate::heatmap::HeatmapDisplay;
use crate::styles::{TRACK_COLOR, svg};
use crate::tile::{self, Anchor, CARD_COLOR, MUTED_TEXT_COLOR, TEXT_COLOR};

/// Cell geometry for one surface: column x of the first cell and the step,
/// cell width, and (y, height, row step) for the 1-row vs 4-row views.
struct Grid {
    x0: f64,
    x_step: f64,
    width: f64,
    single: (f64, f64),
    rows: (f64, f64, f64),
}

const KEY_GRID: Grid = Grid {
    x0: 11.0,
    x_step: 11.5,
    width: 9.0,
    single: (22.0, 56.0),
    rows: (22.0, 12.0, 14.0),
};

const STRIP_GRID: Grid = Grid {
    x0: 12.0,
    x_step: 26.3,
    width: 20.0,
    single: (26.0, 64.0),
    rows: (26.0, 14.0, 16.5),
};

fn cells(display: &HeatmapDisplay, grid: &Grid) -> String {
    let one_row = display.cells.len() <= 7;
    display
        .cells
        .iter()
        .enumerate()
        .map(|(k, shade)| {
            let (col, row) = ((k % 7) as f64, (k / 7) as f64);
            let x = grid.x0 + grid.x_step * col;
            let (y, height) = if one_row {
                grid.single
            } else {
                (grid.rows.0 + grid.rows.2 * row, grid.rows.1)
            };
            let width = grid.width;
            let paint = match shade {
                Some(r) => format!(
                    r#"fill="{}" fill-opacity="{:.2}""#,
                    display.color,
                    0.25 + 0.75 * r
                ),
                None => format!(r#"fill="{TRACK_COLOR}""#),
            };
            format!(
                r#"<rect x="{x:.2}" y="{y:.2}" width="{width}" height="{height}" rx="2" {paint} />"#
            )
        })
        .collect()
}

pub fn render_key(display: &HeatmapDisplay) -> String {
    let caption = tile::text_line(14.0, 11.0, true, MUTED_TEXT_COLOR, &display.caption);
    let letters: String = display
        .weekday_letters
        .iter()
        .enumerate()
        .map(|(i, letter)| {
            let x = KEY_GRID.x0 + KEY_GRID.x_step * i as f64 + KEY_GRID.width / 2.0;
            tile::text_at(
                x,
                92.0,
                Anchor::Middle,
                10.0,
                false,
                MUTED_TEXT_COLOR,
                &letter.to_string(),
            )
        })
        .collect();
    svg(&format!("{caption}{}{letters}", cells(display, &KEY_GRID)))
}

/// The dial strip is 200 wide, so it can't use `styles::svg` or
/// `tile::text_at` (both assume a 100-wide key - `text_at` would squeeze
/// a caption that fits easily here).
pub fn render_strip(display: &HeatmapDisplay) -> String {
    let caption = tile::escape_xml(&display.caption);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><rect x="0" y="0" width="200" height="100" fill="{CARD_COLOR}" /><text x="10" y="18" text-anchor="start" font-family="sans-serif" font-size="14" font-weight="700" fill="{TEXT_COLOR}">{caption}</text>{}</svg>"#,
        cells(display, &STRIP_GRID)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(cells: Vec<Option<f64>>) -> HeatmapDisplay {
        HeatmapDisplay {
            caption: "7 DAYS \u{b7} 1.2M".to_string(),
            cells,
            weekday_letters: vec!['T', 'F', 'S', 'S', 'M', 'T', 'W'],
            color: "#d97757".to_string(),
        }
    }

    fn week() -> Vec<Option<f64>> {
        vec![None, Some(0.0), Some(0.5), None, None, Some(0.2), Some(1.0)]
    }

    #[test]
    fn key_seven_days_is_one_row_of_tall_cells() {
        let s = render_key(&display(week()));
        assert_eq!(s.matches(r#"rx="2""#).count(), 7, "got: {s}");
        assert!(
            s.contains(r#"<rect x="11.00" y="22.00" width="9" height="56" rx="2""#),
            "got: {s}"
        );
        assert!(
            s.contains(r#"<rect x="80.00" y="22.00" width="9" height="56" rx="2""#),
            "got: {s}"
        );
        assert!(s.contains(">7 DAYS \u{b7} 1.2M</text>"));
    }

    #[test]
    fn key_four_weeks_is_a_grid() {
        let s = render_key(&display(vec![Some(0.5); 28]));
        assert_eq!(s.matches(r#"rx="2""#).count(), 28);
        // Last day: row 3, column 6.
        assert!(
            s.contains(r#"<rect x="80.00" y="64.00" width="9" height="12" rx="2""#),
            "got: {s}"
        );
    }

    #[test]
    fn shade_maps_ratio_to_opacity() {
        let s = render_key(&display(week()));
        assert!(s.contains(r##"fill="#d97757" fill-opacity="1.00""##));
        assert!(s.contains(r##"fill="#d97757" fill-opacity="0.25""##));
        assert!(s.contains(r##"fill="#d97757" fill-opacity="0.40""##)); // 0.25 + 0.75*0.2
    }

    #[test]
    fn none_cells_are_track_grey() {
        let s = render_key(&display(vec![None; 7]));
        assert_eq!(
            s.matches(&format!(r#"rx="2" fill="{TRACK_COLOR}""#))
                .count(),
            7
        );
        assert!(!s.contains("fill-opacity"));
    }

    #[test]
    fn key_has_weekday_letters_under_the_columns() {
        let s = render_key(&display(week()));
        assert!(
            s.contains(r#"x="15.5" y="92" text-anchor="middle""#),
            "got: {s}"
        );
        assert_eq!(s.matches(">S</text>").count(), 2);
        assert!(s.contains(">W</text>"));
    }

    #[test]
    fn strip_is_200_wide_with_bigger_cells() {
        let s = render_strip(&display(week()));
        assert!(s.contains(r#"viewBox="0 0 200 100""#));
        assert!(s.contains(&format!(r#"width="200" height="100" fill="{CARD_COLOR}""#)));
        assert!(
            s.contains(r#"<rect x="12.00" y="26.00" width="20" height="64" rx="2""#),
            "got: {s}"
        );
        assert!(
            s.contains(r#"<rect x="169.80" y="26.00" width="20" height="64" rx="2""#),
            "got: {s}"
        );
        let grid = render_strip(&display(vec![Some(1.0); 28]));
        assert!(
            grid.contains(r#"<rect x="169.80" y="75.50" width="20" height="14" rx="2""#),
            "got: {grid}"
        );
    }

    #[test]
    fn strip_caption_is_not_squeezed() {
        let mut d = display(week());
        d.caption = "4 WEEKS \u{b7} $1234.56".to_string();
        let s = render_strip(&d);
        assert!(s.contains(">4 WEEKS \u{b7} $1234.56</text>"), "got: {s}");
        assert!(!s.contains("textLength"), "got: {s}");
    }
}
