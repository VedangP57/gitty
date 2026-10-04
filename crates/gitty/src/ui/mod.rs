//! Rendering. Every pane writes its visible rows straight into the frame buffer.

pub mod bars;
pub mod commit_list;
pub mod diff;
pub mod file_list;
pub mod header;
pub mod layout;
pub mod overlay;
pub mod paint;

use ratatui::Frame;
use ratatui::style::Style;

use crate::app::{App, Tab};
use layout::Sep;
use paint::{centered, fill, text};

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
    let panes = app.panes();
    bars::top(app, buf, panes.top);
    bars::bottom(app, buf, panes.bottom);
    if app.tab == Tab::Changes {
        let b = panes.body;
        centered(buf, b, b.y + b.height / 3, "Changes", base.add_modifier(ratatui::style::Modifier::BOLD));
        centered(buf, b, b.y + b.height / 3 + 1, "Staging and committing arrive in a later milestone (M4).", base.fg(ui.muted));
    } else {
        if let Some(r) = panes.history {
            commit_list::draw(app, buf, r);
        }
        if let Some(r) = panes.header {
            header::draw(app, buf, r);
        }
        if let Some(r) = panes.files {
            file_list::draw(app, buf, r);
        }
        if let Some(r) = panes.diff {
            diff::draw(app, buf, r);
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
    }
    app.hits.panes = panes;
    overlay::draw(app, buf, area);
}
