//! Rendering. Every pane writes its visible rows straight into the frame buffer.

pub mod bars;
pub mod changes;
pub mod commit_list;
pub mod diff;
pub mod file_list;
pub mod files;
pub mod header;
pub mod layout;
pub mod overlay;
pub mod paint;

use ratatui::Frame;
use ratatui::style::Style;

use crate::app::{App, Tab};
use layout::Sep;
use paint::{fill, text};

pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    let buf = f.buffer_mut();
    let ui = app.theme.ui.clone();
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, area, base);
    let dragging = app.hits.dragging;
    app.hits = crate::app::Hits { dragging, ..Default::default() };
    if area.width < 20 || area.height < 5 {
        text(buf, area.x, area.y, area.right(), "gitty: terminal too small", base);
        return;
    }
    app.settle_files();
    let panes = app.panes();
    bars::top(app, buf, panes.top);
    bars::bottom(app, buf, panes.bottom);
    if let Some(r) = panes.history {
        commit_list::draw(app, buf, r);
    }
    if let Some(r) = panes.header {
        header::draw(app, buf, r);
    }
    if let Some(r) = panes.files {
        if app.tab == Tab::Changes {
            changes::draw_files(app, buf, r);
        } else if app.tab == Tab::Files {
            files::draw_tree(app, buf, r);
        } else {
            file_list::draw(app, buf, r);
        }
    }
    if let Some(r) = panes.commit {
        changes::draw_commit(app, buf, r);
    }
    if let Some(r) = panes.diff {
        if app.tab == Tab::Files {
            files::draw_viewer(app, buf, r);
        } else {
            diff::draw(app, buf, r);
        }
    }
    for (r, sep) in &panes.seps {
        if *sep == Sep::FilesBelow {
            continue;
        }
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                buf[(x, y)].set_symbol("│").set_style(base.fg(ui.border));
            }
        }
    }
    app.hits.panes = panes;
    overlay::draw(app, buf, area);
}
