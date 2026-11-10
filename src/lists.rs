//! Community ad-block lists: downloading EasyList and EasyPrivacy, caching the text in the
//! data directory, and rebuilding the WebKit filter. The curated list in `blocklist.txt` is
//! always included; these lists add coverage for sites it does not know.

use super::*;
use soup::prelude::*;
use std::{cell::RefCell, fs, path::PathBuf};

/// The lists change daily; a week-old copy is close enough and avoids a startup download.
const REFRESH_SECS: u64 = 7 * 86_400;
/// A filter list is a few megabytes; anything this big is not one.
const MAX_LIST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LISTS_BYTES: u64 = 2 * MAX_LIST_BYTES;

#[derive(Clone, Debug)]
struct Status {
    /// The community-list switch in Settings.
    enabled: bool,
    running: bool,
    /// WebKit accepted the current filter.
    ready: bool,
    rules: usize,
    /// Unix seconds of the downloaded lists (0 when they came with the program or are missing).
    updated: u64,
    error: String,
}

impl Status {
    const fn new() -> Self {
        Self { enabled: false, running: false, ready: false, rules: 0, updated: 0, error: String::new() }
    }
}

thread_local! {
    static STATUS: RefCell<Status> = const { RefCell::new(Status::new()) };
    /// Only one filter compile at a time; a change that arrives meanwhile runs afterwards.
    static REBUILDING: Cell<bool> = const { Cell::new(false) };
    static REBUILD_AGAIN: Cell<bool> = const { Cell::new(false) };
}

fn cache_dir() -> PathBuf {
    settings::data_dir().join("filterlists")
}

fn cache_file(name: &str) -> PathBuf {
    cache_dir().join(name)
}

fn read_cache_file(name: &str) -> Option<String> {
    let path = cache_file(name);
    if fs::metadata(&path).ok()?.len() > MAX_LIST_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// The converted lists and when they were downloaded, if a usable copy is cached.
fn load() -> Option<(filterlists::Converted, u64)> {
    let (a, b) = (read_cache_file("easylist.txt")?, read_cache_file("easyprivacy.txt")?);
    if a.len() as u64 + b.len() as u64 > MAX_LISTS_BYTES {
        return None;
    }
    let stamp: u64 = fs::read_to_string(cache_file("stamp")).ok()?.trim().parse().ok()?;
    let mut converted = filterlists::convert(&a);
    converted.merge(filterlists::convert(&b));
    (converted.count() > 0).then_some((converted, stamp))
}

fn stale(stamp: u64) -> bool {
    suggest::now().saturating_sub(stamp) >= REFRESH_SECS
}

pub fn status_json() -> String {
    STATUS.with(|s| {
        let s = s.borrow();
        format!(
            r#"{{"enabled":{},"running":{},"ready":{},"rules":{},"updated":{},"error":{}}}"#,
            s.enabled, s.running, s.ready, s.rules, s.updated, quote(&s.error)
        )
    })
}

fn set(f: impl FnOnce(&mut Status)) {
    STATUS.with(|s| f(&mut s.borrow_mut()));
}

/// Load the cache, compile, and start a background refresh if it is missing or old.
pub fn init(app: &Sh) {
    let (enabled, adblock) = app.try_borrow().map(|a| (a.settings.community, a.settings.adblock)).unwrap_or((false, false));
    set(|s| {
        s.enabled = enabled;
        s.error.clear();
    });
    let cache = enabled.then(load).flatten();
    let old = cache.as_ref().is_none_or(|(_, stamp)| stale(*stamp));
    compile_with(app, cache.map(|(c, stamp)| {
        set(|s| s.updated = stamp);
        c
    }));
    if enabled && adblock && old {
        refresh(app);
    }
}

/// A setting that affects the filter changed. Recompile and refresh when needed.
pub fn changed(app: &Sh) {
    let (enabled, adblock) = app.try_borrow().map(|a| (a.settings.community, a.settings.adblock)).unwrap_or((false, false));
    set(|s| s.enabled = enabled);
    rebuild(app);
    if enabled && adblock && load().is_none_or(|(_, stamp)| stale(stamp)) {
        refresh(app);
    }
}

fn rebuild(app: &Sh) {
    let enabled = app.try_borrow().map(|a| a.settings.community).unwrap_or(false);
    compile_with(app, enabled.then(load).flatten().map(|(c, stamp)| {
        set(|s| s.updated = stamp);
        c
    }));
}

/// Build the rule list from the curated list plus `community`, and hand it to WebKit.
fn compile_with(app: &Sh, community: Option<filterlists::Converted>) {
    if REBUILDING.get() {
        REBUILD_AGAIN.set(true);
        return;
    }
    let (data_dir, adblock) = app
        .try_borrow()
        .map(|a| (settings::data_dir(), a.settings.adblock))
        .unwrap_or((settings::data_dir(), true));
    if !adblock {
        // nothing is applied, and compiling would only delay the start of the browser
        set(|s| s.ready = false);
        apply_all(app);
        notify(app);
        return;
    }
    let extra = fs::read_to_string(settings::config_dir().join("blocklist.txt")).unwrap_or_default();
    let rules = adblock::rules_json(&extra, community.as_ref());
    set(|s| {
        s.rules = community.as_ref().map_or(0, filterlists::Converted::count);
        s.error.clear();
    });
    REBUILDING.set(true);
    let app = app.clone();
    adblock::compile(&data_dir, rules, move |ok| {
        set(|s| {
            s.ready = ok;
            if !ok && s.error.is_empty() {
                s.error = "WebKit rejected the ad filter".into();
            }
        });
        REBUILDING.set(false);
        apply_all(&app);
        notify(&app);
        if REBUILD_AGAIN.take() {
            rebuild(&app);
        }
    });
}

fn apply_all(app: &Sh) {
    let (views, on) = match app.try_borrow() {
        Ok(a) => (a.tabs.iter().map(|t| t.wv.clone()).collect::<Vec<_>>(), a.settings.adblock),
        Err(_) => return,
    };
    for wv in views {
        adblock::apply(&wv, on);
    }
}

/// Update the open Settings page with the new state, if there is one.
fn notify(app: &Sh) {
    let (status, ready) = (status_json(), adblock::is_ready());
    let views: Vec<_> = app
        .try_borrow()
        .map(|a| {
            a.tabs
                .iter()
                .filter(|t| internal(&t.url) == Some(Internal::Settings))
                .map(|t| t.wv.clone())
                .collect()
        })
        .unwrap_or_default();
    let js = format!("window.zolithLists && zolithLists({status}); window.zolithAdblock && zolithAdblock({ready})");
    for wv in views {
        run_js(&wv, &js);
    }
}

/// Download both lists now (the Settings "Update now" button, or a stale cache).
pub fn refresh(app: &Sh) {
    let enabled = app.try_borrow().map(|a| a.settings.community).unwrap_or(false);
    if !enabled || STATUS.with(|s| s.borrow().running) {
        return;
    }
    set(|s| {
        s.running = true;
        s.error.clear();
    });
    notify(app);
    let app = app.clone();
    glib::MainContext::default().spawn_local(async move {
        let session = soup::Session::new();
        session.set_timeout(30);
        let result = download(&session).await;
        set(|s| s.running = false);
        match result {
            Ok((a, b)) => {
                let mut converted = filterlists::convert(&a);
                converted.merge(filterlists::convert(&b));
                if converted.count() == 0 {
                    set(|s| s.error = "The downloaded lists had no usable rules".into());
                    notify(&app);
                    return;
                }
                if let Err(e) = save_cache(&a, &b) {
                    // still use what we just downloaded, just don't keep it
                    set(|s| s.error = format!("Could not save the lists: {e}"));
                }
                set(|s| {
                    s.updated = suggest::now();
                    s.rules = converted.count();
                });
                // recompile from the cache we just wrote, so a later restart agrees
                rebuild(&app);
            }
            Err(e) => {
                set(|s| s.error = e);
                notify(&app);
            }
        }
    });
}

async fn download(session: &soup::Session) -> Result<(String, String), String> {
    let mut bodies = vec![];
    for (name, url) in filterlists::LISTS {
        let msg = soup::Message::new("GET", url).map_err(|e| format!("{name}: {e}"))?;
        let bytes = session
            .send_and_read_future(&msg, glib::Priority::DEFAULT)
            .await
            .map_err(|e| format!("{name}: {e}"))?;
        if bytes.len() as u64 > MAX_LIST_BYTES {
            return Err(format!("{name}: the reply was too large"));
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // a redirect to an error page, a captive portal, etc.
        if text.len() < 10_000 || !text.contains("[Adblock") {
            return Err(format!("{name}: that was not a filter list"));
        }
        bodies.push(text);
    }
    let b = bodies.pop().ok_or("The second list did not download")?;
    let a = bodies.pop().ok_or("The first list did not download")?;
    Ok((a, b))
}

fn save_cache(a: &str, b: &str) -> std::io::Result<()> {
    fs::create_dir_all(cache_dir())?;
    fs::write(cache_file("easylist.txt"), a)?;
    fs::write(cache_file("easyprivacy.txt"), b)?;
    // written last: a half-finished download is not mistaken for a complete one
    fs::write(cache_file("stamp"), format!("{}\n", suggest::now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_only_used_when_fresh() {
        assert!(stale(0));
        assert!(!stale(suggest::now()));
        assert!(stale(suggest::now().saturating_sub(REFRESH_SECS + 1)));
    }

    #[test]
    fn status_json_is_valid_json() {
        let s = status_json();
        assert!(s.starts_with('{') && s.ends_with('}'));
        for key in ["enabled", "running", "ready", "rules", "updated", "error"] {
            assert!(s.contains(&format!("\"{key}\":")), "{s}");
        }
        set(|s| s.error = "a \"quoted\" error".into());
        assert!(status_json().contains(r#""error":"a \"quoted\" error""#));
        set(|s| s.error.clear());
    }
}
