//! The ready-list renderer: [`draw`] paints a [`super::App`] into a ratatui
//! `Frame`. Pure over `(App, now)` — no clock read, no I/O, no mutation beyond
//! the frame buffer — so every rendered cell is deterministic and testable with
//! `TestBackend`. See `plans/slices/slice-9.md`.
//!
//! Layout (top to bottom): a one-line title with key hints, the grouped ready
//! list, and a one-line status bar (last-refreshed age + a warning summary).
//! Grouping is a view concern: rows are rendered in the App's flat order (so the
//! selection index and the on-screen order stay aligned and `j`/`k` move one
//! displayed row at a time) with a `▸ <repo>` header emitted whenever the repo
//! changes from the previous row.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{App, ViewMode};
use crate::cli::{format_row_body, sanitize};

/// Shown when the roster has no rows at all (no repos configured / nothing
/// hydrated yet — see slice-9 decision 3).
const EMPTY_HINT: &str = "no repos configured — run: fbd repos discover ~/dev";

/// Shown when there are rows but the active filters hide them all, so the user
/// is not misdirected to reconfigure the roster.
const NO_MATCH_HINT: &str = "no issues match the current filters — press f/p to change";

/// One-line key hints shown along the top.
const KEY_HINTS: &str = "fbd · q quit · r refresh · / search · f repo · p prio · j/k move";

/// Render the whole ready screen for the current [`App`] state and clock `now`.
pub fn draw(frame: &mut Frame, app: &App, now: SystemTime) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title / key hints
            Constraint::Min(0),    // the ready list
            Constraint::Length(1), // status bar
        ])
        .split(frame.area());

    frame.render_widget(Paragraph::new(KEY_HINTS), chunks[0]);
    draw_list(frame, app, chunks[1]);
    frame.render_widget(Paragraph::new(status_line(app, now)), chunks[2]);
}

/// Build and render the middle list area: the loading placeholder, the empty
/// hint, or the repo-grouped rows with the selection scrolled into view.
fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    // Show "Loading…" only while the first refresh is actually in flight. A
    // failed initial refresh (`RefreshCompleted { snapshot: None }`) leaves the
    // App in `Loading` with `stale` cleared; falling through renders the empty
    // hint instead of a permanent, misleading spinner (the status bar carries
    // the failure warning).
    if app.view_mode() == ViewMode::Loading && app.is_stale() {
        frame.render_widget(Paragraph::new("Loading…"), area);
        return;
    }

    let rows = app.filtered_rows();
    if rows.is_empty() {
        // Zero rows at all vs. rows hidden by the active filters: only the former
        // is a roster problem, so only it points at `repos discover`.
        let hint = if app.rows().is_empty() {
            EMPTY_HINT
        } else {
            NO_MATCH_HINT
        };
        frame.render_widget(Paragraph::new(hint), area);
        return;
    }

    let header_style = Style::default().add_modifier(Modifier::BOLD);
    let selected_style = Style::default().add_modifier(Modifier::REVERSED);
    let selection = app.selection();

    // Render rows in the App's flat order (the selection space) and emit a repo
    // header whenever the repo changes, so the on-screen order matches the
    // navigation order — `j`/`k` always move one displayed row. A repo whose
    // rows are non-contiguous in the flat order gets its header again, which is
    // an honest label for the priority-sorted run beneath it.
    let mut lines: Vec<Line> = Vec::new();
    // The rendered line index (headers included) of the selected row, so the
    // list can be scrolled to keep it on screen once it exceeds the area height.
    let mut selected_line: Option<usize> = None;
    let mut current_repo: Option<&str> = None;
    for (i, row) in rows.iter().enumerate() {
        if current_repo != Some(row.repo_name.as_str()) {
            lines.push(Line::styled(
                format!("▸ {}", sanitize(&row.repo_name)),
                header_style,
            ));
            current_repo = Some(row.repo_name.as_str());
        }
        let text = format!("  {}", format_row_body(row));
        if selection == Some(i) {
            selected_line = Some(lines.len());
            // Pad to the full width so the reversed highlight fills the row.
            let padded = format!("{text:<width$}", width = area.width as usize);
            lines.push(Line::styled(padded, selected_style));
        } else {
            lines.push(Line::from(text));
        }
    }

    // Scroll just enough to keep the selected line inside the viewport: nothing
    // until it would fall off the bottom, then anchor it to the last visible
    // row. Stateless (recomputed each draw), so no cross-frame offset to track.
    let offset = scroll_offset(selected_line, area.height as usize, lines.len());
    frame.render_widget(Paragraph::new(lines).scroll((offset, 0)), area);
}

/// The vertical scroll offset that keeps `selected` visible within `height`
/// rendered rows. Returns 0 when the selection fits above the fold or there is
/// nothing to scroll.
fn scroll_offset(selected: Option<usize>, height: usize, total: usize) -> u16 {
    let (Some(sel), true) = (selected, height > 0 && total > height) else {
        return 0;
    };
    let offset = if sel < height { 0 } else { sel - height + 1 };
    offset.min(u16::MAX as usize) as u16
}

/// The status-bar text: last-refreshed age (or a refreshing indicator) plus a
/// one-line summary of any warnings, pointing at `fbd doctor` for detail.
fn status_line(app: &App, now: SystemTime) -> String {
    let mut status = match app.fetched_at() {
        Some(t) => format!("refreshed {}", format_age(now, t)),
        None if app.is_stale() => "refreshing…".to_string(),
        None => "never refreshed".to_string(),
    };
    // A refresh over already-shown rows: annotate without hiding the age.
    if app.is_stale() && app.fetched_at().is_some() {
        status.push_str(" · refreshing…");
    }
    let n = app.status_warnings().len();
    if n > 0 {
        let plural = if n == 1 { "" } else { "s" };
        status.push_str(&format!(" · {n} repo{plural} failed (see doctor)"));
    }
    status
}

/// Humanize the elapsed time since a snapshot was fetched into a compact
/// "… ago" label. Saturates to `just now` when `now` precedes `fetched` (clock
/// skew) so the age is never negative or misleading.
fn format_age(now: SystemTime, fetched: SystemTime) -> String {
    let secs = now
        .duration_since(fetched)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::bd::Issue;
    use crate::snapshot::{Row, Snapshot};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::time::{Duration, UNIX_EPOCH};

    const W: u16 = 80;
    const H: u16 = 24;

    fn row(repo: &str, id: &str, priority: i64, title: &str) -> Row {
        Row {
            issue: Issue {
                id: id.to_string(),
                title: title.to_string(),
                status: "open".into(),
                priority,
                description: None,
                issue_type: None,
                owner: None,
                created_at: None,
                created_by: None,
                updated_at: None,
                dependency_count: None,
                dependent_count: None,
                comment_count: None,
            },
            repo_name: repo.to_string(),
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// An `App` advanced to `List` with the given rows (fetched at `at(1000)`)
    /// and warnings, via a single `RefreshCompleted`.
    fn app_with(rows: Vec<Row>, warnings: Vec<String>) -> App {
        let mut app = App::new();
        app.reduce(Msg::RefreshCompleted {
            snapshot: Some(Snapshot {
                rows,
                fetched_at: at(1000),
            }),
            warnings,
        });
        app
    }

    fn render(app: &App, now: SystemTime) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal.draw(|f| draw(f, app, now)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The full text of buffer row `y`, cell symbols concatenated.
    fn line_text(buf: &Buffer, y: u16) -> String {
        (0..W).map(|x| buf.cell((x, y)).unwrap().symbol()).collect()
    }

    /// The `y` of the first buffer row whose text contains `needle`.
    fn find_line(buf: &Buffer, needle: &str) -> Option<u16> {
        (0..H).find(|&y| line_text(buf, y).contains(needle))
    }

    #[test]
    fn renders_group_headers_and_rows() {
        // Contiguous by repo so each repo reads as one section.
        let app = app_with(
            vec![
                row("session-tui", "ra-2hc", 1, "Ready task one"),
                row("session-tui", "ra-9", 2, "Second task"),
                row("megaclock", "mc-1", 0, "Third task"),
            ],
            vec![],
        );
        let buf = render(&app, at(1000));

        let header = find_line(&buf, "▸ session-tui").expect("session-tui header present");
        assert!(
            find_line(&buf, "▸ megaclock").is_some(),
            "megaclock header present"
        );
        let row_y = find_line(&buf, "P1 ra-2hc").expect("row line present");
        assert!(
            line_text(&buf, row_y).contains("Ready task one"),
            "row carries its title"
        );
        assert_ne!(header, row_y, "header and row are on distinct lines");
    }

    #[test]
    fn renders_selection_highlight() {
        let mut app = app_with(
            vec![
                row("session-tui", "ra-2hc", 1, "Ready task one"),
                row("session-tui", "ra-9", 2, "Second task"),
            ],
            vec![],
        );
        app.reduce(Msg::SelectNext); // selection = 1 -> ra-9
        let buf = render(&app, at(1000));

        let sel_y = find_line(&buf, "ra-9").expect("selected row present");
        assert!(
            buf.cell((2, sel_y))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED),
            "the selected row is highlighted (REVERSED)"
        );
        let other_y = find_line(&buf, "ra-2hc").expect("other row present");
        assert!(
            !buf.cell((2, other_y))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED),
            "a non-selected row is not highlighted"
        );
    }

    #[test]
    fn renders_status_bar_with_age_and_warnings() {
        let app = app_with(
            vec![row("session-tui", "ra-2hc", 1, "Ready task one")],
            vec!["export failed for reading-lite".into()],
        );
        // fetched at 1000; now 180s later.
        let buf = render(&app, at(1180));

        let status = line_text(&buf, H - 1);
        assert!(
            status.contains("refreshed 3m ago"),
            "status shows humanized age: {status:?}"
        );
        assert!(
            status.contains("1 repo failed (see doctor)"),
            "status summarizes the warning: {status:?}"
        );
    }

    #[test]
    fn renders_empty_state_hint() {
        let app = app_with(vec![], vec![]);
        let buf = render(&app, at(1000));

        assert!(
            find_line(&buf, EMPTY_HINT).is_some(),
            "an empty ready list shows the discover hint"
        );
    }

    #[test]
    fn renders_loading_before_first_snapshot() {
        let app = App::new(); // Loading, no snapshot yet
        let buf = render(&app, at(1000));

        assert!(find_line(&buf, "Loading…").is_some(), "loading placeholder");
        assert!(
            find_line(&buf, EMPTY_HINT).is_none(),
            "loading is not the empty state"
        );
    }

    #[test]
    fn navigation_follows_display_order() {
        // Interleaved repos in flat (priority) order: the rendered order must
        // match the flat order so selection index N is the Nth displayed row.
        let app = app_with(
            vec![
                row("repo-a", "ra-0", 0, "a first"),
                row("repo-b", "rb-0", 0, "b first"),
                row("repo-a", "ra-1", 1, "a second"),
            ],
            vec![],
        );
        let buf = render(&app, at(1000));

        let y0 = find_line(&buf, "ra-0").expect("ra-0 present");
        let y1 = find_line(&buf, "rb-0").expect("rb-0 present");
        let y2 = find_line(&buf, "ra-1").expect("ra-1 present");
        assert!(
            y0 < y1 && y1 < y2,
            "rows keep flat order on screen: ra-0@{y0} rb-0@{y1} ra-1@{y2}"
        );
        // repo-a's run is split by repo-b, so its header is drawn twice.
        let repo_a_headers = (0..H)
            .filter(|&y| line_text(&buf, y).contains("▸ repo-a"))
            .count();
        assert_eq!(
            repo_a_headers, 2,
            "a non-contiguous repo re-emits its header"
        );
    }

    #[test]
    fn filtered_empty_shows_filter_hint() {
        // Rows exist but the priority filter hides them all: the user must be
        // told it is a filter, not an unconfigured roster.
        let mut app = app_with(vec![row("repo-a", "ra-2", 2, "low prio")], vec![]);
        app.reduce(Msg::TogglePriorityFilter); // HighOnly -> hides the P2 row
        let buf = render(&app, at(1000));

        assert!(
            find_line(&buf, NO_MATCH_HINT).is_some(),
            "filtered-empty shows the filter hint"
        );
        assert!(
            find_line(&buf, EMPTY_HINT).is_none(),
            "filtered-empty is not misreported as an unconfigured roster"
        );
    }

    #[test]
    fn keeps_selection_in_viewport() {
        // A list far taller than the 24-row screen: selecting the last row must
        // scroll it into view (and the first row out).
        let rows: Vec<Row> = (0..40)
            .map(|n| row("session-tui", &format!("ra-{n:02}"), 1, "task"))
            .collect();
        let mut app = app_with(rows, vec![]);
        for _ in 0..39 {
            app.reduce(Msg::SelectNext); // selection -> last row (ra-39)
        }
        let buf = render(&app, at(1000));

        let sel_y = find_line(&buf, "ra-39").expect("selected last row is on screen");
        assert!(
            buf.cell((2, sel_y))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED),
            "the scrolled-to selection is highlighted"
        );
        assert!(
            find_line(&buf, "ra-00").is_none(),
            "the first row has scrolled off the top"
        );
    }

    #[test]
    fn scroll_offset_keeps_selection_visible() {
        // Fits above the fold: no scroll.
        assert_eq!(scroll_offset(Some(3), 10, 40), 0);
        assert_eq!(scroll_offset(None, 10, 40), 0);
        // Short list (fits entirely): never scroll.
        assert_eq!(scroll_offset(Some(5), 10, 8), 0);
        // Past the fold: anchor selection to the last visible row.
        assert_eq!(scroll_offset(Some(10), 10, 40), 1);
        assert_eq!(scroll_offset(Some(39), 10, 40), 30);
    }

    #[test]
    fn failed_initial_refresh_shows_hint_not_loading() {
        // The first refresh fails (no snapshot): the App stays in `Loading` with
        // `stale` cleared. The list must not show a permanent spinner.
        let mut app = App::new();
        app.reduce(Msg::RefreshCompleted {
            snapshot: None,
            warnings: vec!["hub sync failed".into()],
        });
        let buf = render(&app, at(1000));

        assert!(
            find_line(&buf, "Loading…").is_none(),
            "a concluded (failed) refresh is not still 'Loading…'"
        );
        assert!(
            find_line(&buf, EMPTY_HINT).is_some(),
            "the empty hint is shown instead"
        );
        assert!(
            line_text(&buf, H - 1).contains("1 repo failed (see doctor)"),
            "the failure is surfaced in the status bar"
        );
    }

    #[test]
    fn format_age_humanizes() {
        let base = at(1_000_000);
        assert_eq!(format_age(base, base), "just now");
        assert_eq!(format_age(at(1_000_042), base), "42s ago");
        assert_eq!(format_age(at(1_000_180), base), "3m ago");
        assert_eq!(format_age(at(1_007_200), base), "2h ago");
        assert_eq!(format_age(at(1_432_000), base), "5d ago");
        // Clock skew: now precedes fetched -> saturates, never negative.
        assert_eq!(format_age(base, at(1_000_100)), "just now");
    }
}
