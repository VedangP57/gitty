//! Keyboard and mouse handling.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use gitty_core::conflicts::Choice;
use gitty_core::diff::ops::WsMode;
use gitty_core::diff::view::{Expand, Row, SplitRow};
use ratatui::layout::{Position, Rect};

use super::diffstate::VRow;
use super::changes::Side;
use super::{App, Focus, Overlay, Tab, Toast};
use crate::config::{Config, Density};
use crate::keymap::{Action, State};
use crate::ui::layout::{self, MAX_FILES, MIN_FILES, Mode, Sep};

#[derive(Clone, Copy)]
enum Move {
    Step(i64),
    Half(i64),
    Page(i64),
    Top,
    Bottom,
}

fn target(cur: usize, len: usize, page: usize, m: Move) -> usize {
    if len == 0 {
        return 0;
    }
    let delta = |n: i64| (cur as i64 + n).clamp(0, len as i64 - 1) as usize;
    match m {
        Move::Step(n) => delta(n),
        Move::Half(n) => delta(n * (page as i64 / 2).max(1)),
        Move::Page(n) => delta(n * page as i64),
        Move::Top => 0,
        Move::Bottom => len - 1,
    }
}

fn inside(r: Option<Rect>, x: u16, y: u16) -> Option<Rect> {
    r.filter(|r| r.contains(Position { x, y }))
}

impl App {
    pub fn handle_key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        self.dirty = true;
        self.settle_files();
        if self.toast.as_ref().is_some_and(|t| !t.error) {
            self.toast = None;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match k.code {
                // like q, a foreground job is not cancelled without asking
                KeyCode::Char('c') => {
                    return match self.overlay.take() {
                        // a second Ctrl-C at the question quits
                        Some(Overlay::Quit { .. }) => self.quit_now(),
                        // at a prompt it answers "cancelled", as Esc does: git stops waiting
                        Some(ov @ Overlay::Prompt { .. }) => {
                            self.overlay_key(ov, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                            self.next_ask();
                        }
                        // any other overlay closes, then the usual question (or quit)
                        _ => self.request_quit(),
                    };
                }
                KeyCode::Char('z') => return self.suspend = true,
                _ => {}
            }
        }
        if let Some(ov) = self.overlay.take() {
            self.overlay_key(ov, k);
            self.next_ask();
            return;
        }
        if self.tab == Tab::Changes && self.focus == Focus::Commit {
            return self.commit_key(k);
        }
        if self.search.bar.is_some() {
            return self.search_bar_key(k);
        }
        let state = State { tab: self.tab, focus: self.focus, compare: self.compare.is_some(), conflict: self.conflict_active() };
        if let Some(a) = self.keymap.resolve(&k, state) {
            self.act(a);
        }
    }

    /// Runs a bound action; what some of them mean depends on the tab and pane.
    fn act(&mut self, a: Action) {
        // the split side picked by a click applies to the Space right after it, nothing later
        if a != Action::Stage {
            self.changes.side = None;
        }
        let split = self.split_active();
        let changes = self.tab == Tab::Changes;
        let files_tab = self.tab == Tab::Files;
        let in_diff = self.focus == Focus::Diff;
        // these act on the diff and History state, which the Files tab hides
        if files_tab && matches!(a, Action::Difftool | Action::Split | Action::Wrap | Action::Whitespace | Action::PrevHunk | Action::NextHunk | Action::PrevFile | Action::NextFile | Action::Expand | Action::ExpandFile) {
            return;
        }
        let comparing = self.compare.is_some();
        match a {
            Action::ConflictOurs => self.conflict_resolve(Choice::Ours),
            Action::ConflictTheirs => self.conflict_resolve(Choice::Theirs),
            Action::ConflictBoth => self.conflict_resolve(Choice::Both),
            Action::ConflictNext => self.conflict_nav(1),
            Action::ConflictPrev => self.conflict_nav(-1),
            Action::ConflictUndo => self.conflict_undo(),
            Action::ConflictEdit => self.conflict_edit(),
            Action::Quit => self.request_quit(),
            Action::ChangesTab => self.set_tab(Tab::Changes),
            Action::HistoryTab => self.set_tab(Tab::History),
            Action::FilesTab => self.set_tab(Tab::Files),
            Action::RevealSecret => self.toggle_reveal(),
            Action::OpenEditor => self.files_edit(),
            Action::FilesCollapse => self.files_collapse(),
            Action::FilesExpand => self.files_expand(),
            Action::Fetch => self.start_net(crate::msg::NetOp::Fetch),
            Action::Pull => self.start_net(crate::msg::NetOp::Pull),
            Action::Push => self.start_net(crate::msg::NetOp::Push),
            Action::Cancel => self.cancel_net(),
            Action::OpenPr => self.open_pr(),
            Action::Difftool => self.open_difftool(),
            Action::Theme => self.open_theme_picker(),
            Action::Branches => self.open_switcher(),
            Action::Stashes => self.open_stashes(),
            Action::Operation => self.open_operation(),
            Action::StashPush => self.open_stash_name(),
            Action::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Action::ErrorDetails => {
                if self.toast.as_ref().is_none_or(|t| !t.error) {
                    self.show_background_problem();
                }
                if self.toast.is_some() {
                    self.overlay = Some(Overlay::ErrorDetail);
                }
            }
            Action::CompareBehind => self.compare_tab(-1),
            Action::CompareAhead => self.compare_tab(1),
            Action::CommitBox => {
                self.focus_commit_or_true();
            }
            Action::Amend => self.toggle_amend(),
            Action::UndoCommit => self.undo_commit(),
            Action::Stage if in_diff => self.toggle_lines(),
            Action::Stage => self.toggle_file(self.changes.sel),
            Action::StageAll if in_diff => self.toggle_current_file(),
            Action::StageAll => self.toggle_all_files(),
            Action::Discard if in_diff => self.confirm_discard_lines(),
            Action::Discard => self.confirm_discard_file(),
            Action::Filter => {
                self.changes.filter = self.changes.filter.next();
                self.changes.sel = 0;
                self.changes.scroll = 0;
                self.request_change_diff();
            }
            Action::LineRange => {
                let c = self.diff.as_ref().map_or(0, |d| d.cursor);
                self.changes.visual = if self.changes.visual.is_some() { None } else { Some(c) };
            }
            Action::StageHunk => self.toggle_hunk(),
            // history-only: compare mode shows its own lists
            Action::Search | Action::NextMatch | Action::PrevMatch | Action::Range | Action::Scope if comparing => {}
            Action::Search => self.open_search(),
            Action::NextMatch => self.search_step(true),
            Action::PrevMatch => self.search_step(false),
            Action::Range => self.toggle_range(),
            Action::Compare => self.open_branch_picker(),
            Action::Tree => self.toggle_tree(),
            Action::Scope => self.toggle_scope(),
            Action::CopySha => {
                if let Some(id) = self.selected_id() {
                    self.copy(&id.short(7));
                }
            }
            Action::CopyFullSha => {
                if let Some(id) = self.selected_id() {
                    self.copy(&id.to_hex());
                }
            }
            Action::Header => self.header_expanded = !self.header_expanded,
            Action::Dates => self.date_mode = self.date_mode.next(),
            Action::Density => {
                self.density = match self.density {
                    Density::Compact => Density::Comfortable,
                    Density::Comfortable => Density::Compact,
                };
                self.ensure_list_visible();
                self.request_visible_rows();
            }
            Action::ScrollLeft => self.hscroll(-8),
            Action::ScrollRight => self.hscroll(8),
            Action::PrevHunk => self.hunk(-1, split),
            Action::NextHunk => self.hunk(1, split),
            Action::PrevFile if changes => self.select_change(self.changes.sel.saturating_sub(1)),
            Action::NextFile if changes => self.select_change(self.changes.sel + 1),
            Action::PrevFile => self.step_file(false),
            Action::NextFile => self.step_file(true),
            Action::Expand => {
                if let Some(d) = self.diff.as_mut() {
                    d.expand_near_cursor(split);
                }
                self.ensure_diff_visible();
            }
            Action::ExpandFile => {
                if let Some(d) = self.diff.as_mut() {
                    d.toggle_whole_file(split);
                }
                self.ensure_diff_visible();
            }
            Action::Split => {
                self.split_pref = Some(!self.split_wanted());
                let now = self.split_active();
                if let Some(d) = self.diff.as_mut() {
                    d.remap_cursor(split, now);
                }
                self.ensure_diff_visible();
            }
            Action::Whitespace => {
                self.ws = match self.ws {
                    WsMode::Show => WsMode::IgnoreAll,
                    WsMode::IgnoreAll => WsMode::IgnoreAmount,
                    WsMode::IgnoreAmount => WsMode::Show,
                };
                self.refresh_diff();
            }
            Action::Wrap => {
                self.wrap = !self.wrap;
                if let Some(d) = self.diff.as_mut() {
                    d.hscroll = 0;
                }
                self.ensure_diff_visible();
            }
            Action::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                if self.fullscreen {
                    self.focus = Focus::Diff;
                }
                self.ensure_diff_visible();
            }
            Action::Narrower => self.resize_focused(-4),
            Action::Wider => self.resize_focused(4),
            Action::Down => self.move_any(Move::Step(1)),
            Action::Up => self.move_any(Move::Step(-1)),
            Action::HalfDown => self.move_any(Move::Half(1)),
            Action::HalfUp => self.move_any(Move::Half(-1)),
            Action::PageDown => self.move_any(Move::Page(1)),
            Action::PageUp => self.move_any(Move::Page(-1)),
            Action::Top => self.move_any(Move::Top),
            Action::Bottom => self.move_any(Move::Bottom),
            // Changes has two panes: Tab flips between them
            Action::NextPane | Action::PrevPane if changes || files_tab => self.focus = if in_diff { Focus::Files } else { Focus::Diff },
            Action::NextPane => self.cycle_focus(1),
            Action::PrevPane => self.cycle_focus(-1),
            Action::Open if files_tab => {
                if !in_diff {
                    self.files_open();
                }
            }
            Action::Open if changes && in_diff => {
                let hidden = self.diff.as_ref().is_some_and(|d| {
                    matches!(d.diff.class, gitty_core::diff::classify::FileClass::LargeText { .. } | gitty_core::diff::classify::FileClass::Generated { .. })
                });
                if hidden {
                    self.changes.force_text = true;
                    self.request_change_diff();
                }
            }
            Action::Open if changes => self.focus = Focus::Diff,
            Action::Open => {
                if !(self.focus == Focus::Files && self.toggle_dir()) {
                    self.drill_in();
                }
            }
            Action::Back if changes && in_diff && self.changes.visual.is_some() => self.changes.visual = None,
            Action::Back if changes && in_diff && !self.fullscreen => self.focus = Focus::Files,
            Action::Back if (changes || files_tab) && !in_diff => {}
            Action::Back if self.focus == Focus::History && comparing => self.leave_compare(),
            Action::Back if self.focus == Focus::History && self.range_anchor.is_some() => self.end_range(),
            Action::Back if self.focus == Focus::History && self.search_active() => self.clear_search(),
            Action::Back => self.back(),
        }
    }

    /// Moves in the Changes file list, or in the focused pane.
    fn move_any(&mut self, m: Move) {
        if self.tab == Tab::Files && self.focus != Focus::Diff {
            let t = target(self.files_tab.sel, self.files_tab.rows.len(), self.files_capacity(), m);
            return self.select_files_row(t);
        }
        if self.tab == Tab::Changes && self.focus != Focus::Diff {
            let n = self.changes.visible().len();
            let t = target(self.changes.sel, n, self.files_capacity(), m);
            return self.select_change(t);
        }
        self.move_focused(m);
    }

    fn overlay_key(&mut self, ov: Overlay, k: KeyEvent) {
        match ov {
            Overlay::ThemePicker { mut sel, original } => {
                let names = self.registry.names();
                match k.code {
                    KeyCode::Char('j') | KeyCode::Down => sel = (sel + 1).min(names.len().saturating_sub(1)),
                    KeyCode::Char('k') | KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Enter => {
                        if let Some(name) = names.get(sel) {
                            self.config.theme = name.clone();
                            if let Some(p) = &self.config_path
                                && let Err(e) = Config::save_theme(p, name) {
                                    self.toast = Some(Toast { what: "saving theme".into(), detail: e.to_string(), error: true });
                                }
                        }
                        return;
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('T') => {
                        self.theme = original;
                        return;
                    }
                    _ => {}
                }
                if let Some(name) = names.get(sel) {
                    match self.registry.resolve(name, self.depth, self.config.emph_alpha) {
                        Ok(t) => self.theme = t,
                        Err(e) => self.toast = Some(Toast { what: format!("theme {name}"), detail: format!("{e:#}"), error: true }),
                    }
                }
                self.overlay = Some(Overlay::ThemePicker { sel, original });
            }
            Overlay::Confirm { op, .. } if matches!(k.code, KeyCode::Enter | KeyCode::Char('y')) => self.write(op),
            Overlay::Confirm { .. } if matches!(k.code, KeyCode::Esc | KeyCode::Char('n' | 'q')) => {}
            Overlay::Confirm { .. } => self.overlay = Some(ov),
            Overlay::Prompt { ask, input } => self.prompt_key(ask, input, k),
            Overlay::BranchPicker { query, sel } => self.picker_key(query, sel, k),
            Overlay::Switcher { query, sel } => self.switcher_key(query, sel, k),
            Overlay::NameInput { kind, input } => self.name_key(kind, input, k),
            Overlay::DirtySwitch { name, remote, merge } => self.dirty_key(name, remote, merge, k),
            Overlay::Stashes { sel } => self.stashes_key(sel, k),
            Overlay::InProgress => self.operation_key(k),
            Overlay::Quit { .. } => match k.code {
                KeyCode::Char('y') | KeyCode::Enter => self.quit_now(),
                KeyCode::Char('n') | KeyCode::Esc => {}
                _ => self.overlay = Some(ov),
            },
            Overlay::ForcePush { plan } => match k.code {
                KeyCode::Enter => self.start_force_push(plan),
                KeyCode::Esc | KeyCode::Char('n' | 'q') => {}
                _ => self.overlay = Some(Overlay::ForcePush { plan }),
            },
            Overlay::Diverged => match k.code {
                KeyCode::Char('m') => self.start_net(crate::msg::NetOp::PullMerge),
                KeyCode::Char('r') => self.start_net(crate::msg::NetOp::PullRebase),
                KeyCode::Esc | KeyCode::Char('n' | 'q') => {}
                _ => self.overlay = Some(ov),
            },
            Overlay::Log { .. } if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) => {}
            Overlay::Log { .. } => self.overlay = Some(ov),
            Overlay::Help { scroll } => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?') => {}
                KeyCode::Char('j') | KeyCode::Down => self.overlay = Some(Overlay::Help { scroll: (scroll + 1).min(crate::ui::overlay::help_lines(self).len()) }),
                KeyCode::Char('k') | KeyCode::Up => self.overlay = Some(Overlay::Help { scroll: scroll.saturating_sub(1) }),
                KeyCode::PageDown | KeyCode::Char(' ') => self.overlay = Some(Overlay::Help { scroll: (scroll + 10).min(crate::ui::overlay::help_lines(self).len()) }),
                KeyCode::PageUp => self.overlay = Some(Overlay::Help { scroll: scroll.saturating_sub(10) }),
                KeyCode::Char('g') | KeyCode::Home => self.overlay = Some(Overlay::Help { scroll: 0 }),
                _ => self.overlay = Some(ov),
            },
            Overlay::ErrorDetail => {
                if !matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?' | '!')) {
                    self.overlay = Some(ov);
                } else if matches!(ov, Overlay::ErrorDetail) {
                    self.toast = None;
                }
            }
        }
    }

    fn open_theme_picker(&mut self) {
        let names = self.registry.names();
        let sel = names.iter().position(|n| *n == self.theme.name).unwrap_or(0);
        self.overlay = Some(Overlay::ThemePicker { sel, original: self.theme.clone() });
    }

    fn move_focused(&mut self, m: Move) {
        match self.focus {
            Focus::History if self.compare.is_some() => {
                let cur = self.compare.as_ref().and_then(|c| c.selected()).unwrap_or(0);
                let t = target(cur, self.compare_len(), self.list_capacity().saturating_sub(1), m);
                self.compare_select(t);
            }
            Focus::History => {
                let t = target(self.selected, self.history_len, self.list_capacity(), m);
                self.select(t);
            }
            Focus::Files => {
                let n = self.file_rows().len();
                let t = target(self.file_cursor(), n, self.files_capacity(), m);
                self.select_file_row(t);
            }
            Focus::Commit => {}
            // the Files viewer scrolls by lines; it has no cursor
            Focus::Diff if self.tab == Tab::Files => {
                let max = self.view_lines().saturating_sub(self.diff_capacity());
                self.files_tab.vscroll = target(self.files_tab.vscroll, max + 1, self.diff_capacity(), m);
            }
            // the conflict view scrolls by lines too
            Focus::Diff if self.conflict_active() => {
                let cap = self.conflict_capacity();
                if let Some(v) = self.changes.conflict.as_mut() {
                    let max = v.lines().saturating_sub(cap);
                    v.vscroll = target(v.vscroll, max + 1, cap, m);
                }
            }
            Focus::Diff => {
                let split = self.split_active();
                let (cap, wrap) = (self.diff_capacity(), self.diff_wrap());
                if let Some(d) = self.diff.as_mut() {
                    // wrapped rows are taller than one line: page by what fits on screen
                    let m = match (wrap, m) {
                        (Some(_), Move::Page(n)) => Move::Step(n.signum() * d.rows_in_lines(d.cursor, cap, n, split, wrap) as i64),
                        (Some(_), Move::Half(n)) => Move::Step(n.signum() * d.rows_in_lines(d.cursor, (cap / 2).max(1), n, split, wrap) as i64),
                        _ => m,
                    };
                    d.cursor = target(d.cursor, d.rows(split), cap, m);
                }
                self.ensure_diff_visible();
            }
        }
    }

    fn cycle_focus(&mut self, dir: i64) {
        let order: &[Focus] = match self.tab {
            Tab::Changes if self.fullscreen => &[Focus::Diff],
            Tab::Changes | Tab::Files => &[Focus::Files, Focus::Diff],
            Tab::History => layout::tab_order(self.mode(), self.fullscreen),
        };
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as i64;
        self.focus = order[(i + dir).rem_euclid(order.len() as i64) as usize];
    }

    fn drill_in(&mut self) {
        match self.focus {
            Focus::History => self.focus = Focus::Files,
            Focus::Files => self.focus = Focus::Diff,
            Focus::Diff => self.force_show(),
            Focus::Commit => {}
        }
    }

    fn back(&mut self) {
        if self.fullscreen {
            self.fullscreen = false;
            return;
        }
        match self.focus {
            Focus::Diff | Focus::Commit => self.focus = Focus::Files,
            Focus::Files => self.focus = Focus::History,
            Focus::History => self.toast = None,
        }
    }

    fn hscroll(&mut self, by: i32) {
        // the Files viewer clips long lines (no wrapping) and scrolls sideways instead
        if self.tab == Tab::Files {
            self.files_tab.scroll_sideways(by);
            return;
        }
        if self.conflict_active() {
            return self.conflict_scroll_sideways(by);
        }
        if let Some(d) = self.diff.as_mut().filter(|_| !self.wrap) {
            let max = i32::from(d.max_hscroll(self.config.tab_size));
            d.hscroll = (i32::from(d.hscroll) + by).clamp(0, max) as u16;
        }
    }

    fn hunk(&mut self, dir: i32, split: bool) {
        if self.conflict_active() {
            return self.conflict_nav(i64::from(dir));
        }
        if let Some(d) = self.diff.as_mut() {
            d.next_hunk(split, dir);
        }
        self.ensure_diff_visible();
    }

    fn resize_focused(&mut self, by: i32) {
        let p = self.panes();
        let add = |v: u16, by: i32| (i32::from(v) + by).clamp(0, 1000) as u16;
        match (self.mode(), self.focus) {
            (Mode::Narrow, _) => return,
            (_, Focus::History) => {
                let cur = p.history.map_or(0, |r| r.width);
                self.ui_state.history_width = Some(add(cur, by));
            }
            (Mode::Wide, Focus::Files) => {
                let cur = p.files.map_or(0, |r| r.width);
                self.ui_state.files_width = Some(add(cur, by).clamp(MIN_FILES, MAX_FILES));
            }
            (Mode::Medium, Focus::Files) => {
                let cur = p.files.map_or(0, |r| r.height);
                self.ui_state.files_height = Some(add(cur, by / 2).max(2));
            }
            _ => return,
        }
        self.save_state();
    }

    pub fn handle_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        self.dirty = true;
        self.settle_files();
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(x, y, m.modifiers),
            MouseEventKind::Drag(MouseButton::Left) if self.changes.gutter_drag => self.gutter_drag(y),
            MouseEventKind::Drag(MouseButton::Left) => self.drag(x, y),
            MouseEventKind::Up(MouseButton::Left) => {
                if self.hits.dragging.take().is_some() {
                    self.save_state();
                }
                if std::mem::take(&mut self.changes.gutter_drag) {
                    self.toggle_lines();
                }
            }
            // sideways swipes, and Shift+wheel for terminals that report none
            MouseEventKind::ScrollLeft => self.hwheel(x, y, -8),
            MouseEventKind::ScrollRight => self.hwheel(x, y, 8),
            MouseEventKind::ScrollDown if m.modifiers.contains(KeyModifiers::SHIFT) => self.hwheel(x, y, 8),
            MouseEventKind::ScrollUp if m.modifiers.contains(KeyModifiers::SHIFT) => self.hwheel(x, y, -8),
            MouseEventKind::ScrollDown => self.wheel(x, y, 3),
            MouseEventKind::ScrollUp => self.wheel(x, y, -3),
            _ => {}
        }
    }

    /// Sideways wheel over the Files viewer or the diff: the same step as `h` and `l`.
    fn hwheel(&mut self, x: u16, y: u16, by: i32) {
        if inside(self.hits.panes.diff, x, y).is_none() {
            return;
        }
        self.hscroll(by);
    }

    fn click(&mut self, x: u16, y: u16, mods: KeyModifiers) {
        if self.overlay.is_some() {
            return;
        }
        // clicking away from the search bar abandons the query being typed; a click on its row
        // (the bottom bar) does nothing
        if self.search.bar.is_some() && y == self.hits.panes.bottom.y {
            return;
        }
        self.search.bar = None;
        let double = self.note_click(x, y);
        // a double-click on the Files tab edits the file: its first click already selected it
        // (only on a row: elsewhere it is two ordinary clicks, e.g. on a tab)
        if double && self.tab == Tab::Files && self.files_row_at(x, y).is_some() {
            return self.files_double_click(x, y);
        }
        self.click_once(x, y, mods);
        if !double || self.tab == Tab::Files {
            return;
        }
        // double-click: open the file, except on Changes' checkboxes and gutters (they toggle)
        let on_diff = inside(self.hits.diff_rows, x, y).is_some();
        let on_files = inside(self.hits.files_rows, x, y).is_some();
        let (og, ng) = (self.hits.diff_old_gutter, self.hits.diff_new_gutter);
        let toggles = self.tab == Tab::Changes
            && ((on_files && x < self.hits.files_rows.map_or(0, |r| r.x + 4)) || (on_diff && ((og.0..og.0 + og.1).contains(&x) || (ng.0..ng.0 + ng.1).contains(&x))));
        if (on_diff || on_files) && !toggles {
            self.open_shown_file(on_diff);
        }
    }

    fn click_once(&mut self, x: u16, y: u16, mods: KeyModifiers) {
        if inside(self.hits.pr_badge, x, y).is_some() {
            self.open_url = self.pr_badge.as_ref().map(|(_, info)| info.url.clone());
            return;
        }
        if let Some(tab) = self.hits.tabs.iter().find(|(r, _)| r.contains(Position { x, y })).map(|t| t.1) {
            self.set_tab(tab);
            return;
        }
        if let Some(sep) = self.hits.panes.seps.iter().find(|(r, _)| r.contains(Position { x, y })).map(|s| s.1) {
            self.hits.dragging = Some(sep);
            return;
        }
        if self.tab == Tab::Files {
            return self.files_click(x, y);
        }
        if self.tab == Tab::Changes {
            return self.changes_click(x, y);
        }
        if let Some(r) = inside(self.hits.history_rows, x, y) {
            self.focus = Focus::History;
            let i = self.hits.history_first + ((y - r.y) / self.hits.history_row_h.max(1)) as usize;
            if self.compare.is_some() {
                self.compare_select(i);
            } else if i < self.history_len {
                if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) {
                    self.extend_range(i);
                } else {
                    self.range_anchor = None;
                    self.select(i);
                }
            }
        } else if let Some(r) = inside(self.hits.files_rows, x, y) {
            self.focus = Focus::Files;
            let i = self.hits.files_first + (y - r.y) as usize;
            if i < self.file_rows().len() {
                self.select_file_row(i);
                // a click on a directory row opens or closes it
                self.toggle_dir();
            }
        } else if let Some(r) = inside(self.hits.diff_rows, x, y) {
            self.focus = Focus::Diff;
            let split = self.split_active();
            let Some(&i) = self.hits.diff_lines.get((y - r.y) as usize) else { return };
            let (og, ng) = (self.hits.diff_old_gutter, self.hits.diff_new_gutter);
            let Some(d) = self.diff.as_mut() else { return };
            let gap = match d.vrow(i, split) {
                Some(VRow::Row(Row::Gap { gap, .. }) | VRow::Split(SplitRow::Gap { gap, .. })) => Some(gap),
                Some(_) => None,
                None => return,
            };
            d.cursor = i;
            if let Some(gap) = gap {
                let e = if (og.0..og.0 + og.1).contains(&x) {
                    Expand::Up(gap)
                } else if (ng.0..ng.0 + ng.1).contains(&x) {
                    Expand::Down(gap)
                } else {
                    Expand::All(gap)
                };
                d.expand(e, split);
            }
        }
    }

    /// Changes tab: checkboxes toggle files; the diff gutter toggles lines (drag for a range).
    fn changes_click(&mut self, x: u16, y: u16) {
        if inside(self.hits.commit_button, x, y).is_some() {
            return self.commit();
        }
        if let Some((_, f)) = self.hits.commit_fields.iter().find(|(r, _)| r.contains(Position { x, y })) {
            self.focus = Focus::Commit;
            self.changes.commit.field = *f;
            return;
        }
        if let Some(r) = self.hits.files_rows {
            let in_box = x >= r.x && x < r.x + 4;
            if y + 1 == r.y && r.contains(Position { x, y: r.y }) && in_box {
                self.focus = Focus::Files;
                return self.toggle_all_files();
            }
            if r.contains(Position { x, y }) {
                self.focus = Focus::Files;
                let i = self.changes.scroll + (y - r.y) as usize;
                if i < self.changes.visible().len() {
                    self.select_change(i);
                    if in_box {
                        self.toggle_file(i);
                    }
                }
                return;
            }
        }
        if let Some(r) = inside(self.hits.diff_rows, x, y) {
            self.focus = Focus::Diff;
            let Some(&i) = self.hits.diff_lines.get((y - r.y) as usize) else { return };
            let (og, ng) = (self.hits.diff_old_gutter, self.hits.diff_new_gutter);
            let in_gutter = (og.0..og.0 + og.1).contains(&x) || (ng.0..ng.0 + ng.1).contains(&x);
            let split = self.split_active();
            // split view: deletions on the left half, additions on the right
            self.changes.side = split.then(|| if x < r.x + r.width.saturating_sub(1) / 2 { Side::Old } else { Side::New });
            let Some(d) = self.diff.as_mut() else { return };
            d.cursor = i;
            let is_edge = |j: usize| matches!(d.vrow(j, split), None | Some(VRow::Header(_) | VRow::Row(Row::Gap { .. }) | VRow::Split(SplitRow::Gap { .. })));
            if x == r.x && is_edge(i) && !is_edge(i + 1) {
                // the handle: stage (or unstage) the hunk below the header
                d.cursor = (i + 1).min(d.rows(split).saturating_sub(1));
                return self.toggle_hunk();
            }
            let gap = match d.vrow(i, split) {
                Some(VRow::Row(Row::Gap { gap, .. }) | VRow::Split(SplitRow::Gap { gap, .. })) => Some(gap),
                _ => None,
            };
            match gap {
                Some(gap) => {
                    let e = if (og.0..og.0 + og.1).contains(&x) {
                        Expand::Up(gap)
                    } else if (ng.0..ng.0 + ng.1).contains(&x) {
                        Expand::Down(gap)
                    } else {
                        Expand::All(gap)
                    };
                    d.expand(e, split);
                }
                None if in_gutter => {
                    self.changes.visual = Some(i);
                    self.changes.gutter_drag = true;
                }
                None => self.changes.visual = None,
            }
        }
    }

    /// Extends a gutter selection; at the first or last row the cursor steps on past the
    /// screen, so the diff scrolls a row per drag event.
    fn gutter_drag(&mut self, y: u16) {
        let Some(r) = self.hits.diff_rows else { return };
        let split = self.split_active();
        let row = (y.clamp(r.y, r.bottom().saturating_sub(1)) - r.y) as usize;
        let hit = self.hits.diff_lines.get(row).copied();
        let Some(d) = self.diff.as_mut() else { return };
        let last = d.rows(split).saturating_sub(1);
        // the hit regions are from the last frame, which may be scrolled since: never step back
        d.cursor = if y + 1 >= r.bottom() {
            (hit.map_or(d.cursor, |i| i.max(d.cursor)) + 1).min(last)
        } else if y <= r.y {
            hit.map_or(d.cursor, |i| i.min(d.cursor)).saturating_sub(1)
        } else if let Some(i) = hit {
            i
        } else {
            return;
        };
        self.ensure_diff_visible();
    }

    fn drag(&mut self, x: u16, y: u16) {
        let Some(sep) = self.hits.dragging else { return };
        let p = self.panes();
        match sep {
            Sep::History => self.ui_state.history_width = Some(x.saturating_sub(p.body.x)),
            Sep::Changes => self.ui_state.changes_width = Some(x.saturating_sub(p.body.x)),
            Sep::Files => {
                if let Some(f) = p.files {
                    self.ui_state.files_width = Some(x.saturating_sub(f.x).clamp(MIN_FILES, MAX_FILES));
                }
            }
            Sep::FilesBelow => {
                if let Some(f) = p.files {
                    self.ui_state.files_height = Some(y.saturating_sub(f.y).max(2));
                }
            }
        }
    }

    fn wheel(&mut self, x: u16, y: u16, by: i64) {
        let scroll = |v: usize, max: usize| (v as i64 + by).clamp(0, max as i64) as usize;
        if self.tab == Tab::Files {
            if inside(self.hits.panes.files, x, y).is_some() {
                let max = self.files_tab.rows.len().saturating_sub(self.files_capacity());
                self.files_tab.scroll = scroll(self.files_tab.scroll, max);
            } else if inside(self.hits.panes.diff, x, y).is_some() {
                let max = self.view_lines().saturating_sub(self.diff_capacity());
                self.files_tab.vscroll = scroll(self.files_tab.vscroll, max);
            }
            return;
        }
        if inside(self.hits.panes.history, x, y).is_some() {
            let max = self.history_len.saturating_sub(self.list_capacity());
            self.list_scroll = scroll(self.list_scroll, max);
            self.request_visible_rows();
        } else if inside(self.hits.panes.files, x, y).is_some() {
            let n = self.file_rows().len();
            self.file_scroll = scroll(self.file_scroll, n.saturating_sub(self.files_capacity()));
        } else if inside(self.hits.panes.diff, x, y).is_some() && self.conflict_active() {
            let cap = self.conflict_capacity();
            if let Some(v) = self.changes.conflict.as_mut() {
                v.vscroll = scroll(v.vscroll, v.lines().saturating_sub(cap));
            }
        } else if inside(self.hits.panes.diff, x, y).is_some() {
            let split = self.split_active();
            let cap = self.diff_capacity();
            if let Some(d) = self.diff.as_mut() {
                d.scroll = scroll(d.scroll, d.rows(split).saturating_sub(cap));
            }
        }
    }
}
