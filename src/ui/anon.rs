//! Debug builds only: CUMULUS_DEV_ANON=1 shows "you" instead of the
//! signed-in user's name everywhere, for screenshots. In release builds every
//! function here does nothing.

use std::sync::{Arc, RwLock};

use crate::api::models::{Playlist, Selection, Track, User};

static REAL_NAME: RwLock<Option<String>> = RwLock::new(None);
const SHOWN: &str = "you";

/// Turns anonymous mode on for this user (if asked for).
pub fn start(user: &mut User) {
    if cfg!(debug_assertions) && std::env::var_os("CUMULUS_DEV_ANON").is_some() {
        if let Ok(mut n) = REAL_NAME.write() {
            *n = Some(user.username.clone());
        }
        user.username = SHOWN.into();
        user.full_name = None;
        user.permalink_url = "https://soundcloud.com/".into();
    }
}

fn real() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    REAL_NAME.read().ok().and_then(|n| n.clone())
}

pub fn text(s: &mut String) {
    if let Some(name) = real() {
        if s.contains(&name) {
            *s = s.replace(&name, SHOWN);
        }
    }
}

pub fn track(t: &mut Arc<Track>) {
    if let Some(name) = real() {
        if t.user.username == name {
            Arc::make_mut(t).user.username = SHOWN.into();
        }
    }
}

pub fn playlist(p: &mut Arc<Playlist>) {
    if let Some(name) = real() {
        if p.user.username == name {
            Arc::make_mut(p).user.username = SHOWN.into();
        }
    }
}

pub fn user(u: &mut Arc<User>) {
    if let Some(name) = real() {
        if u.username == name {
            Arc::make_mut(u).username = SHOWN.into();
        }
    }
}

pub fn sections(sections: &mut [Selection]) {
    if real().is_none() {
        return;
    }
    for s in sections {
        text(&mut s.title);
        for item in &mut s.items.collection {
            if let crate::api::models::HomeItem::Playlist(p) = item {
                if Some(&p.user.username) == real().as_ref() {
                    p.user.username = SHOWN.into();
                }
            }
        }
    }
}

/// A name field: replaced when it's the user's own.
pub fn name(s: &mut String) {
    if let Some(n) = real() {
        if *s == n {
            *s = SHOWN.into();
        }
    }
}
