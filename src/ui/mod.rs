mod desktop;
mod fonts;
mod hide;
pub use hide::BlockedArtist;
pub use theme::CustomTheme;
mod anon;
mod home;
mod hotkeys;
mod icon;
mod library;
mod local_page;
mod looks;
mod overlay_ui;
mod player_bar;
mod queue;
mod queue_panel;
mod rows;
mod session;
mod settings;
mod theme;
mod track_page;
mod views;
mod visualizer;

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
use theme::{accent, muted, sidebar};

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
    /// A Local files scan finished.
    LocalScanned(Vec<crate::local::LocalTrack>),
    /// A global hotkey was pressed (its id).
    GlobalHotkey(u32),
    /// A wallpaper picture finished loading.
    Wallpaper { key: String, image: Result<egui::ColorImage, String> },
    /// An artist's profile and links (About tab, header numbers).
    ArtistDetails { id: u64, result: Result<(User, Vec<crate::api::models::WebProfile>), ApiError> },
    /// Home's "Continue listening" and "Fresh drops" rows.
    HomeExtras { continue_items: Vec<crate::api::models::HomeItem>, fresh: Vec<Arc<Track>> },
    /// Similar tracks to continue with after `after` (the queue's last track).
    Autoplay { after: u64, result: Result<Vec<Track>, ApiError> },
    /// A newer version is downloaded/verified and ready to install.
    UpdateReady(crate::update::Ready),
    /// Result of pressing "Check for updates".
    UpdateChecked(crate::update::Outcome),
    /// What a pasted soundcloud.com link points at.
    Resolved(Result<crate::api::Resolved, ApiError>),
    /// Part of a track page arrived (details, related tracks, playlists...).
    TrackPage { id: u64, part: track_page::Part },
    /// An artist-offered download finished (or failed).
    Downloaded { id: u64, title: String, result: Result<std::path::PathBuf, String> },
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
    Discover,
    Stream,
    Likes,
    Playlists,
    Reposts,
    History,
    Following,
    Local,
    Search,
    Settings,
}

impl Nav {
    const ALL: [Nav; 11] = [
        Nav::Home,
        Nav::Discover,
        Nav::Stream,
        Nav::Likes,
        Nav::Playlists,
        Nav::Reposts,
        Nav::History,
        Nav::Following,
        Nav::Local,
        Nav::Search,
        Nav::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Nav::Home => "Home",
            Nav::Discover => "Discover",
            Nav::Stream => "Stream",
            Nav::Likes => "Likes",
            Nav::Playlists => "Playlists",
            Nav::Reposts => "Reposts",
            Nav::History => "History",
            Nav::Following => "Following",
            Nav::Local => "Local files",
            Nav::Search => "Search",
            Nav::Settings => "Settings",
        }
    }

    fn feed(self) -> Option<FeedKind> {
        match self {
            Nav::Stream => Some(FeedKind::Stream),
            Nav::Likes => Some(FeedKind::Likes),
            Nav::Playlists => Some(FeedKind::Library),
            Nav::Reposts => Some(FeedKind::Reposts),
            Nav::History => Some(FeedKind::History),
            Nav::Following => Some(FeedKind::Following),
            Nav::Home | Nav::Discover | Nav::Local | Nav::Search | Nav::Settings => None,
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Nav::Stream => "Nothing in your stream yet. Follow some artists on SoundCloud.",
            Nav::Likes => "You haven't liked any tracks yet.",
            Nav::Playlists => "No playlists yet.",
            Nav::Reposts => "You haven't reposted anything yet.",
            Nav::History => "Nothing played yet.",
            Nav::Following => "You don't follow anyone yet.",
            _ => "No results.",
        }
    }
}

/// Tabs on an artist page.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum ArtistTab {
    #[default]
    Tracks,
    Popular,
    Albums,
    Playlists,
    Reposts,
    Related,
    About,
}

impl ArtistTab {
    const ALL: [ArtistTab; 7] = [Self::Tracks, Self::Popular, Self::Albums, Self::Playlists, Self::Reposts, Self::Related, Self::About];

    fn label(self) -> &'static str {
        match self {
            Self::Tracks => "Tracks",
            Self::Popular => "Popular",
            Self::Albums => "Albums",
            Self::Playlists => "Playlists",
            Self::Reposts => "Reposts",
            Self::Related => "Related artists",
            Self::About => "About",
        }
    }

    fn feed(self, id: u64) -> Option<FeedKind> {
        Some(match self {
            Self::Tracks => FeedKind::Artist(id),
            Self::Popular => FeedKind::ArtistTop(id),
            Self::Albums => FeedKind::ArtistAlbums(id),
            Self::Playlists => FeedKind::ArtistPlaylists(id),
            Self::Reposts => FeedKind::ArtistReposts(id),
            Self::Related => FeedKind::ArtistRelated(id),
            Self::About => return None,
        })
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
    Track(track_page::TrackView),
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
    /// Pages left with Back, for Forward (Alt+→).
    forward: Vec<Page>,
    /// Volume before M muted it.
    muted_from: Option<f32>,
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
    /// Tracks being downloaded right now.
    downloading: HashSet<u64>,
    /// Crossfade between tracks, seconds (0 = off).
    crossfade: f32,
    transition: player::Style,
    trim_silence: bool,
    match_volume: bool,
    /// The Transitions list in the Crossfade menu is expanded.
    transitions_open: bool,
    /// Hidden tracks, blocked artists and keywords.
    hide_rules: hide::Hidden,
    /// Artist pages: which tab is open, and the profile + links once loaded.
    artist_tabs: HashMap<u64, ArtistTab>,
    artist_details: HashMap<u64, Result<(User, Vec<crate::api::models::WebProfile>), String>>,
    /// Discover page: what to list and which genre.
    discover_kind: crate::api::SearchKind,
    discover_genre: u8,
    /// Local files: the library, its tracks as `Track`s, local playlists,
    /// which tab is open, the filter, and scan state.
    local_lib: crate::local::Library,
    local_tracks: Vec<Arc<Track>>,
    local_lists: Vec<crate::local::LocalPlaylist>,
    local_tab: Option<usize>,
    local_filter: String,
    local_scanning: bool,
    local_scanned: bool,
    /// Scrobbling: the sender, connected accounts, what's being listened to
    /// (track id, start time, seconds heard, last position, sent), and the
    /// Last.fm sign-in in progress.
    scrobbler: Option<std::sync::mpsc::Sender<crate::scrobble::Job>>,
    lastfm_user: Option<String>,
    listenbrainz_user: Option<String>,
    scrobble_local: bool,
    listening: Option<(u64, u64, f64, f64, bool)>,
    lastfm_pending: Option<String>,
    lb_token_input: String,
    /// Visualizer style and state.
    viz_style: visualizer::Style,
    viz: visualizer::Visualizer,
    /// Mini player: the pop-out settings to restore when leaving it.
    mini: Option<(bool, bool, bool)>,
    /// OBS outputs.
    obs_text_on: bool,
    obs_text_path: String,
    obs_text_format: String,
    obs_text: crate::obs::TextFile,
    obs_overlay_on: bool,
    obs_port: u16,
    obs_overlay: Option<crate::obs::Overlay>,
    /// Keyboard shortcuts, global hotkeys, and the action being recorded
    /// in Settings (and whether that's its global key).
    bindings: hotkeys::Bindings,
    global_keys: Option<hotkeys::Global>,
    recording: Option<(hotkeys::Action, bool)>,
    /// Appearance settings and the loaded wallpaper.
    looks: looks::Looks,
    wall: looks::Wallpaper,
    /// Playback settings mirrored from config.
    audio_device: Option<String>,
    audio_devices: Vec<String>,
    high_quality: bool,
    skip_stuck_secs: u32,
    /// Track position last seen moving, for skipping stuck tracks.
    stuck_check: Option<(u64, f64, Instant)>,
    start_with_windows: bool,
    /// Order of the Likes list (anything but Recent loads every page).
    likes_sort: library::LikesSort,
    /// Text being typed into Settings → Hiding.
    keyword_input: String,
    /// The playlist/mix the queue was started from (its tile shows pause).
    playing_list: Option<u64>,
    /// Restored from the last run: where to start the current track when
    /// play is pressed.
    resume_at: Option<f64>,
    /// Sleep timer: when to stop.
    sleep: Option<player_bar::Sleep>,
    /// Discord status: the app id to use (None = not available), whether
    /// it's on, the background connection, and what was last sent.
    discord_app_id: Option<String>,
    discord_on: bool,
    discord: Option<std::sync::mpsc::Sender<Option<crate::discord::Activity>>>,
    discord_sent: Option<(u64, u64)>,
    /// Equalizer (preamp + 5 bands, dB) and playback speed.
    eq_on: bool,
    eq_gains: [f32; 6],
    speed: f32,
    /// Last saved session, and when, so it's only written when it changed.
    saved_session: Option<session::Saved>,
    session_saved_at: Instant,
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
        fonts::install(&cc.egui_ctx, &Config::load().font);
        let config = Config::load();
        theme::set(&cc.egui_ctx, &config.theme, &config.accent);
        // Debug builds: CUMULUS_DEV_THEME=grey:teal previews colours without saving them.
        if let Some(spec) = std::env::var("CUMULUS_DEV_THEME").ok().filter(|_| cfg!(debug_assertions)) {
            let (base, accent) = spec.split_once(':').unwrap_or((&spec, "purple"));
            theme::set(&cc.egui_ctx, base, accent);
        }
        let overlay_menu = overlay_ui::OverlayMenu::load(&config);
        let api = Arc::new(Api::new(config.client_id.clone()));
        let (tx, rx) = channel();
        let player = {
            let (tx, ctx) = (tx.clone(), cc.egui_ctx.clone());
            Player::new(api.clone(), config.volume, move |e| {
                let _ = tx.send(Msg::Player(e));
                ctx.request_repaint();
            })
        };
        player.set_crossfade(config.crossfade_secs);
        player.set_high_quality(config.high_quality);
        if config.audio_device.is_some() {
            player.set_device(config.audio_device.clone());
        }
        player.set_eq(config.eq_on, config.eq_gains);
        player.set_speed(config.speed);
        player.set_style(player::Style::from_key(&config.transition));
        player.set_trim_silence(config.trim_silence);
        player.set_match_volume(config.match_volume);
        // Debug builds: CUMULUS_DEV_CROSSFADE=<secs> overrides it without saving.
        #[cfg(debug_assertions)]
        if let Some(secs) = std::env::var("CUMULUS_DEV_CROSSFADE").ok().and_then(|s| s.parse().ok()) {
            player.set_crossfade(secs);
        }
        // Debug builds: CUMULUS_DEV_SPEED=1.25 / CUMULUS_DEV_EQ=<preset index> without saving.
        #[cfg(debug_assertions)]
        if let Some(speed) = std::env::var("CUMULUS_DEV_SPEED").ok().and_then(|s| s.parse().ok()) {
            player.set_speed(speed);
        }
        #[cfg(debug_assertions)]
        if let Some(i) = std::env::var("CUMULUS_DEV_EQ").ok().and_then(|s| s.parse::<usize>().ok()) {
            player.set_eq(true, crate::player::eq::PRESETS[i.min(crate::player::eq::PRESETS.len() - 1)].1);
        }
        #[cfg(debug_assertions)]
        if let Ok(style) = std::env::var("CUMULUS_DEV_TRANSITION") {
            player.set_style(player::Style::from_key(&style));
            player.set_match_volume(std::env::var_os("CUMULUS_DEV_MATCH").is_some());
        }
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
            forward: Vec::new(),
            muted_from: None,
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
            downloading: HashSet::new(),
            crossfade: config.crossfade_secs,
            transition: player::Style::from_key(&config.transition),
            trim_silence: config.trim_silence,
            match_volume: config.match_volume,
            transitions_open: false,
            playing_list: None,
            hide_rules: hide::Hidden::load(&config),
            keyword_input: String::new(),
            likes_sort: library::LikesSort::Recent,
            local_lib: crate::local::Library::load(),
            local_tracks: Vec::new(),
            local_lists: crate::local::load_playlists(),
            local_tab: None,
            local_filter: String::new(),
            local_scanning: false,
            local_scanned: false,
            scrobbler: None,
            lastfm_user: config.lastfm_user.clone(),
            listenbrainz_user: config.listenbrainz_user.clone(),
            scrobble_local: config.scrobble_local,
            listening: None,
            lastfm_pending: None,
            lb_token_input: String::new(),
            viz_style: visualizer::Style::from_key(&config.visualizer),
            viz: visualizer::Visualizer::default(),
            mini: None,
            obs_text_on: config.obs_text,
            obs_text_path: config.obs_text_path.clone().unwrap_or_else(crate::obs::default_text_path),
            obs_text_format: config.obs_text_format.clone(),
            obs_text: crate::obs::TextFile::default(),
            obs_overlay_on: config.obs_overlay,
            obs_port: config.obs_port,
            obs_overlay: None,
            bindings: hotkeys::Bindings::load(&config),
            global_keys: None,
            recording: None,
            looks: looks::Looks::load(&config),
            wall: looks::Wallpaper::default(),
            audio_device: config.audio_device.clone(),
            audio_devices: Vec::new(),
            high_quality: config.high_quality,
            skip_stuck_secs: config.skip_stuck_secs,
            stuck_check: None,
            start_with_windows: crate::autostart::enabled(),
            artist_tabs: HashMap::new(),
            artist_details: HashMap::new(),
            discover_kind: crate::api::SearchKind::Tracks,
            discover_genre: 0,
            resume_at: None,
            sleep: None,
            discord_app_id: config
                .discord_app_id
                .clone()
                .or_else(|| Some(crate::discord::APP_ID.to_string()))
                .filter(|id| !id.is_empty()),
            // Debug builds: CUMULUS_DEV_DISCORD=1 turns it on without saving.
            discord_on: config.discord_status || (cfg!(debug_assertions) && std::env::var_os("CUMULUS_DEV_DISCORD").is_some()),
            discord: None,
            discord_sent: None,
            eq_on: config.eq_on,
            eq_gains: config.eq_gains,
            speed: config.speed,
            saved_session: None,
            session_saved_at: Instant::now(),
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
        app.apply_looks();
        app.start_global_hotkeys();
        app.restart_scrobbler();
        // Debug builds: CUMULUS_DEV_LOCAL_DIR=<folder> uses a throwaway library.
        #[cfg(debug_assertions)]
        if let Ok(dir) = std::env::var("CUMULUS_DEV_LOCAL_DIR") {
            app.local_lib = crate::local::Library { folders: vec![dir], no_save: true, ..Default::default() };
        }
        app.local_tracks = app.local_lib.tracks.iter().map(|t| Arc::new(t.to_track())).collect();
        app.local_lists_changed();
        // Debug builds: CUMULUS_DEV_VIZ=bars / CUMULUS_DEV_OBS=1 without saving.
        #[cfg(debug_assertions)]
        if let Ok(v) = std::env::var("CUMULUS_DEV_VIZ") {
            app.viz_style = visualizer::Style::from_key(&v);
        }
        #[cfg(debug_assertions)]
        if std::env::var_os("CUMULUS_DEV_OBS").is_some() {
            app.obs_overlay_on = true;
            app.obs_text_on = true;
        }
        // Debug builds: CUMULUS_DEV_CUSTOM=<gallery theme> / CUMULUS_DEV_WALL=artwork, unsaved.
        #[cfg(debug_assertions)]
        if let Ok(name) = std::env::var("CUMULUS_DEV_CUSTOM") {
            if let Some(t) = theme::gallery().into_iter().find(|t| t.name == name) {
                theme::set_custom(&app.ctx, &t);
                app.looks.custom = Some(t);
            }
        }
        #[cfg(debug_assertions)]
        if let Ok(mode) = std::env::var("CUMULUS_DEV_WALL") {
            app.looks.wall_mode = looks::WallMode::from_key(&mode);
        }
        app.player.set_visualizer(app.viz_style != visualizer::Style::Off);
        if app.obs_overlay_on {
            app.set_obs_overlay(true);
        }
        // Started with Windows: stay in the tray until clicked.
        if crate::autostart::started_minimized() && app.desktop.has_tray() {
            app.set_hidden(true);
        }
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
            Ok("history") => Nav::History,
            Ok("following") => Nav::Following,
            Ok("discover") => Nav::Discover,
            Ok("local") => Nav::Local,
            Ok("settings") => Nav::Settings,
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
        self.resume_at = None;
        self.saved_session = None;
        session::Saved::clear();
        auth::sign_out();
        self.api.set_token(None);
        self.feeds.clear();
        self.home = home::HomeState::default();
        self.clear_pages();
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
            FeedKind::History => Api::play_history_url(),
            FeedKind::Following => format!("/users/{}/followings?limit=50", self.me),
            other => other.url().unwrap_or_default(),
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
        self.push_page(Page::Playlist(PlaylistView::new(playlist)));
        self.spawn(move |api, send| send(Msg::Playlist { id, result: library::fetch_playlist(api, id) }));
    }

    fn open_artist(&mut self, artist: ArtistInfo) {
        // Keep only artist lists that are still reachable via Back.
        let open: HashSet<u64> = self
            .pages
            .iter()
            .filter_map(|p| match p {
                Page::Artist(a) => Some(a.id),
                _ => None,
            })
            .collect();
        self.feeds.retain(|k, _| k.artist().is_none_or(|id| open.contains(&id)));
        let id = artist.id;
        if !self.artist_details.contains_key(&id) {
            self.spawn(move |api, send| send(Msg::ArtistDetails { id, result: api.artist(id) }));
        }
        self.push_page(Page::Artist(artist));
    }

    /// Opens a page on top of the current one (clears Forward).
    fn push_page(&mut self, page: Page) {
        self.forward.clear();
        self.pages.push(page);
    }

    /// Back to the previous page (Alt+← / the Back button).
    fn go_back(&mut self) {
        if let Some(page) = self.pages.pop() {
            self.forward.push(page);
        }
    }

    /// Forward again after going back (Alt+→).
    fn go_forward(&mut self) {
        if let Some(page) = self.forward.pop() {
            self.pages.push(page);
        }
    }

    /// Leaves every page (switching sidebar sections).
    fn clear_pages(&mut self) {
        self.pages.clear();
        self.forward.clear();
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
        // A pasted soundcloud.com link opens what it points at.
        if crate::api::is_soundcloud_link(&q) {
            if q != self.search_query {
                self.search_query = q.clone();
                self.info("Opening link…");
                self.spawn(move |api, send| send(Msg::Resolved(api.resolve(&q))));
            }
            return;
        }
        if q != self.search_query {
            self.search_query = q;
            self.feeds.retain(|k, _| !matches!(k, FeedKind::Search(_)));
        }
    }

    fn handle(&mut self, mut msg: Msg) {
        if cfg!(debug_assertions) {
            anonymize(&mut msg);
        }
        match msg {
            Msg::Session(Ok(Some(user))) => {
                self.me = user.id;
                self.screen = Screen::SignedIn(user);
                self.load_liked();
                self.restore_session();
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
                    let hidden = &self.hide_rules;
                    feed.rows.retain(|r| !hidden.row(r));
                    // A sorted Likes list needs all of it: keep loading.
                    if kind == FeedKind::Likes && self.likes_sort != library::LikesSort::Recent && !cached {
                        self.likes_sort.sort(&mut feed.rows);
                        if feed.next.is_some() && !feed.loading {
                            self.load_feed(FeedKind::Likes, false);
                        }
                    }
                }
            }
            Msg::TrackPage { id, part } => self.apply_track_page(id, part),
            Msg::Resolved(result) => match result {
                Ok(crate::api::Resolved::Track(t)) => {
                    self.status = None;
                    self.open_track(Arc::new(t));
                }
                Ok(crate::api::Resolved::Playlist(p)) => {
                    self.status = None;
                    self.open_playlist(Arc::new(p));
                }
                Ok(crate::api::Resolved::User(u)) => {
                    self.status = None;
                    self.open_artist(ArtistInfo {
                        id: u.id,
                        username: u.username,
                        permalink_url: u.permalink_url,
                        avatar_url: u.avatar_url,
                    });
                }
                Err(e) => self.error(format!("Couldn't open that link: {e}")),
            },
            Msg::Playlist { id, result } => {
                if let Some(view) = self.playlist_page(id) {
                    view.apply_playlist(result);
                }
            }
            Msg::Tracks { playlist_id, asked, result } => {
                let hidden = std::mem::take(&mut self.hide_rules);
                if let Some(view) = self.playlist_page(playlist_id) {
                    view.apply_tracks(&asked, result);
                    view.entries.retain(|e| !matches!(e, library::Entry::Loaded(t) if hidden.track(t)));
                }
                self.hide_rules = hidden;
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
            Msg::History(Ok(tracks)) => {
                let hidden = &self.hide_rules;
                self.home.history = tracks.into_iter().filter(|t| !hidden.track(t)).collect();
            }
            Msg::Wallpaper { key, image } => self.wallpaper_loaded(key, image),
            Msg::LocalScanned(tracks) => self.local_scanned_done(tracks),
            Msg::GlobalHotkey(id) => {
                if let Some(a) = self.global_keys.as_ref().and_then(|g| g.action(id)) {
                    self.run_action(a);
                }
            }
            Msg::ArtistDetails { id, result } => {
                self.artist_details.insert(id, result.map_err(|e| e.to_string()));
            }
            Msg::HomeExtras { continue_items, fresh } => {
                let hidden = &self.hide_rules;
                self.home.continue_items = continue_items;
                self.home.fresh = fresh.into_iter().filter(|t| !hidden.track(t)).collect();
            }
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
            Msg::Downloaded { id, title, result } => {
                self.downloading.remove(&id);
                match result {
                    Ok(path) => self.info(format!("Saved “{title}” to {}", path.display())),
                    Err(e) => self.error(format!("Couldn't download “{title}”: {e}")),
                }
            }
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
                self.save_session(true);
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
        self.save_session(true);
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
        self.playing_list = None;
        self.resume_at = None;
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
            if t.availability() != crate::api::models::Availability::Playable || self.queue.contains(t.id) || self.hide_rules.track(&t) {
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
            self.info(format!("Autoplay: added {added} similar tracks to the queue (∞ in the player bar turns it off)"));
        }
    }

    /// "Crossfade" button in the sidebar: a 0-12 s slider (0 = off).
    fn crossfade_menu(&mut self, ui: &mut egui::Ui) {
        let label = if self.crossfade > 0.0 {
            format!("♫ Crossfade: {} s", self.crossfade.round())
        } else {
            "♫ Crossfade: off".to_string()
        };
        let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::MenuButton::new(RichText::new(label).small())
            .config(stay_open)
            .ui(ui, |ui| self.crossfade_settings(ui));
    }

    /// The contents of the Crossfade menu.
    fn crossfade_settings(&mut self, ui: &mut egui::Ui) {
        {
            ui.set_min_width(250.0);
            ui.label(RichText::new("Crossfade").strong());
            ui.label(
                RichText::new("Fades each track out while the next fades in, when one track follows on from another. Skipping is still instant.")
                    .small()
                    .color(muted()),
            );
            ui.add_space(4.0);
            ui.spacing_mut().slider_width = 180.0;
            let r = ui.add(
                egui::Slider::new(&mut self.crossfade, 0.0..=12.0)
                    .step_by(1.0)
                    .trailing_fill(true)
                    .custom_formatter(|v, _| if v == 0.0 { "Off".into() } else { format!("{v} s") }),
            );
            if r.changed() {
                self.player.set_crossfade(self.crossfade);
            }
            let mut save = r.drag_stopped() || (r.changed() && !r.dragged());

            ui.add_space(8.0);
            ui.label(RichText::new("Transitions").strong());
            ui.add_enabled_ui(self.crossfade > 0.0, |ui| {
                let arrow = if self.transitions_open { "▴" } else { "▾" };
                let header = egui::Button::new(format!("{}   {arrow}", self.transition.label())).min_size(egui::vec2(236.0, 26.0));
                if ui.add(header).clicked() {
                    self.transitions_open = !self.transitions_open;
                }
                if self.transitions_open {
                    egui::Frame::NONE
                        .fill(sidebar())
                        .corner_radius(6.0)
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            ui.set_min_width(228.0);
                            for style in player::Style::ALL {
                                let selected = self.transition == style;
                                let r = ui
                                    .add_sized([228.0, 24.0], egui::Button::selectable(selected, style.label()))
                                    .on_hover_text(style.description());
                                if r.clicked() {
                                    self.transition = style;
                                    self.player.set_style(style);
                                    self.transitions_open = false;
                                    save = true;
                                }
                            }
                        });
                }
            });
            let note = if self.crossfade > 0.0 { self.transition.description() } else { "Set a crossfade above to use these." };
            ui.label(RichText::new(note).small().color(muted()));

            ui.add_space(8.0);
            if ui
                .checkbox(&mut self.trim_silence, "Trim silence")
                .on_hover_text("Skips silence at the end and start of tracks, so transitions happen over music, not quiet gaps. Works with or without a crossfade.")
                .changed()
            {
                self.player.set_trim_silence(self.trim_silence);
                save = true;
            }
            if ui
                .checkbox(&mut self.match_volume, "Match volume")
                .on_hover_text("Evens out loudness between tracks, so a quiet upload after a loud one doesn't feel like a drop.")
                .changed()
            {
                self.player.set_match_volume(self.match_volume);
                save = true;
            }

            if save {
                let mut config = Config::load();
                config.crossfade_secs = self.crossfade;
                config.transition = self.transition.key().into();
                config.trim_silence = self.trim_silence;
                config.match_volume = self.match_volume;
                config.save();
            }
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

    /// Keeps the Discord status in step with what's playing: sends when the
    /// track, play/pause or position (a seek) changes.
    fn update_discord(&mut self) {
        let Some(app_id) = self.discord_app_id.clone().filter(|_| self.discord_on) else {
            if let Some(tx) = self.discord.take() {
                let _ = tx.send(None);
            }
            self.discord_sent = None;
            return;
        };
        let tx = self.discord.get_or_insert_with(|| crate::discord::start(app_id)).clone();
        let now = self.player.now().filter(|_| !self.player.is_paused());
        let track = self.queue.current().and_then(|i| i.track.clone());
        let want = match (now, track) {
            (Some((id, pos)), Some(t)) if t.id == id => Some(crate::discord::Activity::new(
                &t.title,
                &t.user.username,
                t.artwork().map(|u| crate::api::models::sized_art(u, "t500x500")),
                &t.permalink_url,
                pos,
                t.duration as f64 / 1000.0,
                self.speed,
            )),
            _ => None,
        };
        let key = want.as_ref().map(|a| (a.end_ms - a.start_ms, a.start_ms));
        // Same track and start time (within 2 s): nothing to update.
        let same = match (self.discord_sent, key) {
            (None, None) => true,
            (Some((d1, s1)), Some((d2, s2))) => d1 == d2 && s1.abs_diff(s2) < 2000,
            _ => false,
        };
        if !same {
            let _ = tx.send(want);
            self.discord_sent = key;
        }
    }

    /// "Show on Discord" in the sidebar.
    fn discord_toggle(&mut self, ui: &mut egui::Ui) {
        let available = self.discord_app_id.is_some();
        let tip = if available {
            "Shows \"Listening to\" with the song, artist and cover on your Discord profile (needs the Discord desktop app)."
        } else {
            "Not set up in this build yet."
        };
        let r = ui
            .add_enabled(available, egui::Checkbox::new(&mut self.discord_on, RichText::new("Show on Discord").small()))
            .on_hover_text(tip)
            .on_disabled_hover_text(tip);
        if r.changed() {
            let mut config = Config::load();
            config.discord_status = self.discord_on;
            config.save();
        }
    }

    /// "Skip stuck tracks": if playback is on but the position hasn't moved
    /// for the set time (a stalled stream), go to the next track.
    fn skip_if_stuck(&mut self, ctx: &egui::Context) {
        if self.skip_stuck_secs == 0 || self.player.is_paused() {
            self.stuck_check = None;
            return;
        }
        let Some(id) = self.queue.current_id() else { return };
        let pos = self.player.now().filter(|(i, _)| *i == id).map_or(-1.0, |(_, p)| p);
        match self.stuck_check {
            Some((sid, spos, since)) if sid == id && (pos - spos).abs() < 0.05 => {
                if since.elapsed() >= Duration::from_secs(u64::from(self.skip_stuck_secs)) {
                    self.stuck_check = None;
                    let title = self.queue_title(id);
                    self.error(format!("“{title}” got stuck, so it was skipped."));
                    self.next();
                }
            }
            _ => self.stuck_check = Some((id, pos, Instant::now())),
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    /// Last.fm key/secret: built in, or the user's own from config.
    fn lastfm_app(&self) -> Option<(String, String)> {
        let c = Config::load();
        match (c.lastfm_api_key, c.lastfm_api_secret) {
            (Some(k), Some(s)) if !k.is_empty() && !s.is_empty() => Some((k, s)),
            _ if !crate::scrobble::LASTFM_KEY.is_empty() => {
                Some((crate::scrobble::LASTFM_KEY.to_string(), crate::scrobble::LASTFM_SECRET.to_string()))
            }
            _ => None,
        }
    }

    /// (Re)starts the scrobbler with the connected accounts (or stops it).
    fn restart_scrobbler(&mut self) {
        let lastfm = self.lastfm_app().zip(crate::scrobble::load_secret("lastfm-session")).map(|((k, s), sk)| (k, s, sk));
        let listenbrainz = crate::scrobble::load_secret("listenbrainz-token");
        if lastfm.is_none() && listenbrainz.is_none() {
            self.scrobbler = None;
            return;
        }
        let tx = self.scrobbler.get_or_insert_with(|| crate::scrobble::start(self.api.agent())).clone();
        let _ = tx.send(crate::scrobble::Job::Accounts(crate::scrobble::Accounts { lastfm, listenbrainz }));
    }

    /// Counts how long the current track has really been heard and scrobbles
    /// it once that's enough (half, or 4 minutes).
    fn track_listening(&mut self) {
        let Some(tx) = self.scrobbler.clone() else { return };
        let Some(track) = self.queue.current().and_then(|i| i.track.clone()) else { return };
        if track.local_path.is_some() && !self.scrobble_local {
            return;
        }
        let pos = self.player.now().filter(|(id, _)| *id == track.id).map(|(_, p)| p);
        let make_play = || {
            let (artist, title) = crate::scrobble::split_title(&track.user.username, &track.title);
            crate::scrobble::Play { artist, title, duration_secs: track.duration / 1000, started: 0, url: track.permalink_url.clone() }
        };
        match &mut self.listening {
            Some((id, ..)) if *id == track.id => {}
            _ => {
                // A new track: announce "now playing".
                self.listening = Some((track.id, crate::scrobble::now(), 0.0, pos.unwrap_or(0.0), false));
                if pos.is_some() {
                    let _ = tx.send(crate::scrobble::Job::NowPlaying(make_play()));
                }
            }
        }
        let Some((_, started, heard, last, sent)) = &mut self.listening else { return };
        if let Some(p) = pos.filter(|_| !self.player.is_paused()) {
            let step = p - *last;
            // Normal playback moves forward a little; seeks don't count.
            if (0.0..2.0).contains(&step) {
                *heard += step;
            }
            *last = p;
        }
        if !*sent && crate::scrobble::heard_enough(*heard, track.duration / 1000) {
            *sent = true;
            let mut play = make_play();
            play.started = *started;
            let _ = tx.send(crate::scrobble::Job::Scrobble(play));
        }
    }

    /// Keeps the OBS text file and overlay up to date.
    fn update_obs(&mut self) {
        if !self.obs_text_on && self.obs_overlay.is_none() {
            return;
        }
        let np = self.now_playing();
        if self.obs_text_on {
            let text = if np.track_id == 0 { String::new() } else { crate::obs::format_text(&self.obs_text_format, &np.title, &np.artist) };
            if let Err(e) = self.obs_text.write(&self.obs_text_path, &text) {
                self.obs_text_on = false;
                self.error(format!("Couldn't write the OBS text file: {e}"));
            }
        }
        if let Some(o) = &self.obs_overlay {
            o.set(crate::obs::Now {
                title: np.title,
                artist: np.artist,
                artwork: np.artwork,
                playing: np.playing,
                position: np.position,
                duration: np.duration,
                accent: theme::to_hex(accent()),
            });
        }
    }

    /// Starts/stops the OBS browser overlay server.
    fn set_obs_overlay(&mut self, on: bool) {
        if !on {
            self.obs_overlay = None; // the listener thread keeps the port until exit; harmless
            return;
        }
        if self.obs_overlay.as_ref().is_some_and(|o| o.port == self.obs_port) {
            return;
        }
        match crate::obs::Overlay::start(self.obs_port) {
            Ok(o) => self.obs_overlay = Some(o),
            Err(e) => {
                self.obs_overlay_on = false;
                self.error(format!("Couldn't start the OBS overlay: {e}"));
            }
        }
    }

    /// Mini player: just the always-on-top pop-out with controls.
    fn set_mini(&mut self, on: bool) {
        if on && self.mini.is_none() {
            let m = &mut self.overlay_menu;
            self.mini = Some((m.popout, m.on_top, m.controls));
            (m.popout, m.on_top, m.controls) = (true, true, true);
            self.set_hidden(true);
        } else if !on {
            if let Some((popout, on_top, controls)) = self.mini.take() {
                let m = &mut self.overlay_menu;
                (m.popout, m.on_top, m.controls) = (popout, on_top, controls);
            }
            self.set_hidden(false);
        }
    }

    /// Registers the global hotkeys (only if any are set).
    fn start_global_hotkeys(&mut self) {
        if self.bindings.global.values().all(String::is_empty) && self.global_keys.is_none() {
            return;
        }
        if self.global_keys.is_none() {
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            self.global_keys = Some(hotkeys::Global::new(move |id| {
                let _ = tx.send(Msg::GlobalHotkey(id));
                ctx.request_repaint();
            }));
        }
        let failed = self.global_keys.as_mut().map(|g| g.set(&self.bindings)).unwrap_or_default();
        if !failed.is_empty() {
            self.error(format!("Another app already uses {}: pick a different global key.", failed.join(", ")));
        }
    }

    /// Re-applies the hiding rules to everything already loaded.
    fn rehide(&mut self) {
        let hidden = &self.hide_rules;
        for feed in self.feeds.values_mut() {
            feed.rows.retain(|r| !hidden.row(r));
        }
        self.home.history.retain(|t| !hidden.track(t));
        for page in &mut self.pages {
            match page {
                Page::Playlist(v) => v.entries.retain(|e| !matches!(e, library::Entry::Loaded(t) if hidden.track(t))),
                Page::Track(v) => v.hide(hidden),
                Page::Artist(_) => {}
            }
        }
    }

    /// Puts back the queue from the last run, paused at the same spot.
    fn restore_session(&mut self) {
        if !self.queue.is_empty() {
            return;
        }
        let Some(saved) = session::Saved::load() else { return };
        // Local files come back from the library (dropped if they're gone).
        let mut current = saved.current;
        let mut items = Vec::new();
        for (i, &id) in saved.ids.iter().enumerate() {
            if crate::local::is_local(id) {
                match self.local_lib.find(id) {
                    Some(t) => items.push(QueueItem { id, track: Some(Arc::new(t.to_track())) }),
                    None if i < saved.current => current = current.saturating_sub(1),
                    None => {}
                }
            } else {
                items.push(QueueItem { id, track: None });
            }
        }
        if items.is_empty() {
            return;
        }
        self.queue.set(items, current);
        self.queue.repeat = saved.repeat();
        self.resume_at = Some(saved.position);
        self.saved_session = Some(saved);
    }

    /// Writes the session every few seconds while it changes (or right now).
    fn save_session(&mut self, now: bool) {
        if !now && self.session_saved_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.session_saved_at = Instant::now();
        let position = match self.player.now() {
            Some((id, pos)) if Some(id) == self.queue.current_id() => pos,
            _ => self.resume_at.unwrap_or(0.0),
        };
        let Some(saved) = session::Saved::of(&self.queue, position) else { return };
        if self.saved_session.as_ref() != Some(&saved) {
            saved.save();
            self.saved_session = Some(saved);
        }
    }

    /// Fetches details for queue entries only known by id (once each).
    fn fetch_queue_tracks(&mut self, ids: Vec<u64>) {
        // Local files are always complete; only SoundCloud ids are fetched.
        let ids: Vec<u64> = ids.into_iter().filter(|id| !crate::local::is_local(*id) && self.queue_requested.insert(*id)).collect();
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
            ui.label(RichText::new("A lightweight player for your SoundCloud library").color(muted()));
            ui.add_space(18.0);
            if ui
                .add(egui::Button::new(RichText::new("Sign in with SoundCloud").strong().color(theme::on_accent())).fill(accent()))
                .clicked()
            {
                self.start_login();
            }
            ui.label(
                RichText::new("Opens SoundCloud's own sign-in page in a small window.")
                    .small()
                    .color(muted()),
            );
            if let Some(e) = error {
                ui.add_space(8.0);
                ui.colored_label(theme::error(), e);
            }
            ui.add_space(14.0);
            ui.toggle_value(&mut self.paste_open, "Paste a token instead");
            if self.paste_open {
                ui.label(
                    RichText::new("The value of the oauth_token cookie on soundcloud.com")
                        .small()
                        .color(muted()),
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
        let player = if self.looks.player_on_top { egui::Panel::top("player") } else { egui::Panel::bottom("player") };
        player
            .resizable(false)
            .exact_size(player_bar::HEIGHT)
            .frame(player_bar::bar_frame())
            .show(ui, |ui| self.player_bar(ui));
        let side = if self.looks.sidebar_right { egui::Panel::right("sidebar") } else { egui::Panel::left("sidebar") };
        side
            .resizable(false)
            .exact_size(200.0)
            .frame(egui::Frame::NONE.fill(sidebar()).inner_margin(12.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Cumulus").heading().color(accent()));
                ui.add_space(12.0);
                for nav in Nav::ALL {
                    let selected = self.nav == nav && self.pages.is_empty();
                    let text = RichText::new(nav.label()).size(15.0);
                    if ui.add_sized([176.0, 27.0], egui::Button::selectable(selected, text)).clicked() {
                        self.nav = nav;
                        self.clear_pages();
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(
                        RichText::new("Unofficial app, not affiliated with SoundCloud.")
                            .small()
                            .color(muted()),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(RichText::new(format!("Cumulus {}", crate::update::current_version())).small().color(muted()));
                        if self.update_ready.is_none() {
                            ui.label(RichText::new("·").small().color(muted()));
                            if self.checking_update {
                                ui.label(RichText::new("Checking...").small().color(muted()));
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
                        let button = egui::Button::new(RichText::new(label).strong().color(theme::on_accent())).fill(accent());
                        if ui.add_sized([176.0, 28.0], button).on_hover_text(tip).clicked()
                        {
                            self.install_update();
                        }
                    }
                    ui.add_space(4.0);
                    self.crossfade_menu(ui);
                    if let Some((base, accent)) = theme::appearance_menu(ui) {
                        theme::set(ui.ctx(), base, accent);
                        self.looks.custom = None;
                        let mut config = Config::load();
                        (config.theme, config.accent) = (base.into(), accent.into());
                        config.custom_theme = None;
                        config.save();
                    }
                    ui.horizontal(|ui| {
                        ui.hyperlink_to(RichText::new(&user.username).strong(), &user.permalink_url);
                        if ui.small_button("Sign out").clicked() {
                            self.sign_out(None);
                        }
                    });
                });
            });
        if self.show_queue {
            egui::Panel::right("queue")
                .resizable(false)
                .exact_size(330.0)
                .frame(egui::Frame::NONE.fill(sidebar()).inner_margin(12.0))
                .show(ui, |ui| self.queue_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(20, 16)))
            .show(ui, |ui| {
                self.paint_wallpaper(ui);
                self.status_banner(ui);
                self.content_ui(ui);
            });
    }

    /// One-line message at the top of the content area, shown for a few seconds.
    fn status_banner(&mut self, ui: &mut egui::Ui) {
        if let Some(s) = &self.status {
            if s.at.elapsed() < Duration::from_secs(6) {
                ui.colored_label(if s.error { theme::error() } else { accent() }, &s.text);
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            } else {
                self.status = None;
            }
        }
    }
}

fn status_line(ui: &mut egui::Ui, error: Option<&str>, refreshing_saved: bool) {
    if let Some(e) = error {
        ui.colored_label(theme::error(), e);
    } else if refreshing_saved {
        ui.label(RichText::new("Showing saved copy, refreshing...").small().color(muted()));
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
        self.update_obs();
        self.track_listening();
        self.skip_if_stuck(ctx);
        self.sleep_tick(ctx);
        self.update_discord();
        self.save_session(false);
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
            self.handle_dropped_files(&ctx);
            self.drop_hint(&ctx);
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
                ui.label(RichText::new("Connecting...").color(muted()));
            }),
            Screen::SigningIn => centered(ui, |ui| {
                ui.spinner();
                ui.label("Finish signing in in the SoundCloud window.");
                ui.label(RichText::new("It closes by itself once you're signed in.").small().color(muted()));
            }),
            Screen::SignedOut { error } => self.signed_out_ui(ui, error.as_deref()),
            Screen::Problem(msg) => {
                let (mut retry, mut out) = (false, false);
                centered(ui, |ui| {
                    ui.colored_label(theme::error(), msg);
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
        // Debug builds: CUMULUS_DEV_ARTIST=<id>:<name> opens an artist page once.
        #[cfg(debug_assertions)]
        if let Some((id, name)) = std::env::var("CUMULUS_DEV_ARTIST").ok().and_then(|s| {
            let (id, name) = s.split_once(':')?;
            Some((id.parse::<u64>().ok()?, name.to_string()))
        }) {
            if matches!(self.screen, Screen::SignedIn(_)) {
                // SAFETY: only the UI thread reads this dev-only variable.
                unsafe { std::env::remove_var("CUMULUS_DEV_ARTIST") };
                let url = format!("https://soundcloud.com/{name}");
                self.open_artist(ArtistInfo { id, username: name, permalink_url: url, avatar_url: None });
            }
        }
        // Debug builds: CUMULUS_DEV_TRACK=<id> opens that track's page once.
        #[cfg(debug_assertions)]
        if let Some(id) = std::env::var("CUMULUS_DEV_TRACK").ok().and_then(|s| s.parse::<u64>().ok()) {
            if matches!(self.screen, Screen::SignedIn(_)) {
                // SAFETY: only the UI thread reads this dev-only variable.
                unsafe { std::env::remove_var("CUMULUS_DEV_TRACK") };
                let stub = format!(r#"{{"id":{id},"title":"…","user":{{}}}}"#);
                if let Ok(t) = serde_json::from_str::<Track>(&stub) {
                    self.open_track(Arc::new(t));
                }
            }
        }
        // Debug builds: CUMULUS_DEV_XFADE_MENU=1 shows the Crossfade menu's contents
        // in a window (with Transitions expanded) for screenshots.
        #[cfg(debug_assertions)]
        if std::env::var_os("CUMULUS_DEV_XFADE_MENU").is_some() {
            if ctx.cumulative_frame_nr() < 2 {
                self.transitions_open = true;
                self.crossfade = self.crossfade.max(6.0);
            }
            egui::Window::new("Crossfade (debug)").default_pos([420.0, 140.0]).show(&ctx, |ui| self.crossfade_settings(ui));
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

/// Debug builds with CUMULUS_DEV_ANON=1: the user's own name becomes "you"
/// in everything that arrives, for screenshots (see `anon`).
fn anonymize(msg: &mut Msg) {
    use crate::api::models::{HomeItem, PlaylistEntry};
    match msg {
        Msg::Session(Ok(Some(user))) => anon::start(user),
        Msg::Feed { result: Ok(page), .. } => {
            for r in &mut page.rows {
                match r {
                    library::Row::Track { track, .. } => anon::track(track),
                    library::Row::Playlist { playlist, .. } => anon::playlist(playlist),
                    library::Row::User(u) => anon::user(u),
                }
            }
        }
        Msg::Home { result: Ok(sections), .. } => anon::sections(sections),
        Msg::History(Ok(tracks)) | Msg::QueueTracks(tracks) => tracks.iter_mut().for_each(anon::track),
        Msg::HomeExtras { continue_items, fresh } => {
            fresh.iter_mut().for_each(anon::track);
            for it in continue_items {
                if let HomeItem::Playlist(p) = it {
                    anon::name(&mut p.user.username);
                }
            }
        }
        Msg::Playlist { result: Ok(p), .. } => {
            anon::name(&mut p.user.username);
            for e in &mut p.tracks {
                if let PlaylistEntry::Full(t) = e {
                    anon::name(&mut t.user.username);
                }
            }
        }
        Msg::Tracks { result: Ok(map), .. } => map.values_mut().for_each(|t| anon::name(&mut t.user.username)),
        Msg::TrackPage { part, .. } => match part {
            track_page::Part::Details(Ok(t)) => anon::name(&mut t.user.username),
            track_page::Part::Related(Ok(v)) => v.iter_mut().for_each(|t| anon::name(&mut t.user.username)),
            track_page::Part::Albums(Ok(v)) => v.iter_mut().for_each(|p| anon::name(&mut p.user.username)),
            track_page::Part::Playlists { result: Ok((v, _)), .. } => v.iter_mut().for_each(|p| anon::name(&mut p.user.username)),
            _ => {}
        },
        _ => {}
    }
}
