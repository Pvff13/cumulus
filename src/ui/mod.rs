mod desktop;
mod fonts;
mod home;
mod icon;
mod library;
mod overlay_ui;
mod player_bar;
mod queue;
mod queue_panel;
mod rows;
mod theme;
mod views;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, RichText};

use crate::api::models::{Playlist, Selection, Track, User};
use crate::api::{Api, ApiError, SearchKind};
use crate::auth;
use crate::cache::art::ArtCache;
use crate::config::Config;
use crate::player::{self, Player, QueueItem};
use desktop::{Desktop, DesktopEvent};
pub use desktop::window_icon;
#[cfg(debug_assertions)]
pub use icon::write_files as write_icon_files;
use library::{Feed, FeedKind, PlaylistView, RowPage};
use queue::Queue;
use theme::{ACCENT, ERROR, MUTED, SIDEBAR};

/// Results coming back from worker threads.
enum Msg {
    Session(Result<Option<User>, ApiError>),
    LoginWindowClosed(Result<bool, String>),
    Feed { kind: FeedKind, generation: u32, first: bool, cached: bool, result: Result<RowPage, ApiError> },
    Playlist { id: u64, result: Result<Playlist, ApiError> },
    Tracks { playlist_id: u64, asked: Vec<u64>, result: Result<HashMap<u64, Track>, ApiError> },
    Player(player::Event),
    /// Full details for queued tracks that were only known by id.
    QueueTracks(Vec<Arc<Track>>),
    Liked(Result<HashSet<u64>, ApiError>),
    Desktop(DesktopEvent),
    Home { cached: bool, result: Result<Vec<Selection>, ApiError> },
    History(Result<Vec<Arc<Track>>, ApiError>),
    /// Similar tracks to continue with after `after` (the queue's last track).
    Autoplay { after: u64, result: Result<Vec<Track>, ApiError> },
    /// A newer version is downloaded/verified and ready to install.
    UpdateReady(crate::update::Ready),
    /// Result of pressing "Check for updates".
    UpdateChecked(crate::update::Outcome),
}

enum Screen {
    /// Checking the saved token.
    Starting,
    SignedOut { error: Option<String> },
    /// The sign-in window is open.
    SigningIn,
    /// Had a token but couldn't verify it (offline, server trouble).
    Problem(String),
    SignedIn(User),
    /// Placeholder while the current screen is being drawn.
    Taken,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nav {
    Home,
    Stream,
    Likes,
    Playlists,
    Reposts,
    Search,
}

impl Nav {
    const ALL: [Nav; 6] = [Nav::Home, Nav::Stream, Nav::Likes, Nav::Playlists, Nav::Reposts, Nav::Search];

    fn label(self) -> &'static str {
        match self {
            Nav::Home => "Home",
            Nav::Stream => "Stream",
            Nav::Likes => "Likes",
            Nav::Playlists => "Playlists",
            Nav::Reposts => "Reposts",
            Nav::Search => "Search",
        }
    }

    fn feed(self) -> Option<FeedKind> {
        match self {
            Nav::Stream => Some(FeedKind::Stream),
            Nav::Likes => Some(FeedKind::Likes),
            Nav::Playlists => Some(FeedKind::Library),
            Nav::Reposts => Some(FeedKind::Reposts),
            Nav::Home | Nav::Search => None,
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Nav::Stream => "Nothing in your stream yet. Follow some artists on SoundCloud.",
            Nav::Likes => "You haven't liked any tracks yet.",
            Nav::Playlists => "No playlists yet.",
            Nav::Reposts => "You haven't reposted anything yet.",
            Nav::Home | Nav::Search => "No results.",
        }
    }
}

/// An artist whose uploads are shown in-app.
#[derive(Clone)]
struct ArtistInfo {
    id: u64,
    username: String,
    permalink_url: String,
    avatar_url: Option<String>,
}

/// Pages opened on top of a sidebar view (← Back pops them).
enum Page {
    Playlist(PlaylistView),
    Artist(ArtistInfo),
}

struct Status {
    text: String,
    at: Instant,
    error: bool,
}

pub struct App {
    api: Arc<Api>,
    art: ArtCache,
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    screen: Screen,
    /// Signed-in user id (0 when signed out).
    me: u64,
    nav: Nav,
    home: home::HomeState,
    feeds: HashMap<FeedKind, Feed>,
    pages: Vec<Page>,
    paste_open: bool,
    paste: String,

    liked: HashSet<u64>,
    /// Set after sending the user to soundcloud.com to like / add to a
    /// playlist: re-read their library when this window is focused again.
    recheck_on_focus: Option<rows::TrackAction>,
    was_focused: bool,

    search_text: String,
    /// The query the search results are for.
    search_query: String,
    search_kind: SearchKind,
    search_edited: Option<Instant>,
    focus_search: bool,

    player: Player,
    queue: Queue,
    show_queue: bool,
    /// Queue entries whose details have been requested.
    queue_requested: HashSet<u64>,
    volume: f32,
    /// Seek bar value while being dragged.
    seek_drag: Option<f64>,
    pending_seek: Option<(f64, Instant)>,
    /// Message shown above the content (skipped tracks, likes, playlist edits).
    status: Option<Status>,
    /// Consecutive tracks that failed to play; stops runaway skipping when offline.
    failures: u32,
    /// Keep playing similar tracks when the queue runs out.
    autoplay: bool,
    /// The last-track id autoplay already fetched for (so it asks once).
    autoplay_for: Option<u64>,
    /// Now-playing pop-out window settings.
    overlay_menu: overlay_ui::OverlayMenu,

    desktop: Desktop,
    close_to_tray: bool,
    /// Window hidden to the tray.
    hidden: bool,
    /// Quit was chosen: let the next close request through.
    quitting: bool,

    update_checker: Arc<std::sync::Mutex<crate::update::Checker>>,
    last_update_check: Option<Instant>,
    update_ready: Option<crate::update::Ready>,
    /// "Check for updates" is running.
    checking_update: bool,
}

/// How often to look for a newer build.
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(600);

/// Give up auto-skipping after this many tracks fail in a row.
const MAX_FAILURES: u32 = 5;
/// Search runs this long after you stop typing.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(400);

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        fonts::install(&cc.egui_ctx);
        let config = Config::load();
        let overlay_menu = overlay_ui::OverlayMenu::load(&config);
        let api = Arc::new(Api::new(config.client_id));
        let (tx, rx) = channel();
        let player = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            Player::new(api.clone(), config.volume, move |e| {
                let _ = tx.send(Msg::Player(e));
                ctx.request_repaint();
            })
        };
        let desktop = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            Desktop::new(cc, move |e| {
                let _ = tx.send(Msg::Desktop(e));
                ctx.request_repaint();
            })
        };
        #[allow(unused_mut)]
        let mut app = Self {
            art: ArtCache::new(cc.egui_ctx.clone(), api.agent(), config.art_cache_mb),
            api,
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            screen: Screen::Starting,
            me: 0,
            nav: Nav::Home,
            home: home::HomeState::default(),
            feeds: HashMap::new(),
            pages: Vec::new(),
            paste_open: false,
            paste: String::new(),
            liked: HashSet::new(),
            recheck_on_focus: None,
            was_focused: true,
            search_text: String::new(),
            search_query: String::new(),
            search_kind: SearchKind::Tracks,
            search_edited: None,
            focus_search: false,
            player,
            queue: Queue::default(),
            show_queue: false,
            queue_requested: HashSet::new(),
            volume: config.volume,
            seek_drag: None,
            pending_seek: None,
            status: None,
            failures: 0,
            autoplay: config.autoplay,
            autoplay_for: None,
            overlay_menu,
            desktop,
            close_to_tray: config.close_to_tray,
            hidden: false,
            quitting: false,
            update_checker: Arc::default(),
            last_update_check: None,
            update_ready: None,
            checking_update: false,
        };
        #[cfg(debug_assertions)]
        app.apply_dev_env();
        app.check_session();
        app
    }

    /// Debug builds: start-up switches used for screenshots and tests.
    #[cfg(debug_assertions)]
    fn apply_dev_env(&mut self) {
        self.nav = match std::env::var("CUMULUS_DEV_NAV").as_deref() {
            Ok("stream") => Nav::Stream,
            Ok("playlists") => Nav::Playlists,
            Ok("reposts") => Nav::Reposts,
            Ok("search") => Nav::Search,
            Ok("likes") => Nav::Likes,
            _ => Nav::Home,
        };
        if let Ok(q) = std::env::var("CUMULUS_DEV_SEARCH") {
            self.search_text = q.clone();
            self.search_query = q;
        }
        if let Ok(k) = std::env::var("CUMULUS_DEV_SEARCH_KIND") {
            self.search_kind = match k.as_str() {
                "people" => SearchKind::People,
                "playlists" => SearchKind::Playlists,
                "albums" => SearchKind::Albums,
                _ => SearchKind::Tracks,
            };
        }
        self.show_queue = std::env::var_os("CUMULUS_DEV_QUEUE").is_some();
        if std::env::var_os("CUMULUS_DEV_POPOUT").is_some() {
            self.overlay_menu.popout = true;
        }
    }

    fn spawn(&self, job: impl FnOnce(&Api, &dyn Fn(Msg)) + Send + 'static) {
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), self.ctx.clone());
        thread::spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            job(&api, &send);
        });
    }

    fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), at: Instant::now(), error: false });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status { text: text.into(), at: Instant::now(), error: true });
    }

    /// Load the saved token (if any) and confirm it with /me.
    fn check_session(&self) {
        self.spawn(|api, send| {
            send(Msg::Session(match auth::load_token() {
                Err(e) => Err(ApiError::Other(e)),
                Ok(None) => Ok(None),
                Ok(Some(token)) => {
                    api.set_token(Some(token));
                    api.me().map(Some)
                }
            }))
        });
    }

    /// Which tracks the user has liked (for the hearts).
    fn load_liked(&self) {
        self.spawn(move |api, send| send(Msg::Liked(api.liked_ids())));
    }

    /// Back from soundcloud.com: pick up likes / playlist changes made there.
    fn recheck_library(&mut self, after: rows::TrackAction) {
        self.load_liked();
        let kind = if after == rows::TrackAction::LikeOnSoundCloud { FeedKind::Likes } else { FeedKind::Library };
        if self.feeds.get(&kind).is_some_and(|f| f.started) {
            self.load_feed(kind, true);
        }
    }

    fn start_login(&mut self) {
        self.screen = Screen::SigningIn;
        self.spawn(|_, send| send(Msg::LoginWindowClosed(auth::run_login_process())));
    }

    fn sign_out(&mut self, error: Option<String>) {
        self.player.stop();
        self.queue = Queue::default();
        auth::sign_out();
        self.api.set_token(None);
        self.feeds.clear();
        self.home = home::HomeState::default();
        self.pages.clear();
        self.liked.clear();
        self.recheck_on_focus = None;
        self.me = 0;
        self.screen = Screen::SignedOut { error };
    }

    fn first_url(&self, kind: FeedKind) -> String {
        match kind {
            FeedKind::Likes => Api::likes_url(self.me),
            FeedKind::Stream => Api::stream_url(),
            FeedKind::Reposts => Api::reposts_url(self.me),
            FeedKind::Library => Api::library_url(),
            FeedKind::Search(k) => Api::search_url(k, &self.search_query),
            FeedKind::Artist(id) => Api::user_tracks_url(id),
        }
    }

    /// Loads the first page of a feed (showing the saved copy meanwhile) or the next page.
    fn load_feed(&mut self, kind: FeedKind, first: bool) {
        let first_url = self.first_url(kind);
        let me = self.me;
        let feed = self.feeds.entry(kind).or_default();
        if first {
            feed.generation += 1;
            feed.started = true;
        }
        let url = if first {
            first_url
        } else {
            match feed.next.clone() {
                Some(u) => u,
                None => return,
            }
        };
        feed.loading = true;
        let generation = feed.generation;
        let show_saved = first && feed.rows.is_empty();
        self.spawn(move |api, send| {
            if show_saved {
                if let Some(page) = library::cached_first_page(kind, me) {
                    send(Msg::Feed { kind, generation, first, cached: true, result: Ok(page) });
                }
            }
            let result = library::fetch_page(api, kind, me, &url, first);
            send(Msg::Feed { kind, generation, first, cached: false, result });
        });
    }

    fn open_playlist(&mut self, playlist: Arc<Playlist>) {
        let id = playlist.id;
        self.pages.push(Page::Playlist(PlaylistView::new(playlist)));
        self.spawn(move |api, send| send(Msg::Playlist { id, result: library::fetch_playlist(api, id) }));
    }

    fn open_artist(&mut self, artist: ArtistInfo) {
        // Keep only artist lists that are still reachable via Back.
        let open: HashSet<u64> = self
            .pages
            .iter()
            .filter_map(|p| match p {
                Page::Artist(a) => Some(a.id),
                Page::Playlist(_) => None,
            })
            .collect();
        self.feeds.retain(|k, _| !matches!(k, FeedKind::Artist(id) if !open.contains(id)));
        self.pages.push(Page::Artist(artist));
    }

    fn playlist_page(&mut self, id: u64) -> Option<&mut PlaylistView> {
        self.pages.iter_mut().find_map(|p| match p {
            Page::Playlist(v) if v.playlist.id == id => Some(v),
            _ => None,
        })
    }

    /// Runs the search for whatever is typed (if it changed).
    fn run_search(&mut self) {
        self.search_edited = None;
        let q = self.search_text.trim().to_string();
        if q != self.search_query {
            self.search_query = q;
            self.feeds.retain(|k, _| !matches!(k, FeedKind::Search(_)));
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Session(Ok(Some(user))) => {
                self.me = user.id;
                self.screen = Screen::SignedIn(user);
                self.load_liked();
            }
            Msg::Session(Ok(None)) => self.screen = Screen::SignedOut { error: None },
            Msg::Session(Err(ApiError::Unauthorized)) => self.sign_out(Some(ApiError::Unauthorized.to_string())),
            Msg::Session(Err(e)) => self.screen = Screen::Problem(e.to_string()),
            Msg::LoginWindowClosed(Ok(true)) => {
                self.screen = Screen::Starting;
                self.check_session();
            }
            Msg::LoginWindowClosed(Ok(false)) => self.screen = Screen::SignedOut { error: None },
            Msg::LoginWindowClosed(Err(e)) => self.screen = Screen::SignedOut { error: Some(e) },
            Msg::Feed { result: Err(ApiError::Unauthorized), .. }
            | Msg::Playlist { result: Err(ApiError::Unauthorized), .. }
            | Msg::Tracks { result: Err(ApiError::Unauthorized), .. } => {
                self.sign_out(Some(ApiError::Unauthorized.to_string()))
            }
            Msg::Feed { kind, generation, first, cached, result } => {
                if let Some(feed) = self.feeds.get_mut(&kind).filter(|f| f.generation == generation) {
                    feed.apply(first, cached, result);
                }
            }
            Msg::Playlist { id, result } => {
                if let Some(view) = self.playlist_page(id) {
                    view.apply_playlist(result);
                }
            }
            Msg::Tracks { playlist_id, asked, result } => {
                if let Some(view) = self.playlist_page(playlist_id) {
                    view.apply_tracks(&asked, result);
                }
            }
            Msg::QueueTracks(tracks) => {
                for t in tracks {
                    if t.availability() == crate::api::models::Availability::Drm {
                        self.queue.mark_unplayable(t.id);
                    }
                    self.queue.update_track(t);
                }
                // Something just learned to be unplayable may have been "next".
                self.player.set_next(self.queue.peek_next());
            }
            Msg::Desktop(e) => self.desktop_event(e),
            Msg::UpdateReady(ready) => self.update_ready = Some(ready),
            Msg::UpdateChecked(outcome) => {
                use crate::update::Outcome;
                self.checking_update = false;
                match outcome {
                    Outcome::Ready(ready) => {
                        let what = ready.version.as_deref().map_or("A newer build".to_string(), |v| format!("Version {v}"));
                        self.info(format!("{what} is ready: click the update button below the sidebar menu."));
                        self.update_ready = Some(ready);
                    }
                    Outcome::UpToDate => {
                        self.info(format!("You're up to date (Cumulus {}).", crate::update::current_version()))
                    }
                    Outcome::Problem(why) => self.error(format!("Couldn't check for updates: {why}")),
                }
            }
            Msg::Home { result: Err(ApiError::Unauthorized), .. } => self.sign_out(Some(ApiError::Unauthorized.to_string())),
            Msg::Home { cached, result } => self.home_loaded(cached, result),
            Msg::History(Ok(tracks)) => self.home.history = tracks,
            // Recently Played then falls back to SoundCloud's own row.
            Msg::History(Err(_e)) => {
                #[cfg(debug_assertions)]
                eprintln!("[debug] play history: {_e}");
            }
            Msg::Liked(Ok(ids)) => self.liked = ids,
            // Hearts just stay empty; everything else still works.
            Msg::Liked(Err(_)) => {}
            Msg::Player(player::Event::Failed { id, reason, upcoming }) => {
                let title = self.queue_title(id);
                let drm = reason == player::DRM_REASON;
                self.error(format!("Skipped \u{201c}{title}\u{201d}: {reason}"));
                self.queue.mark_unplayable(id);
                if upcoming {
                    self.player.set_next(self.queue.peek_next());
                } else {
                    // DRM tracks are expected in label releases; they don't
                    // count towards the "something's wrong, stop" limit.
                    if !drm {
                        self.failures += 1;
                    }
                    if self.failures >= MAX_FAILURES {
                        self.error("Stopped: several tracks in a row couldn't be played.");
                        self.player.stop();
                    } else if let Some(item) = self.queue.advance() {
                        self.player.play(item, 0.0);
                        self.player.set_next(self.queue.peek_next());
                    } else {
                        // Nothing left that can play: stop instead of sitting on the failed track.
                        self.player.stop();
                        if drm {
                            self.error(format!(
                                "Nothing else here can play in Cumulus: “{title}” and the rest are DRM-protected. Open them on SoundCloud (↗)."
                            ));
                        }
                    }
                }
            }
            Msg::Player(player::Event::Ended) => {
                if self.autoplay {
                    self.info("End of queue.");
                } else {
                    self.info("End of queue. Turn on ∞ autoplay to keep going with similar tracks.");
                }
            }
            Msg::Autoplay { after, result } => self.autoplay_loaded(after, result),
        }
    }

    fn desktop_event(&mut self, e: DesktopEvent) {
        use souvlaki::{MediaControlEvent as M, MediaPosition, SeekDirection};
        let pos = self.player.now().map_or(0.0, |(_, p)| p);
        let step = |dir: &SeekDirection, secs: f64| if *dir == SeekDirection::Forward { secs } else { -secs };
        match e {
            DesktopEvent::Media(M::Play) if self.player.is_paused() => self.toggle_play(),
            DesktopEvent::Media(M::Pause | M::Stop) if !self.player.is_paused() => self.toggle_play(),
            DesktopEvent::Media(M::Toggle) | DesktopEvent::PlayPause => self.toggle_play(),
            DesktopEvent::Media(M::Next) | DesktopEvent::Next => self.next(),
            DesktopEvent::Media(M::Previous) | DesktopEvent::Previous => self.previous(),
            DesktopEvent::Media(M::SetPosition(MediaPosition(d))) => self.seek_to(d.as_secs_f64()),
            DesktopEvent::Media(M::SeekBy(dir, d)) => self.seek_to(pos + step(&dir, d.as_secs_f64())),
            DesktopEvent::Media(M::Seek(dir)) => self.seek_to(pos + step(&dir, 10.0)),
            DesktopEvent::Media(_) => {}
            DesktopEvent::ToggleWindow => self.set_hidden(!self.hidden),
            DesktopEvent::Quit => {
                self.quitting = true;
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Looks for a newer build in the background now and then.
    fn check_for_update(&mut self) {
        if self.update_ready.is_some() || self.last_update_check.is_some_and(|t| t.elapsed() < UPDATE_CHECK_EVERY) {
            return;
        }
        self.last_update_check = Some(Instant::now());
        let checker = self.update_checker.clone();
        self.spawn(move |api, send| {
            if let Some(ready) = checker.lock().unwrap().check(&api.agent()) {
                send(Msg::UpdateReady(ready));
            }
        });
    }

    /// The "Check for updates" button: check right now, whatever the schedule.
    fn check_for_update_now(&mut self) {
        if self.checking_update {
            return;
        }
        self.checking_update = true;
        self.last_update_check = Some(Instant::now());
        let checker = self.update_checker.clone();
        self.spawn(move |api, send| send(Msg::UpdateChecked(checker.lock().unwrap().check_now(&api.agent()))));
    }

    fn install_update(&mut self) {
        let Some(ready) = self.update_ready.clone() else { return };
        match crate::update::apply(&ready.exe) {
            Ok(()) => {
                self.quitting = true;
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                self.update_ready = None;
                self.error(format!("Update failed: {e}"));
            }
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Visible(!hidden));
        if !hidden {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn queue_title(&self, id: u64) -> String {
        (0..self.queue.len())
            .filter_map(|p| self.queue.at(p))
            .find(|i| i.id == id)
            .and_then(|i| i.track.as_ref())
            .map_or_else(|| "a track".to_string(), |t| t.title.clone())
    }

    /// Starts playing `items[index]`, queueing the rest of the list after it.
    fn play_list(&mut self, items: Vec<QueueItem>, index: usize) {
        // DRM-only tracks are skipped without trying them.
        let drm: Vec<u64> = items
            .iter()
            .filter(|i| i.track.as_ref().is_some_and(|t| t.availability() == crate::api::models::Availability::Drm))
            .map(|i| i.id)
            .collect();
        self.queue.set(items, index);
        for id in drm {
            self.queue.mark_unplayable(id);
        }
        self.queue_requested.clear();
        self.failures = 0;
        if self.queue.current_id().is_some_and(|id| self.queue.is_unplayable(id)) && self.queue.advance().is_none() {
            self.player.stop();
            self.error("Nothing here can play in Cumulus: it's all DRM-protected. Open it on SoundCloud (↗) instead.");
            return;
        }
        if let Some(item) = self.queue.current().cloned() {
            // Debug builds: CUMULUS_DEV_START=<secs> starts the first track there (gapless testing).
            #[cfg(debug_assertions)]
            let at = std::env::var("CUMULUS_DEV_START").ok().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            #[cfg(not(debug_assertions))]
            let at = 0.0;
            self.player.play(item, at);
            self.player.set_next(self.queue.peek_next());
        }
    }

    /// Plays the queue entry at a play-order position.
    fn play_queue_at(&mut self, p: usize) {
        if let Some(item) = self.queue.jump_to(p) {
            self.failures = 0;
            self.player.play(item, 0.0);
            self.player.set_next(self.queue.peek_next());
        }
    }

    /// Keeps the queue in step with what is being heard (the engine moves on
    /// to the next track by itself), and fetches details the bar needs.
    fn sync_playback(&mut self) {
        if let Some((id, pos)) = self.player.now() {
            if self.queue.current_id() != Some(id) {
                self.queue.sync_to(id);
                self.player.set_next(self.queue.peek_next());
            }
            if pos > 2.0 {
                self.failures = 0;
            }
        }
        if let Some(item) = self.queue.current() {
            if item.track.is_none() {
                let id = item.id;
                self.fetch_queue_tracks(vec![id]);
            }
        }
        self.autoplay_if_needed();
    }

    /// When the last track of the queue starts, fetch similar tracks to add
    /// after it. The engine opens the next track ~30 s before the end, so this
    /// continues without a gap.
    fn autoplay_if_needed(&mut self) {
        if !self.autoplay || self.queue.repeat != queue::Repeat::Off || self.queue.peek_next().is_some() {
            return;
        }
        let Some(current) = self.queue.current_id() else { return };
        if self.autoplay_for == Some(current) || self.player.now().is_none_or(|(id, _)| id != current) {
            return;
        }
        self.autoplay_for = Some(current);
        self.spawn(move |api, send| send(Msg::Autoplay { after: current, result: api.related(current) }));
    }

    fn autoplay_loaded(&mut self, after: u64, result: Result<Vec<Track>, ApiError>) {
        // Only if we're still at the end of the queue with autoplay on.
        if !self.autoplay || self.queue.current_id() != Some(after) || self.queue.peek_next().is_some() {
            return;
        }
        let Ok(tracks) = result else { return };
        let mut added = 0;
        for t in tracks {
            if t.availability() != crate::api::models::Availability::Playable || self.queue.contains(t.id) {
                continue;
            }
            self.queue.add_to_end(QueueItem { id: t.id, track: Some(Arc::new(t)) });
            added += 1;
            if added == 20 {
                break;
            }
        }
        if added > 0 {
            self.player.set_next(self.queue.peek_next());
        }
    }

    pub(super) fn set_autoplay(&mut self, on: bool) {
        self.autoplay = on;
        self.autoplay_for = None;
        let mut config = Config::load();
        config.autoplay = on;
        config.save();
        self.info(if on {
            "Autoplay on: similar tracks play when the queue runs out."
        } else {
            "Autoplay off: playback stops at the end of the queue."
        });
    }

    /// Fetches details for queue entries only known by id (once each).
    fn fetch_queue_tracks(&mut self, ids: Vec<u64>) {
        let ids: Vec<u64> = ids.into_iter().filter(|id| self.queue_requested.insert(*id)).collect();
        for chunk in ids.chunks(crate::api::IDS_PER_REQUEST) {
            let chunk = chunk.to_vec();
            self.spawn(move |api, send| {
                if let Ok(found) = api.tracks(&chunk) {
                    send(Msg::QueueTracks(found.into_values().map(Arc::new).collect()));
                }
            });
        }
    }

    fn signed_out_ui(&mut self, ui: &mut egui::Ui, error: Option<&str>) {
        centered(ui, |ui| {
            ui.heading("Cumulus");
            ui.label(RichText::new("A lightweight player for your SoundCloud library").color(MUTED));
            ui.add_space(18.0);
            if ui
                .add(egui::Button::new(RichText::new("Sign in with SoundCloud").strong()).fill(ACCENT))
                .clicked()
            {
                self.start_login();
            }
            ui.label(
                RichText::new("Opens SoundCloud's own sign-in page in a small window.")
                    .small()
                    .color(MUTED),
            );
            if let Some(e) = error {
                ui.add_space(8.0);
                ui.colored_label(ERROR, e);
            }
            ui.add_space(14.0);
            ui.toggle_value(&mut self.paste_open, "Paste a token instead");
            if self.paste_open {
                ui.label(
                    RichText::new("The value of the oauth_token cookie on soundcloud.com")
                        .small()
                        .color(MUTED),
                );
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.paste).password(true).desired_width(260.0));
                    if ui.button("Save").clicked() {
                        self.screen = match auth::normalize_pasted_token(&self.paste) {
                            Some(t) => match auth::save_token(&t) {
                                Ok(()) => {
                                    self.paste.clear();
                                    self.check_session();
                                    Screen::Starting
                                }
                                Err(e) => Screen::SignedOut { error: Some(e) },
                            },
                            None => Screen::SignedOut { error: Some("that doesn't look like a token".into()) },
                        };
                    }
                });
            }
        });
    }

    fn signed_in_ui(&mut self, ui: &mut egui::Ui, user: &User) {
        egui::Panel::bottom("player")
            .resizable(false)
            .exact_size(player_bar::HEIGHT)
            .frame(player_bar::bar_frame())
            .show(ui, |ui| self.player_bar(ui));
        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(200.0)
            .frame(egui::Frame::NONE.fill(SIDEBAR).inner_margin(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Cumulus").heading().color(ACCENT));
                ui.add_space(12.0);
                for nav in Nav::ALL {
                    let selected = self.nav == nav && self.pages.is_empty();
                    let text = RichText::new(nav.label()).size(15.0);
                    if ui.add_sized([176.0, 30.0], egui::Button::selectable(selected, text)).clicked() {
                        self.nav = nav;
                        self.pages.clear();
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(
                        RichText::new("Unofficial app, not affiliated with SoundCloud.")
                            .small()
                            .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(format!("Cumulus {}", crate::update::current_version())).small().color(MUTED));
                        if self.update_ready.is_none() {
                            ui.label(RichText::new("·").small().color(MUTED));
                            if self.checking_update {
                                ui.label(RichText::new("Checking...").small().color(MUTED));
                            } else if ui.link(RichText::new("Check for updates").small()).clicked() {
                                self.check_for_update_now();
                            }
                        }
                    });
                    if let Some(ready) = &self.update_ready {
                        ui.add_space(4.0);
                        let label = match &ready.version {
                            Some(v) => format!("⟳  Update to {v}"),
                            None => "⟳  Restart to update".to_string(),
                        };
                        let tip = format!(
                            "{} is downloaded and verified. Restarts in a second; your settings and sign-in are kept. (Now running {}.)",
                            ready.version.as_deref().map_or("A newer build".to_string(), |v| format!("Cumulus {v}")),
                            crate::update::current_version(),
                        );
                        let button = egui::Button::new(RichText::new(label).strong()).fill(ACCENT);
                        if ui.add_sized([176.0, 28.0], button).on_hover_text(tip).clicked()
                        {
                            self.install_update();
                        }
                    }
                    ui.add_space(4.0);
                    if self.desktop.has_tray()
                        && ui
                            .checkbox(&mut self.close_to_tray, RichText::new("Close to tray, keep playing").small())
                            .changed()
                    {
                        let mut config = Config::load();
                        config.close_to_tray = self.close_to_tray;
                        config.save();
                    }
                    if ui.small_button("Sign out").clicked() {
                        self.sign_out(None);
                    }
                    ui.hyperlink_to(RichText::new(&user.username).strong(), &user.permalink_url);
                });
            });
        if self.show_queue {
            egui::Panel::right("queue")
                .resizable(false)
                .exact_size(330.0)
                .frame(egui::Frame::NONE.fill(SIDEBAR).inner_margin(12.0))
                .show(ui, |ui| self.queue_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(20, 16)))
            .show(ui, |ui| {
                self.status_banner(ui);
                self.content_ui(ui);
            });
    }

    /// One-line message at the top of the content area, shown for a few seconds.
    fn status_banner(&mut self, ui: &mut egui::Ui) {
        if let Some(s) = &self.status {
            if s.at.elapsed() < Duration::from_secs(6) {
                ui.colored_label(if s.error { ERROR } else { ACCENT }, &s.text);
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            } else {
                self.status = None;
            }
        }
    }
}

fn status_line(ui: &mut egui::Ui, error: Option<&str>, refreshing_saved: bool) {
    if let Some(e) = error {
        ui.colored_label(ERROR, e);
    } else if refreshing_saved {
        ui.label(RichText::new("Showing saved copy, refreshing...").small().color(MUTED));
    }
}

impl eframe::App for App {
    /// Runs before every frame and also while the window is hidden, so
    /// playback keeps advancing through the queue when minimised.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
        self.sync_playback();
        self.check_for_update();
        let current = self.queue.current().and_then(|i| i.track.clone());
        let now = self.player.now();
        let playing = !self.player.is_paused() && now.is_some();
        self.desktop.update(current.as_deref(), playing, now.map(|(_, p)| p));
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            if let Some(after) = self.recheck_on_focus.take() {
                self.recheck_library(after);
            }
        }
        self.was_focused = focused;
        if let Some(at) = self.search_edited {
            let waited = at.elapsed();
            if waited >= SEARCH_DEBOUNCE {
                self.run_search();
            } else {
                ctx.request_repaint_after(SEARCH_DEBOUNCE - waited);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // The window's ✕ hides to the tray (if there is one) and playback continues.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.close_to_tray && self.desktop.has_tray() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.set_hidden(true);
        }
        if matches!(self.screen, Screen::SignedIn(_)) {
            self.shortcuts(&ctx);
        }
        // While playing, refresh the progress bar a few times a second.
        if !self.player.is_paused() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let screen = std::mem::replace(&mut self.screen, Screen::Taken);
        let bg = egui::Frame::NONE.fill(ui.visuals().panel_fill);
        egui::CentralPanel::default().frame(bg).show(ui, |ui| match &screen {
            Screen::Starting | Screen::Taken => centered(ui, |ui| {
                ui.spinner();
                ui.label(RichText::new("Connecting...").color(MUTED));
            }),
            Screen::SigningIn => centered(ui, |ui| {
                ui.spinner();
                ui.label("Finish signing in in the SoundCloud window.");
                ui.label(RichText::new("It closes by itself once you're signed in.").small().color(MUTED));
            }),
            Screen::SignedOut { error } => self.signed_out_ui(ui, error.as_deref()),
            Screen::Problem(msg) => {
                let (mut retry, mut out) = (false, false);
                centered(ui, |ui| {
                    ui.colored_label(ERROR, msg);
                    ui.horizontal(|ui| {
                        retry = ui.button("Retry").clicked();
                        out = ui.button("Sign out").clicked();
                    });
                });
                if retry {
                    self.screen = Screen::Starting;
                    self.check_session();
                } else if out {
                    self.sign_out(None);
                }
            }
            Screen::SignedIn(user) => self.signed_in_ui(ui, user),
        });
        // Keep the drawn screen unless an action above switched to another one.
        if matches!(self.screen, Screen::Taken) {
            self.screen = screen;
        }
        self.popout(&ctx);
        self.art.end_frame();
    }
}

fn centered(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.add_space((ui.available_height() * 0.28).max(24.0));
        add(ui);
    });
}
