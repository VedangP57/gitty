//! Keyboard and mouse handling.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use gitty_core::diff::ops::WsMode;
use gitty_core::diff::view::{Expand, Row, SplitRow};
use ratatui::layout::{Position, Rect};

use super::diffstate::VRow;
use super::{App, Focus, Overlay, Tab, Toast};
use crate::config::{Config, Density};
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
        if self.toast.as_ref().is_some_and(|t| !t.error) {
            self.toast = None;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match k.code {
                KeyCode::Char('c') => return self.quit = true,
                KeyCode::Char('z') => return self.suspend = true,
                _ => {}
            }
        }
        if let Some(ov) = self.overlay.take() {
            return self.overlay_key(ov, k);
        }
        match k.code {
            KeyCode::Char('q') => return self.quit = true,
            KeyCode::Char('1') => return self.set_tab(Tab::Changes),
            KeyCode::Char('2') => return self.set_tab(Tab::History),
            KeyCode::Char('T') => return self.open_theme_picker(),
            KeyCode::Char('?') => return self.overlay = Some(Overlay::Help),
            KeyCode::Char('!') if self.toast.is_some() => return self.overlay = Some(Overlay::ErrorDetail),
            _ => {}
        }
        if self.tab == Tab::Changes {
            return self.changes_key(k);
        }
        let split = self.split_active();
        match (k.code, ctrl) {
            (KeyCode::Char('d'), true) => self.move_focused(Move::Half(1)),
            (KeyCode::Char('u'), true) => self.move_focused(Move::Half(-1)),
            (KeyCode::Char('f'), true) | (KeyCode::PageDown, _) => self.move_focused(Move::Page(1)),
            (KeyCode::Char('b'), true) | (KeyCode::PageUp, _) => self.move_focused(Move::Page(-1)),
            (KeyCode::Char('j') | KeyCode::Down, false) => self.move_focused(Move::Step(1)),
            (KeyCode::Char('k') | KeyCode::Up, false) => self.move_focused(Move::Step(-1)),
            (KeyCode::Char('g') | KeyCode::Home, _) => self.move_focused(Move::Top),
            (KeyCode::Char('G') | KeyCode::End, _) => self.move_focused(Move::Bottom),
            (KeyCode::Tab, _) => self.cycle_focus(1),
            (KeyCode::BackTab, _) => self.cycle_focus(-1),
            (KeyCode::Enter, _) => self.drill_in(),
            (KeyCode::Esc, _) => self.back(),
            (KeyCode::Char('h') | KeyCode::Left, false) => self.hscroll(-8),
            (KeyCode::Char('l') | KeyCode::Right, false) => self.hscroll(8),
            (KeyCode::Char('['), _) => self.hunk(-1, split),
            (KeyCode::Char(']'), _) => self.hunk(1, split),
            (KeyCode::Char('{'), _) => self.select_file(self.file_sel.saturating_sub(1)),
            (KeyCode::Char('}'), _) => self.select_file(self.file_sel + 1),
            (KeyCode::Char('e'), _) => {
                if let Some(d) = self.diff.as_mut() {
                    d.expand_near_cursor(split);
                }
                self.ensure_diff_visible();
            }
            (KeyCode::Char('E'), _) => {
                if let Some(d) = self.diff.as_mut() {
                    d.toggle_whole_file(split);
                }
                self.ensure_diff_visible();
            }
            (KeyCode::Char('s'), _) => {
                self.split_pref = Some(!split);
                if let Some(d) = self.diff.as_mut() {
                    d.remap_cursor(split, !split);
                }
                self.ensure_diff_visible();
            }
            (KeyCode::Char('w'), _) => {
                self.ws = match self.ws {
                    WsMode::Show => WsMode::IgnoreAll,
                    WsMode::IgnoreAll => WsMode::IgnoreAmount,
                    WsMode::IgnoreAmount => WsMode::Show,
                };
                self.refresh_diff();
            }
            (KeyCode::Char('W'), _) => {
                self.wrap = !self.wrap;
                if let Some(d) = self.diff.as_mut() {
                    d.hscroll = 0;
                }
                self.ensure_diff_visible();
            }
            (KeyCode::Char('F'), _) => {
                self.fullscreen = !self.fullscreen;
                if self.fullscreen {
                    self.focus = Focus::Diff;
                }
                self.ensure_diff_visible();
            }
            (KeyCode::Char('o'), _) => self.header_expanded = !self.header_expanded,
            (KeyCode::Char('D'), _) => self.date_mode = self.date_mode.next(),
            (KeyCode::Char('z'), _) => {
                self.density = match self.density {
                    Density::Compact => Density::Comfortable,
                    Density::Comfortable => Density::Compact,
                };
                self.ensure_list_visible();
                self.request_visible_rows();
            }
            (KeyCode::Char('r'), _) => self.toggle_scope(),
            (KeyCode::Char('y'), _) => {
                if let Some(id) = self.selected_id() {
                    self.copy(&id.short(7));
                }
            }
            (KeyCode::Char('Y'), _) => {
                if let Some(id) = self.selected_id() {
                    self.copy(&id.to_hex());
                }
            }
            (KeyCode::Char('<'), _) => self.resize_focused(-4),
            (KeyCode::Char('>'), _) => self.resize_focused(4),
            _ => {}
        }
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
            Overlay::Help | Overlay::ErrorDetail => {
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

    fn changes_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let m = match (k.code, ctrl) {
            (KeyCode::Char('d'), true) => Move::Half(1),
            (KeyCode::Char('u'), true) => Move::Half(-1),
            (KeyCode::Char('f'), true) | (KeyCode::PageDown, _) => Move::Page(1),
            (KeyCode::Char('b'), true) | (KeyCode::PageUp, _) => Move::Page(-1),
            (KeyCode::Char('j') | KeyCode::Down, false) => Move::Step(1),
            (KeyCode::Char('k') | KeyCode::Up, false) => Move::Step(-1),
            (KeyCode::Char('g') | KeyCode::Home, _) => Move::Top,
            (KeyCode::Char('G') | KeyCode::End, _) => Move::Bottom,
            _ => return,
        };
        let n = self.changes.visible().len();
        let t = target(self.changes.sel, n, self.files_capacity(), m);
        self.select_change(t);
    }

    fn move_focused(&mut self, m: Move) {
        match self.focus {
            Focus::History => {
                let t = target(self.selected, self.history_len, self.list_capacity(), m);
                self.select(t);
            }
            Focus::Files => {
                let n = self.files.as_ref().map_or(0, |f| f.len());
                let t = target(self.file_sel, n, self.files_capacity(), m);
                self.select_file(t);
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
        let order = layout::tab_order(self.mode(), self.fullscreen);
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as i64;
        self.focus = order[(i + dir).rem_euclid(order.len() as i64) as usize];
    }

    fn drill_in(&mut self) {
        match self.focus {
            Focus::History => self.focus = Focus::Files,
            Focus::Files => self.focus = Focus::Diff,
            Focus::Diff => self.force_show(),
        }
    }

    fn back(&mut self) {
        if self.fullscreen {
            self.fullscreen = false;
            return;
        }
        match self.focus {
            Focus::Diff => self.focus = Focus::Files,
            Focus::Files => self.focus = Focus::History,
            Focus::History => self.toast = None,
        }
    }

    fn hscroll(&mut self, by: i32) {
        if let Some(d) = self.diff.as_mut().filter(|_| !self.wrap) {
            d.hscroll = (i32::from(d.hscroll) + by).clamp(0, 10_000) as u16;
        }
    }

    fn hunk(&mut self, dir: i32, split: bool) {
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
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(x, y),
            MouseEventKind::Drag(MouseButton::Left) => self.drag(x, y),
            MouseEventKind::Up(MouseButton::Left) => {
                if self.hits.dragging.take().is_some() {
                    self.save_state();
                }
            }
            MouseEventKind::ScrollDown => self.wheel(x, y, 3),
            MouseEventKind::ScrollUp => self.wheel(x, y, -3),
            _ => {}
        }
    }

    fn click(&mut self, x: u16, y: u16) {
        if self.overlay.is_some() {
            return;
        }
        if let Some(tab) = self.hits.tabs.iter().find(|(r, _)| r.contains(Position { x, y })).map(|t| t.1) {
            self.tab = tab;
            return;
        }
        if self.tab == Tab::Changes {
            return;
        }
        if let Some(sep) = self.hits.panes.seps.iter().find(|(r, _)| r.contains(Position { x, y })).map(|s| s.1) {
            self.hits.dragging = Some(sep);
            return;
        }
        if let Some(r) = inside(self.hits.history_rows, x, y) {
            self.focus = Focus::History;
            let i = self.hits.history_first + ((y - r.y) / self.hits.history_row_h.max(1)) as usize;
            if i < self.history_len {
                self.select(i);
            }
        } else if let Some(r) = inside(self.hits.files_rows, x, y) {
            self.focus = Focus::Files;
            let i = self.hits.files_first + (y - r.y) as usize;
            if self.files.as_ref().is_some_and(|f| i < f.len()) {
                self.select_file(i);
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

    fn drag(&mut self, x: u16, y: u16) {
        let Some(sep) = self.hits.dragging else { return };
        let p = self.panes();
        match sep {
            Sep::History => self.ui_state.history_width = Some(x.saturating_sub(p.body.x)),
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
        if inside(self.hits.panes.history, x, y).is_some() {
            let max = self.history_len.saturating_sub(self.list_capacity());
            self.list_scroll = scroll(self.list_scroll, max);
            self.request_visible_rows();
        } else if inside(self.hits.panes.files, x, y).is_some() {
            let n = self.files.as_ref().map_or(0, |f| f.len());
            self.file_scroll = scroll(self.file_scroll, n.saturating_sub(self.files_capacity()));
        } else if inside(self.hits.panes.diff, x, y).is_some() {
            let split = self.split_active();
            let cap = self.diff_capacity();
            if let Some(d) = self.diff.as_mut() {
                d.scroll = scroll(d.scroll, d.rows(split).saturating_sub(cap));
            }
        }
    }
}
