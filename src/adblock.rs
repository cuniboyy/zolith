//! Ad blocking: a WebKit content filter (network + generic cosmetic rules) compiled once at
//! startup, plus a YouTube script (`SCRIPT`) the caller injects into each tab.
// shortcut: curated domain list, not EasyList. Upgrade to a real filter-list parser if
// coverage isn't enough. Twitch stream ads are stitched into the video server-side and are
// not blocked.

use crate::settings::quote;
use gtk::{gio, glib, glib::translate::*};
use std::{cell::Cell, ffi::CString, path::Path, ptr};
use webkit2gtk::{ffi, WebViewExt};

pub const SCRIPT: &str = include_str!("adblock.js");
const DOMAINS: &str = include_str!("blocklist.txt");

/// URL patterns blocked everywhere (WebKit content-blocker regex subset: no `|`).
const PATTERNS: [&str; 4] = [
    r"youtube\.com/pagead/",
    r"youtube\.com/api/stats/ads",
    r"youtube\.com/ptracking",
    r"youtube\.com/get_midroll_",
];

/// Simple selectors only: one the engine can't parse would reject the whole filter.
const HIDE: &str = ".adsbygoogle, [id^=\"google_ads_iframe\"], [id^=\"div-gpt-ad\"], \
    iframe[src*=\"doubleclick.net\"], iframe[src*=\"googlesyndication.com\"], \
    [id^=\"taboola-\"], .OUTBRAIN";

thread_local! {
    static FILTER: Cell<*mut ffi::WebKitUserContentFilter> = const { Cell::new(ptr::null_mut()) };
    static DONE: Cell<bool> = const { Cell::new(false) };
}

// The user's blocklist is untrusted text going into a regex inside JSON: hostnames only.
fn is_domain(d: &str) -> bool {
    d.contains('.')
        && !d.starts_with(['.', '-'])
        && !d.ends_with(['.', '-'])
        && d.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

fn rules_json(extra: &str) -> String {
    let mut out = String::from("[");
    for d in DOMAINS.lines().chain(extra.lines()).map(|l| l.trim().to_ascii_lowercase()) {
        if !is_domain(&d) {
            continue;
        }
        let re = format!(r"^[^:]+://+([^:/]+\.)?{}[:/]", d.replace('.', r"\."));
        out += &format!(
            r#"{{"trigger":{{"url-filter":{},"load-type":["third-party"]}},"action":{{"type":"block"}}}},"#,
            quote(&re)
        );
    }
    for p in PATTERNS {
        out += &format!(r#"{{"trigger":{{"url-filter":{}}},"action":{{"type":"block"}}}},"#, quote(p));
    }
    out += &format!(
        r#"{{"trigger":{{"url-filter":".*"}},"action":{{"type":"css-display-none","selector":{}}}}}]"#,
        quote(HIDE)
    );
    out
}

unsafe extern "C" fn saved(
    store: *mut glib::gobject_ffi::GObject,
    res: *mut gio::ffi::GAsyncResult,
    _data: glib::ffi::gpointer,
) {
    let mut err: *mut glib::ffi::GError = ptr::null_mut();
    // SAFETY: GIO calls this once with the store we passed to `_save` and its result.
    let filter = unsafe { ffi::webkit_user_content_filter_store_save_finish(store as *mut _, res, &mut err) };
    if filter.is_null() {
        if err.is_null() {
            eprintln!("zolith: ad filter failed to compile");
        } else {
            // SAFETY: a non-null GError from `_finish` is owned by us.
            let e: glib::Error = unsafe { from_glib_full(err) };
            eprintln!("zolith: ad filter failed to compile: {e}");
        }
    } else {
        FILTER.set(filter);
    }
    DONE.set(true);
}

/// Compile the filter. Blocks (spinning the GTK main loop) until it's ready so the first
/// page already loads filtered. On failure the browser just runs unfiltered.
pub fn init(data_dir: &Path, extra_domains: &str) {
    let Ok(path) = CString::new(data_dir.join("filters").to_string_lossy().into_owned()) else { return };
    let bytes = glib::Bytes::from_owned(rules_json(extra_domains).into_bytes());
    unsafe {
        // The store is deliberately never freed: it lives as long as the process.
        let store = ffi::webkit_user_content_filter_store_new(path.as_ptr());
        ffi::webkit_user_content_filter_store_save(
            store,
            c"zolith".as_ptr(),
            bytes.to_glib_none().0,
            ptr::null_mut(),
            Some(saved),
            ptr::null_mut(),
        );
    }
    // Never hang startup on the compiler.
    glib::timeout_add_seconds_local_once(5, || DONE.set(true));
    let ctx = glib::MainContext::default();
    while !DONE.get() {
        ctx.iteration(true);
    }
}

pub fn is_ready() -> bool {
    !FILTER.get().is_null()
}

/// Turn network/cosmetic filtering on or off for one tab. Takes effect on its next requests.
pub fn apply(wv: &webkit2gtk::WebView, on: bool) {
    let Some(manager) = wv.user_content_manager() else { return };
    let filter = FILTER.get();
    unsafe {
        let m: *mut ffi::WebKitUserContentManager = manager.to_glib_none().0;
        ffi::webkit_user_content_manager_remove_all_filters(m);
        if on && !filter.is_null() {
            ffi::webkit_user_content_manager_add_filter(m, filter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_validated() {
        assert!(is_domain("ads.example.com"));
        for bad in ["", "# comment", "nodot", "a b.com", "evil\".com", "x.com/path", ".x.com", "x.com.", "(a|b).com"] {
            assert!(!is_domain(bad), "{bad}");
        }
    }

    #[test]
    fn rules_shape() {
        let json = rules_json("My-Ads.Example.org\nnot a domain\n\"],\n");
        assert!(json.starts_with("[{") && json.ends_with("}]"));
        assert!(json.contains(r#""url-filter":"^[^:]+://+([^:/]+\\.)?doubleclick\\.net[:/]""#));
        assert!(json.contains(r"my-ads\\.example\\.org"));
        assert!(!json.contains("not a domain"));
        // every rule is an object with exactly one trigger and one action
        assert_eq!(json.matches(r#"{"trigger":"#).count(), json.matches(r#""action":"#).count());
        // balanced quotes => the user line with a quote was not injected
        assert_eq!(json.replace(r#"\""#, "").matches('"').count() % 2, 0);
    }
}
