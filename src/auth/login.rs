//! Sign-in window. Runs in its own process (`cumulus --login`): opens the
//! SoundCloud sign-in page in the system webview, waits until the site sets
//! its `oauth_token` cookie, saves that to the credential store and exits.
//! The user types their own credentials; nothing here touches the form.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

pub const LOGIN_ARG: &str = "--login";
pub const EXIT_CANCELLED: i32 = 2;
const EXIT_FAILED: i32 = 3;

const SIGNIN_URL: &str = "https://soundcloud.com/signin";
const COOKIE_URL: &str = "https://soundcloud.com/";
const POLL: Duration = Duration::from_millis(750);

struct Login {
    ctx: WebContext,
    window: Option<Window>,
    webview: Option<WebView>,
    exit_code: i32,
    /// Set when the main app's end of the stdin pipe closes.
    parent_gone: Arc<AtomicBool>,
}

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

/// Entry point for `cumulus --login`. Returns the process exit code.
pub fn run() -> i32 {
    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("cumulus login: {e}");
            return EXIT_FAILED;
        }
    };
    // The main app holds our stdin open; EOF means it has exited or crashed.
    let parent_gone = Arc::new(AtomicBool::new(false));
    {
        let flag = parent_gone.clone();
        std::thread::spawn(move || {
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            flag.store(true, Ordering::Relaxed);
        });
    }
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
