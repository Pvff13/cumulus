//! The Local files page: the user's own music folders and local playlists,
//! plus drag-and-drop of files and folders onto the window.

use std::path::Path;
use std::sync::Arc;

use eframe::egui::{self, RichText};

use super::rows::{self, RowCtx, TrackAction};
use super::theme::muted;
use super::{App, Msg};
use crate::api::models::Track;
use crate::local::{self, Entry, LocalPlaylist};
use crate::player::QueueItem;

/// Names of the local playlists, for the "Add to local playlist" menu
/// (which is drawn where the app state isn't at hand).
pub static PLAYLIST_NAMES: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

impl App {
    /// Rebuilds the track list after a scan or a library change.
    pub(super) fn local_changed(&mut self) {
        self.local_tracks = self.local_lib.tracks.iter().map(|t| Arc::new(t.to_track())).collect();
        self.local_lib.save();
    }

    pub(super) fn local_lists_changed(&mut self) {
        local::save_playlists(&self.local_lists);
        if let Ok(mut names) = PLAYLIST_NAMES.write() {
            *names = self.local_lists.iter().map(|l| l.name.clone()).collect();
        }
    }

    /// Scans the folders in the background.
    pub(super) fn rescan_local(&mut self) {
        if self.local_scanning {
            return;
        }
        self.local_scanning = true;
        self.local_scanned = true;
        let (folders, files, old) = (self.local_lib.folders.clone(), self.local_lib.files.clone(), self.local_lib.tracks.clone());
        self.spawn(move |_, send| send(Msg::LocalScanned(local::scan(&folders, &files, &old))));
    }

    pub(super) fn local_scanned_done(&mut self, tracks: Vec<local::LocalTrack>) {
        self.local_scanning = false;
        let n = tracks.len();
        self.local_lib.tracks = tracks;
        self.local_changed();
        self.info(format!("Local files: {n} tracks"));
    }

    fn add_local_paths(&mut self, paths: Vec<std::path::PathBuf>) -> usize {
        let mut added = 0;
        for p in paths {
            let s = p.display().to_string();
            if p.is_dir() {
                if !self.local_lib.folders.contains(&s) {
                    self.local_lib.folders.push(s);
                    added += 1;
                }
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| crate::player::local::EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                && !self.local_lib.files.contains(&s)
            {
                self.local_lib.files.push(s);
                added += 1;
            }
        }
        added
    }

    /// Files/folders dropped onto the window go into Local files (and into
    /// the open local playlist, if one is showing).
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if dropped.is_empty() {
            return;
        }
        if let (super::Nav::Local, Some(i)) = (self.nav, self.local_tab) {
            if let Some(list) = self.local_lists.get_mut(i) {
                for p in dropped.iter().filter(|p| p.is_file()) {
                    list.entries.push(Entry::Local { path: p.display().to_string() });
                }
                self.local_lists_changed();
            }
        }
        let added = self.add_local_paths(dropped);
        if added > 0 {
            self.local_lib.save();
            self.rescan_local();
            self.info(format!("Added {added} to Local files"));
        } else {
            self.info("Only music files and folders can be added (MP3, M4A, FLAC, WAV, AIFF, OGG).");
        }
    }

    /// Shows a hint while files are dragged over the window.
    pub(super) fn drop_hint(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.raw.hovered_files.is_empty()) {
            return;
        }
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop-hint")));
        painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(170));
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            "Drop music files or folders to add them to Local files",
            egui::FontId::proportional(20.0),
            egui::Color32::WHITE,
        );
    }

    /// Adds `track` to local playlist `i` (or a new one).
    pub(super) fn add_to_local_playlist(&mut self, i: Option<usize>, track: &Track) {
        let i = match i {
            Some(i) if i < self.local_lists.len() => i,
            _ => {
                let n = self.local_lists.len() + 1;
                self.local_lists.push(LocalPlaylist { name: format!("Playlist {n}"), entries: Vec::new() });
                self.local_lists.len() - 1
            }
        };
        self.local_lists[i].entries.push(Entry::of(track));
        let name = self.local_lists[i].name.clone();
        self.local_lists_changed();
        self.info(format!("Added “{}” to {name}", track.title));
    }

    pub(super) fn remove_from_local_playlist(&mut self, track: &Track) {
        let Some(list) = self.local_tab.and_then(|i| self.local_lists.get_mut(i)) else { return };
        let entry = Entry::of(track);
        let at = list.entries.iter().position(|e| match (e, &entry) {
            (Entry::Cloud { id: a, .. }, Entry::Cloud { id: b, .. }) => a == b,
            (a, b) => a == b,
        });
        if let Some(at) = at {
            list.entries.remove(at);
            self.local_lists_changed();
        }
    }

    pub(super) fn local_ui(&mut self, ui: &mut egui::Ui) {
        if !self.local_scanned && !self.local_lib.folders.is_empty() {
            self.rescan_local(); // pick up changes since last time, once per run
        }
        ui.horizontal(|ui| {
            ui.heading("Local files");
            if self.local_scanning {
                ui.spinner();
                ui.label(RichText::new("Scanning…").small().color(muted()));
            }
        });
        ui.add_space(4.0);
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add folder…").clicked() {
                if let Some(dir) = crate::dialogs::pick_folder("Music folder") {
                    changed |= self.add_local_paths(vec![dir]) > 0;
                }
            }
            if ui.button("Add files…").clicked() {
                let files = crate::dialogs::pick_files("Music files", "Music", &crate::player::local::EXTENSIONS);
                if !files.is_empty() {
                    changed |= self.add_local_paths(files) > 0;
                }
            }
            if ui.add_enabled(!self.local_scanning, egui::Button::new("↻ Rescan")).clicked() {
                self.rescan_local();
            }
            let mut remove = None;
            for (i, f) in self.local_lib.folders.iter().enumerate() {
                let name = Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or(f);
                if ui.small_button(format!("📁 {name}  ×")).on_hover_text(format!("{f}\nClick to remove this folder")).clicked() {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                self.local_lib.folders.remove(i);
                changed = true;
            }
        });
        if changed {
            self.local_lib.save();
            self.local_scanned = false;
            self.rescan_local();
        }

        // Tabs: everything, then each playlist.
        ui.add_space(6.0);
        let mut new_list = false;
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.local_tab.is_none(), format!("All files ({})", self.local_tracks.len())).clicked() {
                self.local_tab = None;
            }
            for (i, l) in self.local_lists.iter().enumerate() {
                if ui.selectable_label(self.local_tab == Some(i), format!("{} ({})", l.name, l.entries.len())).clicked() {
                    self.local_tab = Some(i);
                }
            }
            new_list = ui.button("+ New playlist").clicked();
        });
        if new_list {
            let n = self.local_lists.len() + 1;
            self.local_lists.push(LocalPlaylist { name: format!("Playlist {n}"), entries: Vec::new() });
            self.local_tab = Some(self.local_lists.len() - 1);
            self.local_lists_changed();
        }

        // Playlist header: rename / delete.
        if let Some(i) = self.local_tab.filter(|i| *i < self.local_lists.len()) {
            let mut delete = false;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.local_lists[i].name).desired_width(240.0));
                if r.lost_focus() {
                    self.local_lists_changed();
                }
                delete = ui.button("Delete playlist").clicked();
            });
            ui.label(RichText::new("Add tracks from any list: right-click → Add to local playlist. Or drop files here.").small().color(muted()));
            if delete {
                self.local_lists.remove(i);
                self.local_tab = None;
                self.local_lists_changed();
                return;
            }
        } else if self.local_tracks.is_empty() && !self.local_scanning {
            ui.add_space(20.0);
            ui.label(RichText::new("Add a music folder, or drag files and folders onto the window.").color(muted()));
            return;
        }

        ui.add(egui::TextEdit::singleline(&mut self.local_filter).hint_text("Filter by title, artist or album").desired_width(300.0));
        ui.add_space(4.0);

        // What's listed: (track, ready to play without fetching).
        let list: Vec<(Arc<Track>, bool)> = match self.local_tab.and_then(|i| self.local_lists.get(i)) {
            Some(pl) => pl.entries.iter().filter_map(|e| e.track(&self.local_lib)).collect(),
            None => self.local_tracks.iter().map(|t| (t.clone(), true)).collect(),
        };
        let needle = self.local_filter.trim().to_lowercase();
        let albums: std::collections::HashMap<u64, &str> = self.local_lib.tracks.iter().map(|t| (t.id(), t.album.as_str())).collect();
        let shown: Vec<usize> = (0..list.len())
            .filter(|&i| {
                needle.is_empty() || {
                    let t = &list[i].0;
                    let album = albums.get(&t.id).copied().unwrap_or("");
                    format!("{} {} {}", t.title, t.user.username, album).to_lowercase().contains(&needle)
                }
            })
            .collect();

        let cx = RowCtx { art: &self.art, liked: &self.liked, current: self.queue.current_id(), local_list: self.local_tab.is_some() };
        let mut action: Option<(usize, TrackAction)> = None;
        ui.spacing_mut().item_spacing.y = 2.0;
        egui::ScrollArea::vertical().id_salt(("local", self.local_tab)).auto_shrink(false).show_rows(
            ui,
            rows::row_height(),
            shown.len(),
            |ui, range| {
                for n in range {
                    let (t, _) = &list[shown[n]];
                    let album = albums.get(&t.id).copied().filter(|a| !a.is_empty());
                    if let Some(a) = rows::track_row(ui, &cx, t, album) {
                        action = Some((n, a));
                    }
                }
            },
        );
        if let Some((n, a)) = action {
            let track = list[shown[n]].0.clone();
            if a == TrackAction::Play {
                let items = shown
                    .iter()
                    .map(|&i| {
                        let (t, full) = &list[i];
                        QueueItem { id: t.id, track: full.then(|| t.clone()) }
                    })
                    .collect();
                self.play_list(items, n);
            } else {
                self.track_action(a, track);
            }
        }
    }
}
