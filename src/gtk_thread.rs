//! Linux only: GTK can only be used from one thread, and eframe's window
//! isn't a GTK window. So one background thread runs GTK for everything that
//! needs it (the tray icon and file pickers); other threads hand it work.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use gtk::glib;

type Job = Box<dyn FnOnce() + Send>;

static JOBS: OnceLock<Option<Sender<Job>>> = OnceLock::new();

/// Starts the GTK thread (once). None if GTK can't start (no display).
fn jobs() -> Option<&'static Sender<Job>> {
    JOBS.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let (ok_tx, ok_rx) = channel();
        std::thread::Builder::new()
            .name("gtk".into())
            .spawn(move || {
                if gtk::init().is_err() {
                    let _ = ok_tx.send(false);
                    return;
                }
                let _ = ok_tx.send(true);
                // Run queued jobs from inside the GTK main loop.
                glib::timeout_add_local(Duration::from_millis(30), move || {
                    while let Ok(job) = rx.try_recv() {
                        job();
                    }
                    glib::ControlFlow::Continue
                });
                gtk::main();
            })
            .ok()?;
        ok_rx.recv_timeout(Duration::from_secs(5)).ok().filter(|ok| *ok).map(|_| tx)
    })
    .as_ref()
}

/// Runs `f` on the GTK thread without waiting.
pub fn spawn(f: impl FnOnce() + Send + 'static) -> bool {
    jobs().is_some_and(|tx| tx.send(Box::new(f)).is_ok())
}

/// Runs `f` on the GTK thread and waits for its result.
pub fn run<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
    let (tx, rx) = channel();
    if !spawn(move || {
        let _ = tx.send(f());
    }) {
        return None;
    }
    rx.recv().ok()
}

/// Whether a tray (StatusNotifier) host is running. Without one a tray icon
/// would be invisible, so Cumulus mustn't hide to it. Runs on the GTK thread.
pub fn tray_host_present() -> bool {
    {
        let Ok(bus) = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE) else {
            return false;
        };
        let reply = bus.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&("org.kde.StatusNotifierWatcher",).into()),
            Some(glib::VariantTy::new("(b)").expect("valid type")),
            gtk::gio::DBusCallFlags::NONE,
            1000,
            gtk::gio::Cancellable::NONE,
        );
        reply.ok().and_then(|v| v.get::<(bool,)>()).is_some_and(|(b,)| b)
    }
}
