//! Session + Weekly on one key: two stacked rows (horizontal) or two tall
//! bars side by side (vertical), each bar in its own window's level color
//! with tick marks at the shared marks.

use crate::combo::ComboLayout;
use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, svg};
use crate::tile::{self, Anchor, MUTED_TEXT_COLOR, TEXT_COLOR};

pub fn render(session: &UsageDisplay, weekly: &UsageDisplay, layout: ComboLayout) -> String {
    let body = match layout {
        ComboLayout::Horizontal => format!(
            "{}{}",
            row(session, "Session", 8.0),
            row(weekly, "Weekly", 54.0)
        ),
        ComboLayout::Vertical => format!(
            "{}{}",
            column(session, "5h", 32.0),
            column(weekly, "7d", 68.0)
        ),
    };
    svg(&body)
}

fn marks(display: &UsageDisplay) -> [f64; 3] {
    [
        display.marks.watch,
        display.marks.risk,
        display.marks.critical,
    ]
}

/// One window as a row: name left, percent right, a thin bar with ticks,
/// and the reset countdown underneath. `y0` is the row's top.
fn row(display: &UsageDisplay, name: &str, y0: f64) -> String {
    let name = tile::text_at(8.0, y0 + 12.0, Anchor::Start, 11.0, true, TEXT_COLOR, name);
    let percent = tile::text_at(
        92.0,
        y0 + 12.0,
        Anchor::End,
        13.0,
        true,
        &display.color,
        &display.percent_text,
    );
    let track_y = y0 + 17.0;
    let track = format!(
        r#"<rect x="8" y="{track_y}" width="84" height="6" rx="3" fill="{TRACK_COLOR}" />"#
    );
    let fill = if display.bar_value > 0.0 {
        // At least as wide as it is tall, so 1% still shows as a dot.
        let width = (84.0 * display.bar_value / 100.0).max(6.0);
        format!(
            r#"<rect x="8" y="{track_y}" width="{width:.2}" height="6" rx="3" fill="{}" />"#,
            display.color
        )
    } else {
        String::new()
    };
    let (y1, y2) = (y0 + 15.0, y0 + 25.0);
    let ticks: String = marks(display)
        .iter()
        .map(|m| {
            let x = 8.0 + 84.0 * m / 100.0;
            format!(r#"<line x1="{x:.2}" y1="{y1}" x2="{x:.2}" y2="{y2}" stroke="{TEXT_COLOR}" stroke-width="1" />"#)
        })
        .collect();
    let detail = tile::text_at(
        8.0,
        y0 + 35.0,
        Anchor::Start,
        10.0,
        false,
        MUTED_TEXT_COLOR,
        &display.detail_text,
    );
    format!("{name}{percent}{track}{fill}{ticks}{detail}")
}

/// One window as a tall bar filling upward, percent on top, "5h"/"7d"
/// underneath. `cx` is the column's center.
fn column(display: &UsageDisplay, label: &str, cx: f64) -> String {
    let percent = tile::text_at(
        cx,
        17.0,
        Anchor::Middle,
        13.0,
        true,
        &display.color,
        &display.percent_text,
    );
    let x = cx - 10.0;
    let track =
        format!(r#"<rect x="{x}" y="23" width="20" height="58" rx="4" fill="{TRACK_COLOR}" />"#);
    let fill = if display.bar_value > 0.0 {
        // At least twice the corner radius, so 1% still shows as a cap.
        let height = (58.0 * display.bar_value / 100.0).max(8.0);
        let y = 81.0 - height;
        format!(
            r#"<rect x="{x}" y="{y:.2}" width="20" height="{height:.2}" rx="4" fill="{}" />"#,
            display.color
        )
    } else {
        String::new()
    };
    let (x1, x2) = (cx - 13.0, cx + 13.0);
    let ticks: String = marks(display)
        .iter()
        .map(|m| {
            let y = 81.0 - 58.0 * m / 100.0;
            format!(r#"<line x1="{x1:.2}" y1="{y:.2}" x2="{x2:.2}" y2="{y:.2}" stroke="{TEXT_COLOR}" stroke-width="1" />"#)
        })
        .collect();
    let label = tile::text_at(
        cx,
        94.0,
        Anchor::Middle,
        12.0,
        false,
        MUTED_TEXT_COLOR,
        label,
    );
    format!("{percent}{track}{fill}{ticks}{label}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::error_display;
    use crate::styles::bar::tests::display;

    fn pair() -> (UsageDisplay, UsageDisplay) {
        let mut weekly = display(82.0);
        weekly.color = "#f97316".to_string();
        weekly.detail_text = "resets in 3d 2h".to_string();
        (display(46.0), weekly)
    }

    #[test]
    fn horizontal_rows_show_names_percents_and_details() {
        let (s, w) = pair();
        let svg = render(&s, &w, ComboLayout::Horizontal);
        assert!(
            svg.contains(">Session</text>") && svg.contains(">Weekly</text>"),
            "got: {svg}"
        );
        assert!(svg.contains(r##"fill="#d97757">46%</text>"##), "got: {svg}");
        assert!(svg.contains(r##"fill="#f97316">82%</text>"##), "got: {svg}");
        assert!(svg.contains(">resets in 3d 2h</text>"), "got: {svg}");
    }

    #[test]
    fn horizontal_fills_each_bar_in_its_own_color() {
        let (s, w) = pair();
        let svg = render(&s, &w, ComboLayout::Horizontal);
        // 84 * 0.46 = 38.64; 84 * 0.82 = 68.88
        assert!(
            svg.contains(r##"y="25" width="38.64" height="6" rx="3" fill="#d97757""##),
            "got: {svg}"
        );
        assert!(
            svg.contains(r##"y="71" width="68.88" height="6" rx="3" fill="#f97316""##),
            "got: {svg}"
        );
    }

    #[test]
    fn horizontal_ticks_at_default_marks() {
        let (s, w) = pair();
        let svg = render(&s, &w, ComboLayout::Horizontal);
        // watch 50 -> 50.00, risk 75 -> 71.00, critical 90 -> 83.60
        for x in ["50.00", "71.00", "83.60"] {
            assert!(
                svg.contains(&format!(r#"x1="{x}" y1="23" x2="{x}" y2="33""#)),
                "session tick {x}"
            );
            assert!(
                svg.contains(&format!(r#"x1="{x}" y1="69" x2="{x}" y2="79""#)),
                "weekly tick {x}"
            );
        }
    }

    #[test]
    fn vertical_bars_fill_from_the_bottom() {
        let (s, w) = pair();
        let svg = render(&s, &w, ComboLayout::Vertical);
        // 58 * 0.46 = 26.68 tall, top at 81 - 26.68 = 54.32
        assert!(
            svg.contains(r##"x="22" y="54.32" width="20" height="26.68" rx="4" fill="#d97757""##),
            "got: {svg}"
        );
        // 58 * 0.82 = 47.56 tall, top at 33.44
        assert!(
            svg.contains(r##"x="58" y="33.44" width="20" height="47.56" rx="4" fill="#f97316""##),
            "got: {svg}"
        );
        assert!(svg.contains(">5h</text>") && svg.contains(">7d</text>"));
    }

    #[test]
    fn vertical_ticks_at_default_marks() {
        let (s, w) = pair();
        let svg = render(&s, &w, ComboLayout::Vertical);
        // watch 50 -> y 52.00, risk 75 -> 37.50, critical 90 -> 28.80
        for y in ["52.00", "37.50", "28.80"] {
            assert!(
                svg.contains(&format!(r#"x1="19.00" y1="{y}" x2="45.00" y2="{y}""#)),
                "session tick {y}"
            );
            assert!(
                svg.contains(&format!(r#"x1="55.00" y1="{y}" x2="81.00" y2="{y}""#)),
                "weekly tick {y}"
            );
        }
    }

    #[test]
    fn full_usage_fills_the_whole_track() {
        let full = display(100.0);
        assert!(
            render(&full, &full, ComboLayout::Horizontal).contains(r#"width="84.00" height="6""#)
        );
        assert!(
            render(&full, &full, ComboLayout::Vertical)
                .contains(r#"y="23.00" width="20" height="58.00""#)
        );
    }

    #[test]
    fn tiny_usage_is_still_visible() {
        let tiny = display(1.0);
        assert!(
            render(&tiny, &tiny, ComboLayout::Horizontal).contains(r#"width="6.00" height="6""#)
        );
        assert!(render(&tiny, &tiny, ComboLayout::Vertical).contains(r#"height="8.00" rx="4""#));
    }

    #[test]
    fn error_displays_render_two_dashes_and_no_fill() {
        let e = error_display();
        for layout in [ComboLayout::Horizontal, ComboLayout::Vertical] {
            let svg = render(&e, &e, layout);
            assert_eq!(
                svg.matches(">\u{2014}</text>").count(),
                2,
                "{layout:?}: {svg}"
            );
            // The only colored rects are the two tracks.
            assert_eq!(
                svg.matches("<rect").count(),
                3,
                "{layout:?}: card + 2 tracks in {svg}"
            );
        }
    }
}
