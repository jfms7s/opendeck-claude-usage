//! The Sessions key: a summary of every live Claude Code session, or one
//! session's details. Text is drawn inside the SVG (see tile.rs).

use chrono::{DateTime, Utc};

use crate::format::format_duration_compact;
use crate::level::{Level, Palette};
use crate::sessions::{Focus, Session, State, summarize};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR, text_line};

/// Longest session name shown before it is cut with `…`.
const NAME_CHARS: usize = 12;

fn level_color(level: Level) -> String {
    Palette::default().color(level).to_string()
}

fn cut(name: &str) -> String {
    if name.chars().count() <= NAME_CHARS {
        return name.to_string();
    }
    let kept: String = name.chars().take(NAME_CHARS - 1).collect();
    format!("{kept}…")
}

fn key(top: &str, big: &str, big_color: &str, big_size: f64, bottom: &str) -> String {
    let body = [
        text_line(24.0, 13.0, true, TEXT_COLOR, top),
        text_line(60.0, big_size, true, big_color, big),
        text_line(85.0, 11.0, false, MUTED_TEXT_COLOR, bottom),
    ]
    .concat();
    tile::svg(&body)
}

/// `3 sessions` over the longest wait (or `busy`) over the breakdown.
pub fn summary_svg(sessions: &[Session], now: DateTime<Utc>) -> String {
    let summary = summarize(sessions, now);
    let top = match summary.count {
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    };
    let (big, color, size) = match summary.longest_wait {
        Some(waited) => (
            format_duration_compact(waited),
            level_color(summary.level()),
            26.0,
        ),
        None if summary.count == 0 => ("–".to_string(), MUTED_TEXT_COLOR.to_string(), 26.0),
        None if summary.busy > 0 => ("busy".to_string(), level_color(Level::Normal), 22.0),
        None => ("?".to_string(), MUTED_TEXT_COLOR.to_string(), 26.0),
    };
    let bottom = if summary.count == 0 {
        "none open".to_string()
    } else {
        summary.breakdown()
    };
    key(&top, &big, &color, size, &bottom)
}

/// One session: its name over its time in state over `idle · 2/3`.
pub fn session_svg(session: &Session, position: usize, total: usize, now: DateTime<Utc>) -> String {
    let (big, color) = match session.in_state(now) {
        Some(d) if session.state != State::Unknown => {
            (format_duration_compact(d), level_color(session.level(now)))
        }
        _ => ("?".to_string(), MUTED_TEXT_COLOR.to_string()),
    };
    let bottom = format!("{} · {position}/{total}", session.state.word());
    key(&cut(&session.name), &big, &color, 26.0, &bottom)
}

/// The key's image for `focus`: the focused session when it is still
/// live, else the summary.
pub fn build_sessions_icon(focus: &Focus, sessions: &[Session], now: DateTime<Utc>) -> String {
    let focused = match focus {
        Focus::Summary => None,
        Focus::Session(id) => sessions.iter().position(|s| &s.id == id),
    };
    let svg = match focused {
        Some(i) => session_svg(&sessions[i], i + 1, sessions.len(), now),
        None => summary_svg(sessions, now),
    };
    tile::data_uri(&svg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{DEFAULT_CRITICAL, DEFAULT_NORMAL, DEFAULT_WATCH};
    use crate::sessions::parse;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_793_700_000_000).unwrap()
    }

    fn session(id: &str, status: &str, minutes_ago: i64) -> Session {
        let at = (now() - Duration::minutes(minutes_ago)).timestamp_millis();
        parse(
            "10",
            format!(
                r#"{{"sessionId":"{id}","name":"{id}","status":"{status}","statusUpdatedAt":{at}}}"#
            )
            .as_bytes(),
        )
    }

    #[test]
    fn no_sessions_show_zero_not_an_error() {
        let svg = summary_svg(&[], now());
        assert!(svg.contains(">0 sessions</text>"), "{svg}");
        assert!(svg.contains(">none open</text>"), "{svg}");
    }

    #[test]
    fn all_busy_stays_copper() {
        let svg = summary_svg(&[session("a", "busy", 30)], now());
        assert!(svg.contains(">1 session</text>"), "{svg}");
        assert!(
            svg.contains(&format!(r#"fill="{DEFAULT_NORMAL}">busy<"#)),
            "{svg}"
        );
        assert!(!svg.contains(DEFAULT_WATCH) && !svg.contains(DEFAULT_CRITICAL));
    }

    #[test]
    fn the_longest_wait_is_shown_in_its_level_colour() {
        let sessions = [
            session("a", "idle", 3),
            session("b", "waiting", 12),
            session("c", "busy", 1),
        ];
        let svg = summary_svg(&sessions, now());
        assert!(svg.contains(">3 sessions</text>"), "{svg}");
        assert!(svg.contains(">12m</text>"), "{svg}");
        assert!(svg.contains(DEFAULT_CRITICAL), "{svg}");
        assert!(svg.contains(">1 wait · 1 idle · 1 busy</text>"), "{svg}");
    }

    #[test]
    fn a_focused_session_shows_its_details() {
        let sessions = [
            session("a", "idle", 3),
            session("a-very-long-name", "busy", 1),
        ];
        let icon = build_sessions_icon(&Focus::Session("a".into()), &sessions, now());
        assert_eq!(
            icon,
            tile::data_uri(&session_svg(&sessions[0], 1, 2, now()))
        );
        let svg = session_svg(&sessions[0], 1, 2, now());
        assert!(
            svg.contains(">3m</text>") && svg.contains(DEFAULT_WATCH),
            "{svg}"
        );
        assert!(svg.contains(">idle · 1/2</text>"), "{svg}");
        let svg = session_svg(&sessions[1], 2, 2, now());
        assert!(svg.contains(">a-very-long…</text>"), "{svg}");
    }

    #[test]
    fn a_gone_session_falls_back_to_the_summary() {
        let sessions = [session("a", "idle", 3)];
        assert_eq!(
            build_sessions_icon(&Focus::Session("gone".into()), &sessions, now()),
            tile::data_uri(&summary_svg(&sessions, now()))
        );
    }

    #[test]
    fn an_unknown_session_shows_a_question_mark() {
        let s = parse("10", b"garbage");
        let svg = session_svg(&s, 1, 1, now());
        assert!(
            svg.contains(">?</text>") && svg.contains(">? · 1/1</text>"),
            "{svg}"
        );
        let svg = summary_svg(&[s], now());
        assert!(svg.contains(">1 ?</text>"), "{svg}");
    }
}
