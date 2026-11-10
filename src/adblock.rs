//! Ad blocking: a WebKit content filter (network + cosmetic rules) compiled in the
//! background, plus a YouTube script (`SCRIPT`) the caller injects into each tab.
//!
//! The filter is always built from the curated list in `blocklist.txt`; the converted
//! community lists (see `lists.rs`) are added on top when enabled. Twitch stream ads are
//! stitched into the video server-side and cannot be blocked this way.

use crate::filterlists::Converted;
use crate::settings::quote;
use gtk::{gio, glib, glib::translate::*};
use std::{
    cell::{Cell, RefCell},
    ffi::CString,
    path::Path,
    ptr,
};
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
    /// True while WebKit is compiling a rule list. New pages keep the old filter until then.
    static COMPILING: Cell<bool> = const { Cell::new(false) };
    static AFTER: RefCell<Option<Box<dyn FnOnce(bool)>>> = const { RefCell::new(None) };
}

// The user's blocklist is untrusted text going into a regex inside JSON: hostnames only.
fn is_domain(d: &str) -> bool {
    d.contains('.')
        && !d.starts_with(['.', '-'])
        && !d.ends_with(['.', '-'])
        && d.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// The curated rules, in application order: blocking, then cosmetic.
fn curated_rules(extra: &str) -> Vec<String> {
    let mut out = vec![];
    for d in DOMAINS.lines().chain(extra.lines()).map(|l| l.trim().to_ascii_lowercase()) {
        if !is_domain(&d) {
            continue;
        }
        let re = format!(r"^[^:]+://+([^:/]+\.)?{}[:/]", d.replace('.', r"\."));
        out.push(format!(
            r#"{{"trigger":{{"url-filter":{},"load-type":["third-party"]}},"action":{{"type":"block"}}}}"#,
            quote(&re)
        ));
    }
    for p in PATTERNS {
        out.push(format!(r#"{{"trigger":{{"url-filter":{}}},"action":{{"type":"block"}}}}"#, quote(p)));
    }
    out.push(format!(
        r#"{{"trigger":{{"url-filter":".*"}},"action":{{"type":"css-display-none","selector":{}}}}}"#,
        quote(HIDE)
    ));
    out
}

/// The complete content-blocker rule list: curated rules first, then the community lists
/// (whose exceptions must come after their own block rules).
pub fn rules_json(extra: &str, community: Option<&Converted>) -> String {
    let curated = curated_rules(extra);
    match community {
        Some(c) => c.json(&curated, true),
        None => format!("[{}]", curated.join(",")),
    }
}

unsafe extern "C" fn saved(
    store: *mut glib::gobject_ffi::GObject,
    res: *mut gio::ffi::GAsyncResult,
    _data: glib::ffi::gpointer,
) {
    let mut err: *mut glib::ffi::GError = ptr::null_mut();
    // SAFETY: GIO calls this once with the store we passed to `_save` and its result.
    let filter = unsafe { ffi::webkit_user_content_filter_store_save_finish(store as *mut _, res, &mut err) };
    let ok = if filter.is_null() {
        if err.is_null() {
            eprintln!("zolith: ad filter failed to compile");
        } else {
            // SAFETY: a non-null GError from `_finish` is owned by us.
            let e: glib::Error = unsafe { from_glib_full(err) };
            eprintln!("zolith: ad filter failed to compile: {e}");
        }
        // keep whatever filter was working before
        false
    } else {
        FILTER.set(filter);
        true
    };
    COMPILING.set(false);
    let after = AFTER.with(|a| a.borrow_mut().take());
    if let Some(done) = after {
        done(ok);
    }
}

/// Compile a rule list. `done` runs on the main loop with whether WebKit accepted it; until
/// then the previous filter (if any) stays in use. Never blocks startup.
pub fn compile(data_dir: &Path, rules: String, done: impl FnOnce(bool) + 'static) {
    let Ok(path) = CString::new(data_dir.join("filters").to_string_lossy().into_owned()) else {
        done(false);
        return;
    };
    let bytes = glib::Bytes::from_owned(rules.into_bytes());
    COMPILING.set(true);
    AFTER.with(|a| *a.borrow_mut() = Some(Box::new(done)));
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
}

pub fn is_ready() -> bool {
    !FILTER.get().is_null()
}

pub fn compiling() -> bool {
    COMPILING.get()
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
    use crate::filterlists;

    #[test]
    fn domains_are_validated() {
        assert!(is_domain("ads.example.com"));
        for bad in ["", "# comment", "nodot", "a b.com", "evil\".com", "x.com/path", ".x.com", "x.com.", "(a|b).com"] {
            assert!(!is_domain(bad), "{bad}");
        }
    }

    #[test]
    fn rules_shape() {
        let json = rules_json("My-Ads.Example.org\nnot a domain\n\"],\n", None);
        assert!(json.starts_with("[{") && json.ends_with("}]"));
        assert!(json.contains(r#""url-filter":"^[^:]+://+([^:/]+\\.)?doubleclick\\.net[:/]""#));
        assert!(json.contains(r"my-ads\\.example\\.org"));
        assert!(!json.contains("not a domain"));
        // every rule is an object with exactly one trigger and one action
        assert_eq!(json.matches(r#"{"trigger":"#).count(), json.matches(r#""action":"#).count());
        // balanced quotes => the user line with a quote was not injected
        assert_eq!(json.replace(r#"\""#, "").matches('"').count() % 2, 0);
    }

    #[test]
    fn community_rules_come_between_curated_blocks_and_exceptions() {
        let c = filterlists::convert("||ads.example^\n@@||ads.example/ok.js\n##.ad-box\n");
        let json = rules_json("", Some(&c));
        let curated = json.find("doubleclick").unwrap();
        let community = json.find("ads\\\\.example").unwrap();
        let hiding = json.rfind("css-display-none").unwrap();
        let exception = json.find("ignore-previous-rules").unwrap();
        assert!(curated < community && community < hiding && hiding < exception, "{json}");
        assert_eq!(json.matches("ignore-previous-rules").count(), 1);
    }

    #[test]
    fn unescaped_community_selector_cannot_break_the_json() {
        // conversion only lets simple selectors through, and those are quoted again here
        let c = filterlists::convert("##div.ad[data-x=\"y\"]\n");
        let json = rules_json("", Some(&c));
        assert_eq!(json.replace(r#"\""#, "").matches('"').count() % 2, 0);
    }
}
