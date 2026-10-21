//! Sign-in window. Runs in its own process (`cumulus --login`): opens the
//! SoundCloud sign-in page in the system webview, waits until the site sets
//! its `oauth_token` cookie, saves that to the credential store and exits.
//! The user types their own credentials; nothing here touches the form.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
#[cfg(not(target_os = "linux"))]
use std::time::Instant;

#[cfg(not(target_os = "linux"))]
use winit::application::ApplicationHandler;
#[cfg(not(target_os = "linux"))]
use winit::dpi::LogicalSize;
#[cfg(not(target_os = "linux"))]
use winit::event::{StartCause, WindowEvent};
#[cfg(not(target_os = "linux"))]
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
#[cfg(not(target_os = "linux"))]
use winit::window::{Window, WindowId};
#[cfg(not(target_os = "linux"))]
use wry::{Rect, WebView};
use wry::{WebContext, WebViewBuilder};

pub const LOGIN_ARG: &str = "--login";
pub const EXIT_CANCELLED: i32 = 2;
const EXIT_FAILED: i32 = 3;

const SIGNIN_URL: &str = "https://soundcloud.com/signin";
const COOKIE_URL: &str = "https://soundcloud.com/";
const POLL: Duration = Duration::from_millis(750);

#[cfg(not(target_os = "linux"))]
struct Login {
    ctx: WebContext,
    window: Option<Window>,
    webview: Option<WebView>,
    exit_code: i32,
    /// Set when the main app's end of the stdin pipe closes.
    parent_gone: Arc<AtomicBool>,
}

#[cfg(not(target_os = "linux"))]
impl Login {
    fn poll_cookie(&mut self, el: &ActiveEventLoop) {
        if self.parent_gone.load(Ordering::Relaxed) {
            return el.exit();
        }
        let Some(wv) = &self.webview else { return };
        let Ok(cookies) = wv.cookies_for_url(COOKIE_URL) else { return };
        let Some(token) = cookies
            .iter()
            .find(|c| c.name() == "oauth_token" && !c.value().is_empty())
            .map(|c| c.value().to_string())
        else {
            return;
        };
        match super::save_token(&token) {
            Ok(()) => self.exit_code = 0,
            Err(e) => {
                eprintln!("cumulus login: {e}");
                self.exit_code = EXIT_FAILED;
            }
        }
        el.exit();
    }
}

#[cfg(not(target_os = "linux"))]
impl ApplicationHandler for Login {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Cumulus - sign in with your SoundCloud account")
            .with_inner_size(LogicalSize::new(1000.0, 820.0));
        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("cumulus login: {e}");
                self.exit_code = EXIT_FAILED;
                return el.exit();
            }
        };
        match WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_url(SIGNIN_URL)
            .build(&window)
        {
            Ok(wv) => self.webview = Some(wv),
            Err(e) => {
                eprintln!("cumulus login: webview: {e}");
                self.exit_code = EXIT_FAILED;
                return el.exit();
            }
        }
        // A parent started minimized/hidden passes that on; always show it.
        window.set_minimized(false);
        window.focus_window();
        self.window = Some(window);
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL));
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.poll_cookie(el);
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + POLL));
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                if let Some(wv) = &self.webview {
                    let _ = wv.set_bounds(Rect {
                        position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                        size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                    });
                }
            }
            _ => {}
        }
    }
}

/// Watches stdin: the main app holds it open, so EOF means it has exited
/// or crashed.
fn watch_parent() -> Arc<AtomicBool> {
    let parent_gone = Arc::new(AtomicBool::new(false));
    let flag = parent_gone.clone();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
        flag.store(true, Ordering::Relaxed);
    });
    parent_gone
}

/// Linux: a GTK window with WebKitGTK inside.
#[cfg(target_os = "linux")]
pub fn run() -> i32 {
    use std::cell::Cell;
    use std::rc::Rc;

    use gtk::glib;
    use gtk::prelude::*;
    use wry::WebViewBuilderExtUnix;

    // WebKit's GPU path often shows a blank page under WSL and some drivers.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: still single-threaded here (before GTK or any thread starts).
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    if gtk::init().is_err() {
        eprintln!("cumulus login: no display");
        return EXIT_FAILED;
    }
    let parent_gone = watch_parent();
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("Cumulus - sign in with your SoundCloud account");
    window.set_default_size(1000, 820);
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.add(&container);
    let mut ctx = WebContext::new(Some(crate::paths::webview_dir()));
    let webview = match WebViewBuilder::new_with_web_context(&mut ctx).with_url(SIGNIN_URL).build_gtk(&container) {
        Ok(wv) => wv,
        Err(e) => {
            eprintln!("cumulus login: webview: {e}");
            return EXIT_FAILED;
        }
    };
    window.show_all();
    window.present();
    let code = Rc::new(Cell::new(EXIT_CANCELLED));
    window.connect_delete_event(|_, _| {
        gtk::main_quit();
        glib::Propagation::Proceed
    });
    let done = code.clone();
    glib::timeout_add_local(POLL, move || {
        if parent_gone.load(Ordering::Relaxed) {
            gtk::main_quit();
            return glib::ControlFlow::Break;
        }
        let Ok(cookies) = webview.cookies_for_url(COOKIE_URL) else { return glib::ControlFlow::Continue };
        let Some(token) = cookies.iter().find(|c| c.name() == "oauth_token" && !c.value().is_empty()).map(|c| c.value().to_string())
        else {
            return glib::ControlFlow::Continue;
        };
        match super::save_token(&token) {
            Ok(()) => done.set(0),
            Err(e) => {
                eprintln!("cumulus login: {e}");
                done.set(EXIT_FAILED);
            }
        }
        gtk::main_quit();
        glib::ControlFlow::Break
    });
    gtk::main();
    code.get()
}

/// Entry point for `cumulus --login`. Returns the process exit code.
#[cfg(not(target_os = "linux"))]
pub fn run() -> i32 {
    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("cumulus login: {e}");
            return EXIT_FAILED;
        }
    };
    let parent_gone = watch_parent();
    let mut app = Login {
        ctx: WebContext::new(Some(crate::paths::webview_dir())),
        window: None,
        webview: None,
        exit_code: EXIT_CANCELLED,
        parent_gone,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("cumulus login: {e}");
        return EXIT_FAILED;
    }
    app.exit_code
}
