//! Top bar (repo, branch, ahead/behind, fetch age, tabs) and bottom bar (keys, toast).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{fill, spans, text, width};
use crate::app::{App, Focus, Tab};
use crate::dates::{DateMode, format_date};
use gitty_core::forge::PrState;
use gitty_core::refs::Head;

pub fn top(app: &mut App, buf: &mut Buffer, r: Rect) {
    if r.height == 0 {
        return;
    }
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.status_bg).fg(ui.fg);
    fill(buf, r, base);
    let muted = base.fg(ui.muted);
    let y = r.y;
    let changes = match app.changes.entries().len() {
        0 => "[1] Changes".to_string(),
        n => format!("[1] Changes ({n})"),
    };
    // a bare repository has no working tree to browse
    let files = (Tab::Files, "[3] Files");
    let tabs: Vec<(Tab, &str)> = [Some((Tab::Changes, changes.as_str())), Some((Tab::History, "[2] History")), app.workdir.is_some().then_some(files)].into_iter().flatten().collect();
    let tabs_w: u16 = tabs.iter().map(|t| width(t.1) + 2).sum();
    let right = r.right();
    let mut tx = right.saturating_sub(tabs_w);
    let mut tab_hits = Vec::new();
    if r.width >= tabs_w + 20 {
        for (tab, label) in tabs {
            let st = if app.tab == tab { base.fg(ui.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { muted };
            let w = width(label);
            text(buf, tx + 1, y, right, label, st);
            tab_hits.push((Rect::new(tx, y, w + 2, 1), tab));
            tx += w + 2;
        }
    }
    let max_x = right.saturating_sub(tabs_w + 1);
    let mut x = spans(buf, r.x, y, max_x, &[(" gitty ", base.fg(ui.accent).add_modifier(Modifier::BOLD)), (&app.repo_name, base.add_modifier(Modifier::BOLD))]);
    if let Some(refs) = &app.refs {
        let branch = match &refs.head {
            Head::Branch { name, .. } => format!("  ⎇ {name}"),
            Head::Detached { id } => format!("  detached {}", id.short(7)),
        };
        x = text(buf, x, y, max_x, &branch, base);
        if refs.upstream.is_some() {
            let ahead = format!("  ↑{}", app.ahead.len());
            let behind = format!(" ↓{}", app.behind.len());
            x = spans(buf, x, y, max_x, &[(&ahead, base.fg(ui.ahead)), (&behind, base.fg(ui.behind))]);
        } else if refs.unpublished() {
            let n = app.ahead.len();
            let s = if n > 0 { format!("  ↑{n} not published") } else { "  not published".to_string() };
            x = text(buf, x, y, max_x, &s, base.fg(ui.ahead));
        }
    }
    // a stopped merge, rebase, cherry-pick or revert comes before the PR badge and the fetch age;
    // when it does not fit, it gives up its details one at a time and the bare word last; when
    // even that does not fit (a very narrow terminal), nothing is drawn
    if let Some(op) = &app.op
        && let Some(v) = op_banner(op).into_iter().find(|v| x + 2 + width(v) <= max_x)
    {
        x = text(buf, x + 2, y, max_x, &v, base.fg(ui.warning).add_modifier(Modifier::BOLD));
    }
    let mut pr_hit = None;
    if let Some((_, pr)) = &app.pr_badge {
        let color = match pr.state {
            PrState::Open => ui.pr_open,
            PrState::Draft => ui.pr_draft,
            PrState::Merged => ui.pr_merged,
            PrState::Closed => ui.pr_closed,
        };
        let number = format!("#{}", pr.number);
        let w = 3 + width(&number);
        // the whole badge or none: the branch name is never cut for it
        if x + 2 + w <= max_x {
            let st = base.fg(color);
            let end = spans(buf, x + 2, y, max_x, &[("PR ", st), (&number, st.add_modifier(Modifier::UNDERLINED))]);
            pr_hit = Some(Rect::new(x + 2, y, w, 1));
            x = end;
        }
    }
    if let Some(r) = app.needs_auth.as_deref().filter(|_| app.net.is_none()) {
        text(buf, x, y, max_x, &format!("  {r} needs auth · f"), base.fg(ui.warning));
    } else if let Some(p) = app.background_problem().filter(|_| app.net.is_none()) {
        text(buf, x, y, max_x, &format!("  {p}"), base.fg(ui.warning));
    } else if let Some(bar) = app.net_bar() {
        text(buf, x, y, max_x, &format!("  {bar}"), base.fg(ui.accent));
    } else if let Some(t) = app.fetched_at {
        let ago = format_date(t, 0, app.now, DateMode::Relative);
        let s = if ago == "now" { "  fetched just now".to_string() } else { format!("  fetched {ago} ago") };
        // beside the badge, the age goes first when it does not fit
        if pr_hit.is_none() || x + width(&s) <= max_x {
            text(buf, x, y, max_x, &s, muted);
        }
    }
    app.hits.tabs = tab_hits;
    app.hits.pr_badge = pr_hit;
}

/// The banner for a state in progress, from the full text to the bare word.
fn op_banner(s: &gitty_core::op_state::OpState) -> Vec<String> {
    use gitty_core::op_state::RepoOp;
    let verb = match s.op {
        RepoOp::Merge => "MERGING",
        RepoOp::Rebase => "REBASING",
        RepoOp::CherryPick => "CHERRY-PICKING",
        RepoOp::Revert => "REVERTING",
    };
    let step = s.step.map(|(n, m)| format!("step {n}/{m}"));
    let conflicts = match s.conflicts {
        0 => "no conflicts".to_string(),
        1 => "1 conflict".to_string(),
        n => format!("{n} conflicts"),
    };
    let join = |parts: &[Option<String>]| parts.iter().flatten().cloned().collect::<Vec<_>>().join(" · ");
    let named = if s.detail.is_empty() { verb.to_string() } else { format!("{verb} {}", s.detail) };
    let mut out = vec![
        join(&[Some(named), step.clone(), Some(conflicts.clone())]),
        join(&[Some(verb.to_string()), step, Some(conflicts.clone())]),
        join(&[Some(verb.to_string()), Some(conflicts)]),
        verb.to_string(),
    ];
    out.dedup();
    out
}

/// `G` stays `G`; named keys read lowercase (`enter`, `ctrl-d`).
fn hint_label(k: &crate::keymap::Key) -> String {
    let l = k.label();
    if l.chars().count() == 1 { l } else { l.to_lowercase() }
}

/// Bottom-bar hints for the focused pane, with the keys the keymap really uses (`alt+enter` is
/// fixed).
fn hints(app: &App) -> Vec<(String, String)> {
    use crate::keymap::Action as A;
    if app.tab == Tab::Changes && app.focus != Focus::Commit && app.conflict_active() {
        return conflict_hints(app);
    }
    let list: &[(&[A], &str)] = if app.tab == Tab::Files {
        match app.focus {
            Focus::Diff => &[(&[A::Down, A::Up], "scroll"), (&[A::ScrollLeft, A::ScrollRight], "sideways"), (&[A::OpenEditor], "edit"), (&[A::RevealSecret], "reveal"), (&[A::Back], "back"), (&[A::Help], "help")],
            _ => &[(&[A::Down, A::Up], "move"), (&[A::FilesCollapse, A::FilesExpand], "fold"), (&[A::Open], "open"), (&[A::OpenEditor], "edit"), (&[A::RevealSecret], "reveal"), (&[A::HistoryTab], "history"), (&[A::Help], "help"), (&[A::Quit], "quit")],
        }
    } else if app.tab == Tab::Changes {
        match app.focus {
            Focus::Diff => &[(&[A::Stage], "stage line"), (&[A::LineRange], "range"), (&[A::StageHunk], "hunk"), (&[A::StageAll], "file"), (&[A::Discard], "discard"), (&[A::PrevHunk, A::NextHunk], "hunk"), (&[A::Back], "back")],
            Focus::Commit => return vec![("alt+enter".into(), "commit".into()), ("tab".into(), "field".into()), ("esc".into(), "leave".into())],
            _ => &[(&[A::Stage], "stage"), (&[A::StageAll], "all"), (&[A::Discard], "discard"), (&[A::Filter], "filter"), (&[A::Open], "diff"), (&[A::HistoryTab], "history"), (&[A::Help], "help"), (&[A::Quit], "quit")],
        }
    } else {
        match app.focus {
            Focus::History if app.compare.is_some() => &[(&[A::Down, A::Up], "move"), (&[A::CompareBehind, A::CompareAhead], "tab"), (&[A::Open], "files"), (&[A::Compare], "other branch"), (&[A::Back], "leave"), (&[A::Help], "help")],
            Focus::History => &[(&[A::Down, A::Up], "move"), (&[A::Open], "files"), (&[A::Search], "search"), (&[A::Range], "range"), (&[A::Compare], "compare"), (&[A::NextPane], "pane"), (&[A::Scope], "scope"), (&[A::Help], "help"), (&[A::Quit], "quit")],
            Focus::Files => &[(&[A::Down, A::Up], "file"), (&[A::Open], "diff"), (&[A::Tree], "tree"), (&[A::Back], "back"), (&[A::PrevHunk, A::NextHunk], "hunk"), (&[A::Help], "help"), (&[A::Quit], "quit")],
            Focus::Commit => &[],
            Focus::Diff => &[(&[A::Down, A::Up], "line"), (&[A::PrevHunk, A::NextHunk], "hunk"), (&[A::Expand, A::ExpandFile], "expand"), (&[A::Split], "split"), (&[A::Whitespace], "whitespace"), (&[A::Difftool], "difftool"), (&[A::Fullscreen], "full"), (&[A::Back], "back")],
        }
    };
    list.iter()
        .filter_map(|(acts, what)| {
            let keys: Vec<String> = acts.iter().filter_map(|a| app.keymap.keys_of(*a).first().map(hint_label)).collect();
            (!keys.is_empty()).then(|| (keys.join("/"), what.to_string()))
        })
        .collect()
}

/// What the keys do in the conflict view, by the names of the sides.
fn conflict_hints(app: &App) -> Vec<(String, String)> {
    use crate::keymap::Action as A;
    let (ours, theirs, blocks) = app.conflict_view().map_or(("Current", "Incoming", 0), |v| (v.sides.ours.title, v.sides.theirs.title, v.conflicts().len()));
    let mut list: Vec<(Vec<A>, String)> = vec![(vec![A::ConflictOurs], format!("keep {ours}")), (vec![A::ConflictTheirs], format!("take {theirs}"))];
    if blocks > 0 {
        list.push((vec![A::ConflictBoth], "both".into()));
        list.push((vec![A::ConflictPrev, A::ConflictNext], "prev/next".into()));
    }
    list.extend([(vec![A::ConflictUndo], "undo".into()), (vec![A::ConflictEdit], "edit".into()), (vec![A::Stage], "stage".into()), (vec![A::Help], "help".into())]);
    // p, u and e are other things on other files: say so where the keys are listed
    list.push((vec![A::ConflictPrev, A::ConflictUndo, A::ConflictEdit], "act on conflicts here".into()));
    list.into_iter()
        .filter_map(|(acts, what)| {
            let keys: Vec<String> = acts.iter().filter_map(|a| app.keymap.keys_of(*a).first().map(hint_label)).collect();
            (!keys.is_empty()).then(|| (keys.join("/"), what))
        })
        .collect()
}

pub fn bottom(app: &App, buf: &mut Buffer, r: Rect) {
    if r.height == 0 {
        return;
    }
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.status_bg).fg(ui.status_fg);
    fill(buf, r, base);
    let mut max_x = r.right();
    if let Some(t) = &app.toast {
        let (s, st) = if t.error {
            (format!(" ✗ {}  ! details ", t.what), base.fg(ui.error).add_modifier(Modifier::BOLD))
        } else {
            (format!(" {} ", t.what), base.fg(ui.accent))
        };
        let w = width(&s).min(r.width * 2 / 3);
        let x = r.right().saturating_sub(w);
        text(buf, x, r.y, r.right(), &s, st);
        max_x = x;
    }
    let mut x = r.x + 1;
    if let Some(label) = app.search_label() {
        let end = text(buf, x, r.y, max_x, &label, base.fg(ui.accent).add_modifier(Modifier::BOLD));
        if let Some(bar) = &app.search.bar {
            // block cursor after "/" and the text before the editor's cursor
            let cx = x + 1 + width(&bar.text()[..bar.cursor()]);
            if cx < max_x {
                let cell = &mut buf[(cx, r.y)];
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
        x = end + 2;
    }
    // an overlay has the keys: it prints its own hints, the pane's would mislead
    let pane_hints = if app.overlay.is_some() { Vec::new() } else { hints(app) };
    for (k, d) in pane_hints {
        if x + width(&k) + width(&d) + 3 > max_x {
            break;
        }
        x = spans(buf, x, r.y, max_x, &[(&k, base.fg(ui.accent)), (" ", base), (&d, base), ("  ", base)]);
    }
}
