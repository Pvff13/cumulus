//! Linux only: GTK can only be used from one thread, and eframe's window
//! isn't a GTK window. So one background thread runs GTK for everything that
//! needs it (file pickers); other threads hand it work.

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
