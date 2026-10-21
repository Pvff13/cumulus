//! File and folder pickers: the native Windows dialogs, or GTK's on Linux
//! (run on the GTK thread). They block until the user picks or cancels.

use std::path::PathBuf;

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;

    pub fn pick_folder(title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    pub fn pick_files(title: &str, filter: &str, exts: &[&str]) -> Vec<PathBuf> {
        rfd::FileDialog::new().set_title(title).add_filter(filter, exts).pick_files().unwrap_or_default()
    }

    pub fn save_file(title: &str, name: &str, filter: &str, exts: &[&str]) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).set_file_name(name).add_filter(filter, exts).save_file()
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    use gtk::prelude::*;

    fn dialog(title: &str, action: gtk::FileChooserAction, accept: &str) -> gtk::FileChooserDialog {
        let d = gtk::FileChooserDialog::with_buttons(
            Some(title),
            None::<&gtk::Window>,
            action,
            &[("Cancel", gtk::ResponseType::Cancel), (accept, gtk::ResponseType::Accept)],
        );
        d.set_keep_above(true);
        d
    }

    fn filter(name: &str, exts: &[&str]) -> gtk::FileFilter {
        let f = gtk::FileFilter::new();
        f.set_name(Some(name));
        for e in exts {
            // Case-insensitive: *.mp3 and *.MP3.
            let pattern: String = e.chars().map(|c| if c.is_ascii_alphabetic() { format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()) } else { c.to_string() }).collect();
            f.add_pattern(&format!("*.{pattern}"));
        }
        f
    }

    fn run(d: gtk::FileChooserDialog) -> Vec<PathBuf> {
        let picked = if d.run() == gtk::ResponseType::Accept { d.filenames() } else { Vec::new() };
        d.close();
        // Let GTK actually remove the window before returning.
        while gtk::events_pending() {
            gtk::main_iteration();
        }
        picked
    }

    pub fn pick_folder(title: &str) -> Option<PathBuf> {
        let title = title.to_string();
        crate::gtk_thread::run(move || run(dialog(&title, gtk::FileChooserAction::SelectFolder, "Choose"))).and_then(|v| v.into_iter().next())
    }

    pub fn pick_files(title: &str, name: &str, exts: &[&str]) -> Vec<PathBuf> {
        let (title, name) = (title.to_string(), name.to_string());
        let exts: Vec<String> = exts.iter().map(|e| e.to_string()).collect();
        crate::gtk_thread::run(move || {
            let d = dialog(&title, gtk::FileChooserAction::Open, "Open");
            d.set_select_multiple(true);
            let refs: Vec<&str> = exts.iter().map(String::as_str).collect();
            d.add_filter(filter(&name, &refs));
            run(d)
        })
        .unwrap_or_default()
    }

    pub fn save_file(title: &str, file: &str, name: &str, exts: &[&str]) -> Option<PathBuf> {
        let (title, file, name) = (title.to_string(), file.to_string(), name.to_string());
        let exts: Vec<String> = exts.iter().map(|e| e.to_string()).collect();
        crate::gtk_thread::run(move || {
            let d = dialog(&title, gtk::FileChooserAction::Save, "Save");
            d.set_current_name(&file);
            d.set_do_overwrite_confirmation(true);
            let refs: Vec<&str> = exts.iter().map(String::as_str).collect();
            d.add_filter(filter(&name, &refs));
            run(d)
        })
        .and_then(|v| v.into_iter().next())
    }
}

pub fn pick_folder(title: &str) -> Option<PathBuf> {
    imp::pick_folder(title)
}

pub fn pick_file(title: &str, filter: &str, exts: &[&str]) -> Option<PathBuf> {
    imp::pick_files(title, filter, exts).into_iter().next()
}

pub fn pick_files(title: &str, filter: &str, exts: &[&str]) -> Vec<PathBuf> {
    imp::pick_files(title, filter, exts)
}

pub fn save_file(title: &str, name: &str, filter: &str, exts: &[&str]) -> Option<PathBuf> {
    imp::save_file(title, name, filter, exts)
}
