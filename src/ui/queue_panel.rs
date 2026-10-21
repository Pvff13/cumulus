//! The queue side panel: what's playing and what's next. Drag entries to
//! reorder, × to remove, double-click to jump to one.

use eframe::egui::{self, Align, CursorIcon, Layout, Rect, RichText, Sense, Stroke, UiBuilder, vec2};

use super::App;
use super::rows;
use super::theme::{accent, hover, muted};
use crate::player::QueueItem;

const ENTRY_HEIGHT: f32 = 46.0;

enum Edit {
    Jump(usize),
    Remove(usize),
    Move { from: usize, to: usize },
}

impl App {
    pub(super) fn queue_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Queue");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Button::new("×").frame(false)).on_hover_text("Close").clicked() {
                    self.show_queue = false;
                }
            });
        });
        ui.add_space(6.0);
        if self.queue.is_empty() {
            ui.label(RichText::new("Nothing queued. Play something, or right-click a track → Add to queue.").color(muted()));
            return;
        }

        let pos = self.queue.position();
        let mut edit: Option<Edit> = None;
        ui.label(RichText::new("Now playing").small().color(muted()));
        if let Some(item) = self.queue.current().cloned() {
            let _ = self.entry(ui, &item, pos, true);
        }
        ui.add_space(8.0);
        let upcoming = self.queue.len() - pos - 1;
        ui.label(RichText::new(format!("Next up · {upcoming}")).small().color(muted()));
        if upcoming == 0 {
            let note = if self.autoplay {
                "Nothing after this track yet. Autoplay will add similar tracks."
            } else {
                "Nothing after this track. (Autoplay is off.)"
            };
            ui.label(RichText::new(note).color(muted()));
        }

        let mut missing = Vec::new();
        ui.spacing_mut().item_spacing.y = 2.0;
        egui::ScrollArea::vertical().id_salt("queue-list").auto_shrink(false).show_rows(
            ui,
            ENTRY_HEIGHT,
            upcoming,
            |ui, range| {
                for i in range {
                    let p = pos + 1 + i;
                    let Some(item) = self.queue.at(p).cloned() else { continue };
                    if item.track.is_none() {
                        missing.push(item.id);
                    }
                    if let Some(e) = self.entry(ui, &item, p, false) {
                        edit = Some(e);
                    }
                }
            },
        );
        self.fetch_queue_tracks(missing);

        match edit {
            Some(Edit::Jump(p)) => self.play_queue_at(p),
            Some(Edit::Remove(p)) => {
                self.queue.remove_at(p);
                self.player.set_next(self.queue.peek_next());
            }
            Some(Edit::Move { from, to }) => {
                self.queue.move_entry(from, to);
                self.player.set_next(self.queue.peek_next());
            }
            None => {}
        }
    }

    /// One queue entry at play-order position `p`.
    fn entry(&self, ui: &mut egui::Ui, item: &QueueItem, p: usize, current: bool) -> Option<Edit> {
        let sense = if current { Sense::click() } else { Sense::click_and_drag() };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ENTRY_HEIGHT), sense);
        let mut edit = None;
        if resp.hovered() || resp.dragged() {
            ui.painter().rect_filled(rect, 6.0, hover());
        }
        if !current {
            resp.dnd_set_drag_payload(p);
            if resp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            // Drop target: insert above or below this entry depending on the pointer.
            let below = ui.pointer_hover_pos().is_some_and(|pt| pt.y > rect.center().y);
            if resp.dnd_hover_payload::<usize>().is_some() {
                let y = if below { rect.bottom() } else { rect.top() };
                ui.painter().hline(rect.x_range(), y, Stroke::new(2.0, accent()));
            }
            if let Some(from) = resp.dnd_release_payload::<usize>() {
                let target = p + usize::from(below);
                let to = if *from < target { target - 1 } else { target };
                edit = Some(Edit::Move { from: *from, to });
            }
            if resp.double_clicked() {
                edit = Some(Edit::Jump(p));
            }
        }

        let inner = Rect::from_min_max(rect.min + vec2(6.0, 4.0), rect.max - vec2(6.0, 4.0));
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
        let ui = &mut child;
        if !current && ui.add(egui::Button::new(RichText::new("×").color(muted())).frame(false)).on_hover_text("Remove").clicked() {
            edit = Some(Edit::Remove(p));
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            if !current {
                ui.label(RichText::new("☰").color(muted())).on_hover_cursor(CursorIcon::Grab);
            }
            let art = item.track.as_ref().and_then(|t| t.artwork().map(str::to_string));
            let _ = rows::artwork(ui, &self.art, art.as_deref(), 34.0, "t67x67");
            ui.vertical(|ui| {
                let unplayable = self.queue.is_unplayable(item.id);
                match &item.track {
                    Some(t) => {
                        let mut title = RichText::new(&t.title);
                        if current {
                            title = title.color(accent()).strong();
                        } else if unplayable {
                            title = title.color(muted()).strikethrough();
                        }
                        ui.add(egui::Label::new(title).truncate().selectable(false));
                        ui.add(egui::Label::new(RichText::new(&t.user.username).small().color(muted())).truncate().selectable(false));
                    }
                    None => {
                        ui.label(RichText::new("Loading...").color(muted()));
                    }
                }
            });
        });
        edit
    }
}
