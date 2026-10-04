//! Pane rectangles from the terminal size (spec §11.1).

use ratatui::layout::Rect;

use crate::config::UiState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// <120 cols: one pane at a time.
    Narrow,
    /// 120–159: history left; header, files and diff stacked right.
    Medium,
    /// ≥160: history, files, diff side by side; header spans files and diff.
    Wide,
}

impl Mode {
    pub fn of(width: u16) -> Mode {
        match width {
            0..120 => Mode::Narrow,
            120..160 => Mode::Medium,
            _ => Mode::Wide,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Focus {
    History,
    Files,
    Diff,
    /// Changes tab: the commit message editor.
    Commit,
}

/// A draggable boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sep {
    /// Vertical line right of the history pane.
    History,
    /// Vertical line right of the file list (wide layout).
    Files,
    /// The diff title row below the file list (medium layout).
    FilesBelow,
    /// Vertical line right of the Changes tab's left column.
    Changes,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Panes {
    pub top: Rect,
    pub bottom: Rect,
    pub body: Rect,
    pub history: Option<Rect>,
    pub header: Option<Rect>,
    pub files: Option<Rect>,
    pub diff: Option<Rect>,
    /// Changes tab: the commit box under the file list.
    pub commit: Option<Rect>,
    pub seps: Vec<(Rect, Sep)>,
}

pub struct LayoutInput<'a> {
    pub width: u16,
    pub height: u16,
    pub focus: Focus,
    pub fullscreen: bool,
    pub header_height: u16,
    pub file_count: usize,
    pub ui: &'a UiState,
}

pub const MIN_HISTORY: u16 = 30;
pub const MIN_FILES: u16 = 24;
pub const MAX_FILES: u16 = 60;
pub const MIN_DIFF: u16 = 40;

pub fn history_width(width: u16, ui: &UiState) -> u16 {
    let default = match Mode::of(width) {
        Mode::Medium => width * 2 / 5,
        _ if width >= 200 => width / 4,
        _ => width * 32 / 100,
    };
    let right_min = match Mode::of(width) {
        Mode::Wide => MIN_FILES + 1 + MIN_DIFF,
        _ => MIN_DIFF,
    };
    let max = width.saturating_sub(right_min + 1).max(MIN_HISTORY);
    ui.history_width.unwrap_or(default).clamp(MIN_HISTORY, max)
}

pub fn compute(i: &LayoutInput) -> Panes {
    let (w, h) = (i.width, i.height);
    let top = Rect::new(0, 0, w, h.min(1));
    let bottom = Rect::new(0, h.saturating_sub(1), w, u16::from(h >= 2));
    let body = Rect::new(0, top.height, w, h.saturating_sub(top.height + bottom.height));
    let mut p = Panes { top, bottom, body, ..Panes::default() };
    if body.height == 0 || body.width == 0 {
        return p;
    }
    let header_h = i.header_height.min(body.height / 2);
    if i.fullscreen {
        p.diff = Some(body);
        return p;
    }
    match Mode::of(w) {
        Mode::Narrow => match i.focus {
            Focus::History => p.history = Some(body),
            Focus::Files => {
                let (hd, rest) = split_v(body, header_h);
                p.header = Some(hd);
                p.files = Some(rest);
            }
            Focus::Diff | Focus::Commit => p.diff = Some(body),
        },
        Mode::Medium => {
            let hw = history_width(w, i.ui);
            let (hist, sep, right) = split_h(body, hw);
            p.history = Some(hist);
            p.seps.push((sep, Sep::History));
            let (hd, rest) = split_v(right, header_h);
            p.header = Some(hd);
            let wanted = (i.file_count as u16).saturating_add(1).min(rest.height * 35 / 100).max(3);
            let fh = i.ui.files_height.unwrap_or(wanted).clamp(2, rest.height.saturating_sub(3).max(2));
            let (files, diff) = split_v(rest, fh);
            p.files = Some(files);
            if diff.height > 0 {
                p.seps.push((Rect::new(diff.x, diff.y, diff.width, 1), Sep::FilesBelow));
            }
            p.diff = Some(diff);
        }
        Mode::Wide => {
            let hw = history_width(w, i.ui);
            let (hist, sep, right) = split_h(body, hw);
            p.history = Some(hist);
            p.seps.push((sep, Sep::History));
            let (hd, rest) = split_v(right, header_h);
            p.header = Some(hd);
            let pct = if w >= 200 { 22 } else { 30 };
            let default_fw = (right.width * pct / 100).clamp(MIN_FILES, MAX_FILES);
            let fw = i.ui.files_width.unwrap_or(default_fw).clamp(MIN_FILES, MAX_FILES.min(rest.width.saturating_sub(MIN_DIFF + 1)).max(MIN_FILES));
            let (files, sep2, diff) = split_h(rest, fw);
            p.files = Some(files);
            p.seps.push((sep2, Sep::Files));
            p.diff = Some(diff);
        }
    }
    p
}

/// Height of the Changes tab's commit box.
pub const COMMIT_HEIGHT: u16 = 8;

pub fn changes_width(width: u16, ui: &UiState) -> u16 {
    let max = width.saturating_sub(MIN_DIFF + 1).max(MIN_FILES);
    ui.changes_width.unwrap_or((width * 30 / 100).clamp(30, MAX_FILES)).clamp(MIN_FILES, max)
}

/// Changes tab: file list over the commit box on the left, the diff on the right. Narrow
/// terminals show the left column or the diff.
pub fn compute_changes(i: &LayoutInput) -> Panes {
    let (w, h) = (i.width, i.height);
    let top = Rect::new(0, 0, w, h.min(1));
    let bottom = Rect::new(0, h.saturating_sub(1), w, u16::from(h >= 2));
    let body = Rect::new(0, top.height, w, h.saturating_sub(top.height + bottom.height));
    let mut p = Panes { top, bottom, body, ..Panes::default() };
    if body.height == 0 || body.width == 0 {
        return p;
    }
    if i.fullscreen {
        p.diff = Some(body);
        return p;
    }
    let left = match Mode::of(w) {
        Mode::Narrow if i.focus == Focus::Diff => {
            p.diff = Some(body);
            return p;
        }
        Mode::Narrow => body,
        _ => {
            let (left, sep, right) = split_h(body, changes_width(w, i.ui));
            p.seps.push((sep, Sep::Changes));
            p.diff = Some(right);
            left
        }
    };
    let ch = COMMIT_HEIGHT.min(left.height / 2);
    let (files, commit) = split_v(left, left.height - ch);
    p.files = Some(files);
    p.commit = (commit.height > 0).then_some(commit);
    p
}

/// (left `w` cols, 1-col separator, rest).
fn split_h(r: Rect, w: u16) -> (Rect, Rect, Rect) {
    let w = w.min(r.width);
    let left = Rect::new(r.x, r.y, w, r.height);
    let sep_w = u16::from(r.width > w);
    let sep = Rect::new(r.x + w, r.y, sep_w, r.height);
    let right = Rect::new(r.x + w + sep_w, r.y, r.width - w - sep_w, r.height);
    (left, sep, right)
}

/// (top `h` rows, rest).
fn split_v(r: Rect, h: u16) -> (Rect, Rect) {
    let h = h.min(r.height);
    (Rect::new(r.x, r.y, r.width, h), Rect::new(r.x, r.y + h, r.width, r.height - h))
}

/// Panes in Tab order that this layout can show.
pub fn tab_order(mode: Mode, fullscreen: bool) -> &'static [Focus] {
    if fullscreen { &[Focus::Diff] } else {
        let _ = mode;
        &[Focus::History, Focus::Files, Focus::Diff]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp(w: u16, h: u16, focus: Focus) -> Panes {
        compute(&LayoutInput { width: w, height: h, focus, fullscreen: false, header_height: 3, file_count: 4, ui: &UiState::default() })
    }

    #[test]
    fn modes_by_width() {
        assert_eq!(Mode::of(119), Mode::Narrow);
        assert_eq!(Mode::of(120), Mode::Medium);
        assert_eq!(Mode::of(159), Mode::Medium);
        assert_eq!(Mode::of(160), Mode::Wide);
        assert_eq!(Mode::of(260), Mode::Wide);
    }

    #[test]
    fn narrow_shows_focused_pane_only() {
        let p = inp(100, 30, Focus::History);
        assert!(p.history.is_some() && p.files.is_none() && p.diff.is_none());
        let p = inp(100, 30, Focus::Files);
        assert!(p.header.is_some() && p.files.is_some() && p.history.is_none());
        let p = inp(100, 30, Focus::Diff);
        assert_eq!(p.diff, Some(Rect::new(0, 1, 100, 28)));
    }

    #[test]
    fn wide_panes_tile_the_body() {
        for w in [160u16, 180, 220, 300] {
            let p = inp(w, 40, Focus::History);
            let (hst, files, diff) = (p.history.unwrap(), p.files.unwrap(), p.diff.unwrap());
            assert_eq!(hst.width + 1 + files.width + 1 + diff.width, w, "{w}");
            assert!(diff.width >= MIN_DIFF);
            assert_eq!(p.header.unwrap().x, files.x);
            assert_eq!(p.header.unwrap().width, files.width + 1 + diff.width);
        }
    }

    #[test]
    fn medium_stacks_right_side() {
        let p = inp(140, 40, Focus::History);
        let (hd, files, diff) = (p.header.unwrap(), p.files.unwrap(), p.diff.unwrap());
        assert_eq!(hd.x, files.x);
        assert_eq!(files.x, diff.x);
        assert_eq!(hd.y + hd.height, files.y);
        assert_eq!(files.y + files.height, diff.y);
        assert_eq!(diff.y + diff.height, 39);
        assert_eq!(files.height, 5, "4 files + title");
    }

    #[test]
    fn tiny_sizes_never_panic() {
        for w in 0..=45 {
            for h in 0..=8 {
                for f in [Focus::History, Focus::Files, Focus::Diff] {
                    let p = inp(w, h, f);
                    for r in [p.history, p.header, p.files, p.diff].into_iter().flatten() {
                        assert!(r.right() <= w && r.bottom() <= h, "{w}x{h} {r:?}");
                    }
                }
            }
        }
        for w in [120u16, 160, 200] {
            for h in 0..=8 {
                let _ = inp(w, h, Focus::Diff);
            }
        }
    }

    #[test]
    fn changes_layout_tiles_and_drills() {
        let inp = |w, focus| compute_changes(&LayoutInput { width: w, height: 30, focus, fullscreen: false, header_height: 3, file_count: 4, ui: &UiState::default() });
        let p = inp(140, Focus::Files);
        let (files, commit, diff) = (p.files.unwrap(), p.commit.unwrap(), p.diff.unwrap());
        assert_eq!(files.width + 1 + diff.width, 140);
        assert_eq!((commit.x, commit.width, commit.height), (files.x, files.width, COMMIT_HEIGHT));
        assert_eq!(files.bottom(), commit.y);
        assert_eq!(commit.bottom(), diff.bottom());
        let p = inp(100, Focus::Files);
        assert!(p.diff.is_none() && p.files.unwrap().width == 100);
        let p = inp(100, Focus::Diff);
        assert!(p.files.is_none() && p.diff.unwrap().width == 100);
        for w in 0..=45 {
            for h in 0..=8 {
                let p = compute_changes(&LayoutInput { width: w, height: h, focus: Focus::Files, fullscreen: false, header_height: 3, file_count: 0, ui: &UiState::default() });
                for r in [p.files, p.commit, p.diff].into_iter().flatten() {
                    assert!(r.right() <= w && r.bottom() <= h);
                }
            }
        }
    }

    #[test]
    fn user_widths_are_clamped() {
        let ui = UiState { history_width: Some(5), files_width: Some(500), ..UiState::default() };
        let p = compute(&LayoutInput { width: 180, height: 40, focus: Focus::History, fullscreen: false, header_height: 3, file_count: 4, ui: &ui });
        assert_eq!(p.history.unwrap().width, MIN_HISTORY);
        assert!(p.files.unwrap().width <= MAX_FILES);
        assert!(p.diff.unwrap().width >= MIN_DIFF);
    }
}
