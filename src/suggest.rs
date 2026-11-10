//! Address-bar suggestions: browsing history, ranking of history + bookmarks, and the
//! search engine's live suggestions.

use crate::settings;
use gtk::glib;
use soup::prelude::*;
use std::{fs, path::PathBuf, time::{SystemTime, UNIX_EPOCH}};

/// Oldest/least-used entries are dropped past this many.
const MAX_HISTORY: usize = 3000;

#[derive(Clone, Debug, PartialEq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub count: u32,
    /// Unix seconds of the latest visit.
    pub last: u64,
}

/// A row in the dropdown.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: Kind,
    /// Main line: page title or search words.
    pub text: String,
    /// Dim line: address shown next to it.
    pub detail: String,
    /// What opening this row loads.
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Go,
    Search,
    History,
    Bookmark,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn path() -> PathBuf {
    settings::config_dir().join("history.tsv")
}

/// A results page of one of the built-in search engines (e.g. `duckduckgo.com/?q=...`).
pub fn is_search_page(url: &str) -> bool {
    let (base, query) = url.split_once('?').unwrap_or((url, ""));
    !query.is_empty()
        && settings::ENGINES.iter().any(|e| e.1.split('?').next() == Some(base))
}

/// Pages worth remembering: real websites only, never our own pages or search results.
pub fn recordable(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && !is_search_page(url)
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

pub fn parse(text: &str) -> Vec<Visit> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.splitn(4, '\t');
            let (last, count, url, title) = (p.next()?, p.next()?, p.next()?, p.next().unwrap_or(""));
            let (count, last) = (count.parse().ok()?, last.parse().ok()?);
            recordable(url).then(|| Visit { url: url.into(), title: title.into(), count, last })
        })
        .collect()
}

pub fn serialize(v: &[Visit]) -> String {
    v.iter().map(|e| format!("{}\t{}\t{}\t{}\n", e.last, e.count, clean(&e.url), clean(&e.title))).collect()
}

pub fn load() -> Vec<Visit> {
    parse(&fs::read_to_string(path()).unwrap_or_default())
}

pub fn save(v: &[Visit]) {
    let _ = fs::create_dir_all(settings::config_dir());
    if let Err(e) = fs::write(path(), serialize(v)) {
        eprintln!("zolith: could not save history: {e}");
    }
}

/// Count a visit to `url` (fragment ignored), keeping the list bounded.
pub fn record(v: &mut Vec<Visit>, url: &str, title: &str, now: u64) {
    if !recordable(url) {
        return;
    }
    let url = url.split('#').next().unwrap_or(url);
    match v.iter_mut().find(|e| e.url == url) {
        Some(e) => {
            e.count += 1;
            e.last = now;
            if !title.is_empty() {
                e.title = title.into();
            }
        }
        None => v.push(Visit { url: url.into(), title: title.into(), count: 1, last: now }),
    }
    // over the limit: drop the least valuable page, never the one just visited
    while v.len() > MAX_HISTORY {
        let worst = v.iter().enumerate().filter(|(_, e)| e.url != url).min_by_key(|(_, e)| score(e, now)).map(|(i, _)| i);
        match worst {
            Some(i) => { v.swap_remove(i); }
            None => break,
        }
    }
}

/// Update the stored title of the page at `url` once it is known.
pub fn set_title(v: &mut [Visit], url: &str, title: &str) {
    let url = url.split('#').next().unwrap_or(url);
    if let Some(e) = v.iter_mut().find(|e| e.url == url) {
        e.title = title.into();
    }
}

/// Forget visits whose latest visit is within the last `secs` seconds (0 = everything).
pub fn clear(v: &mut Vec<Visit>, secs: u64, now: u64) {
    if secs == 0 {
        v.clear();
    } else {
        v.retain(|e| e.last < now.saturating_sub(secs));
    }
}

/// How much a page deserves to be suggested: frequent and recent beats old and rare.
fn score(e: &Visit, now: u64) -> i64 {
    let days = now.saturating_sub(e.last) / 86_400;
    (((e.count as f64 + 1.0).ln() * 10.0) + 20.0 / (1.0 + days as f64)) as i64
}

fn bare(url: &str) -> &str {
    let u = url.split_once("://").map_or(url, |x| x.1);
    u.strip_prefix("www.").unwrap_or(u)
}

/// History and bookmarks matching what was typed, best first. Every word typed must appear
/// in the page's title or address.
pub fn find(history: &[Visit], marks: &[(String, String)], typed: &str, now: u64, limit: usize) -> Vec<Row> {
    let terms: Vec<String> = typed.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return vec![];
    }
    let mut cands: Vec<(i64, Row)> = vec![];
    let mut consider = |url: &str, title: &str, bookmark: bool, base: i64| {
        let hay = format!("{} {}", title.to_lowercase(), url.to_lowercase());
        if !terms.iter().all(|t| hay.contains(t.as_str())) {
            return;
        }
        let mut score = base + if bookmark { 25 } else { 0 };
        // typing the start of the site's name is the strongest hint
        if bare(url).to_lowercase().starts_with(&terms[0]) {
            score += 30;
        }
        let row = Row {
            kind: if bookmark { Kind::Bookmark } else { Kind::History },
            text: if title.is_empty() { url.to_string() } else { title.to_string() },
            detail: bare(url).trim_end_matches('/').to_string(),
            url: url.to_string(),
        };
        match cands.iter_mut().find(|(_, r)| r.url == url) {
            Some((s, r)) => {
                // the same page as a bookmark and in history: show once, as a bookmark
                if bookmark {
                    r.kind = Kind::Bookmark;
                }
                *s = (*s).max(score);
            }
            None => cands.push((score, row)),
        }
    };
    for e in history {
        consider(&e.url, &e.title, false, score(e, now));
    }
    for (url, title) in marks {
        consider(url, title, true, 10);
    }
    cands.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
    cands.into_iter().take(limit).map(|(_, r)| r).collect()
}

/// The site to complete in the address bar: the best-ranked bookmark or visited page whose
/// address starts with what was typed. Returns the full text to show (`youtube.com/`) and the
/// row for it. Words with spaces never complete.
pub fn best_site(history: &[Visit], marks: &[(String, String)], typed: &str, now: u64) -> Option<(String, Row)> {
    let t = typed.to_lowercase();
    if t.is_empty() || t.contains(char::is_whitespace) || t.starts_with(['/', '.']) || t.ends_with('/') {
        return None;
    }
    let t = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(&t).to_string();
    let t = t.strip_prefix("www.").unwrap_or(&t).to_string();
    let all = history.iter().map(|e| (e.url.as_str(), e.title.as_str(), score(e, now)))
        .chain(marks.iter().map(|(u, ti)| (u.as_str(), ti.as_str(), 35)));
    let (url, _, _) = all.filter(|(u, _, _)| bare(u).to_lowercase().starts_with(&t)).max_by_key(|x| x.2)?;
    let host = bare(url).split('/').next().unwrap_or("");
    let completion = if t.contains('/') {
        // already typing a path: complete that page's address
        bare(url).to_string()
    } else {
        format!("{host}/")
    };
    if completion.len() <= t.len() || !completion.to_lowercase().starts_with(&t) {
        return None;
    }
    // keep the site's real address (it may need its "www.")
    let origin: String = url.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    let target = if t.contains('/') { url.to_string() } else { format!("{origin}/") };
    // the site's own title, if we have its front page
    let front = |u: &str| u.trim_end_matches('/') == origin;
    let title = history.iter().map(|e| (e.url.as_str(), e.title.as_str()))
        .chain(marks.iter().map(|(u, ti)| (u.as_str(), ti.as_str())))
        .find(|(u, ti)| front(u) && !ti.is_empty()).map(|(_, ti)| ti.to_string());
    let bookmarked = marks.iter().any(|(u, _)| *u == target);
    let row = Row {
        kind: if bookmarked { Kind::Bookmark } else { Kind::History },
        text: if t.contains('/') { title.unwrap_or_else(|| bare(url).to_string()) } else { title.unwrap_or_else(|| host.to_string()) },
        detail: bare(&target).trim_end_matches('/').to_string(),
        url: target,
    };
    Some((completion, row))
}

/// Read an OpenSearch suggestion reply, `["typed",["one","two"],...]`, into its suggestions:
/// the strings of the top-level array's second element. Anything else is ignored.
pub fn parse_suggestions(body: &str) -> Vec<String> {
    let b = body.as_bytes();
    // `depth` counts open [ and {; `elem` is which top-level element we're in
    let (mut i, mut depth, mut elem, mut out) = (0usize, 0i32, 0usize, vec![]);
    while i < b.len() {
        match b[i] {
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
            }
            b',' if depth == 1 => elem += 1,
            b'"' => {
                let (text, next) = read_string(b, i + 1);
                i = next;
                if depth == 2 && elem == 1 && !text.is_empty() && out.len() < 20 {
                    out.push(text);
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Read a JSON string body starting after its opening quote; returns the text and the index after the closing quote.
fn read_string(b: &[u8], mut i: usize) -> (String, usize) {
    let mut bytes: Vec<u8> = vec![];
    while i < b.len() {
        match b[i] {
            b'"' => return (String::from_utf8_lossy(&bytes).into_owned(), i + 1),
            b'\\' if i + 1 < b.len() => {
                i += 1;
                match b[i] {
                    b'n' | b'r' | b't' => bytes.push(b' '),
                    b'u' if i + 4 < b.len() => {
                        let code = std::str::from_utf8(&b[i + 1..i + 5]).ok().and_then(|h| u32::from_str_radix(h, 16).ok());
                        let mut c = code.and_then(char::from_u32);
                        // a UTF-16 surrogate pair: 😀
                        if let (Some(hi @ 0xD800..=0xDBFF), true) = (code, b.get(i + 5..i + 7) == Some(b"\\u")) {
                            let lo = std::str::from_utf8(b.get(i + 7..i + 11).unwrap_or(b"")).ok().and_then(|h| u32::from_str_radix(h, 16).ok());
                            if let Some(lo @ 0xDC00..=0xDFFF) = lo {
                                c = char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                                i += 6;
                            }
                        }
                        let mut buf = [0u8; 4];
                        bytes.extend_from_slice(c.unwrap_or('\u{fffd}').encode_utf8(&mut buf).as_bytes());
                        i += 4;
                    }
                    other => bytes.push(other),
                }
            }
            c => bytes.push(c),
        }
        i += 1;
    }
    (String::from_utf8_lossy(&bytes).into_owned(), i)
}

/// One shared HTTP client for suggestion requests; gives up after a few seconds.
pub fn new_session() -> soup::Session {
    let s = soup::Session::new();
    s.set_timeout(4);
    s
}

/// Ask the search engine for suggestions for `typed`. Calls `done` on the main loop with the
/// result (empty on any failure); nothing is sent anywhere unless this is called.
pub fn fetch(session: &soup::Session, engine: &str, typed: &str, done: impl FnOnce(Vec<String>) + 'static) {
    let Some(template) = settings::suggest_template(engine) else { return done(vec![]) };
    let url = template.replacen("%s", &settings::encode_query(typed), 1);
    let Ok(msg) = soup::Message::new("GET", &url) else { return done(vec![]) };
    let fut = session.send_and_read_future(&msg, glib::Priority::DEFAULT);
    glib::MainContext::default().spawn_local(async move {
        let out = match fut.await {
            Ok(bytes) if bytes.len() < 64 * 1024 => parse_suggestions(&String::from_utf8_lossy(&bytes)),
            _ => vec![],
        };
        done(out);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn v(url: &str, title: &str, count: u32, last: u64) -> Visit {
        Visit { url: url.into(), title: title.into(), count, last }
    }

    #[test]
    fn parses_opensearch() {
        assert_eq!(parse_suggestions(r#"["rust",["rust lang","rust book","rust \"quoted\""]]"#),
                   ["rust lang", "rust book", r#"rust "quoted""#]);
        assert_eq!(parse_suggestions(r#"["x",[],[],{"google:suggesttype":["QUERY"]}]"#), Vec::<String>::new());
        // extra arrays after the suggestions (Google/Brave add metadata) are ignored
        assert_eq!(parse_suggestions(r#"["q",["a","b"],["","" ],{"k":["z"]}]"#), ["a", "b"]);
        assert_eq!(parse_suggestions(r#"["q",["café","😀 ok","tab\there"]]"#), ["café", "😀 ok", "tab here"]);
        for junk in ["", "not json", "<html>", "[", r#"["q",["unterminated"#, "]]]"] {
            let _ = parse_suggestions(junk); // must not panic
        }
    }

    #[test]
    fn history_round_trip_and_hygiene() {
        let h = vec![v("https://a.example/x", "A page", 3, 100), v("https://b.example/", "", 1, 200)];
        assert_eq!(parse(&serialize(&h)), h);
        // tabs/newlines in titles cannot corrupt the file; non-web pages are never loaded
        let bad = vec![v("https://c.example/", "evil\ttitle\nline", 1, 5)];
        assert_eq!(parse(&serialize(&bad))[0].title, "evil title line");
        assert!(parse("1\t1\tzolith://settings/\tx\nfile:///etc/passwd\n9\t9\n").is_empty());
        let mut h = vec![];
        record(&mut h, "zolith://settings/", "S", 1);
        record(&mut h, "https://a.example/p#top", "", 10);
        record(&mut h, "https://a.example/p", "Title", 20);
        assert_eq!(h, [v("https://a.example/p", "Title", 2, 20)]);
        set_title(&mut h, "https://a.example/p#x", "New");
        assert_eq!(h[0].title, "New");
    }

    #[test]
    fn history_is_bounded_keeping_the_best() {
        let mut h = vec![];
        for i in 0..MAX_HISTORY + 50 {
            record(&mut h, &format!("https://site{i}.example/"), "", 1000);
        }
        record(&mut h, "https://popular.example/", "", 1000);
        for _ in 0..30 { record(&mut h, "https://popular.example/", "", 1000); }
        assert!(h.len() <= MAX_HISTORY);
        assert!(h.iter().any(|e| e.url == "https://popular.example/"));
    }

    #[test]
    fn clearing_by_range() {
        let now = 10 * DAY;
        let mut h = vec![v("https://old.example/", "", 1, now - 5 * DAY), v("https://new.example/", "", 1, now - 60)];
        clear(&mut h, 3600, now);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].url, "https://old.example/");
        clear(&mut h, 0, now);
        assert!(h.is_empty());
    }

    #[test]
    fn search_pages_are_not_sites() {
        assert!(is_search_page("https://duckduckgo.com/?q=youtube&ia=web"));
        assert!(is_search_page("https://www.google.com/search?q=x"));
        assert!(!is_search_page("https://duckduckgo.com/"));
        assert!(!is_search_page("https://duckduckgo.com.evil.example/?q=x"));
        assert!(!is_search_page("https://example.com/search?q=x"));
        assert!(!recordable("https://duckduckgo.com/?q=youtube"));
        // already-saved search pages disappear when history is loaded
        assert!(parse("5\t2\thttps://duckduckgo.com/?q=a\tA\n").is_empty());
    }

    #[test]
    fn completes_sites() {
        let now = 100 * DAY;
        let hist = vec![
            v("https://www.youtube.com/watch?v=1", "A video - YouTube", 40, now - 60),
            v("https://www.youtube.com/", "YouTube", 30, now - 60),
            v("https://yourbank.example/login", "Bank", 2, now - 10 * DAY),
            v("http://localhost:3000/app", "Dev app", 5, now - 5),
        ];
        let marks = vec![("https://docs.rs/".to_string(), "Docs.rs".to_string())];
        let (c, row) = best_site(&hist, &marks, "you", now).unwrap();
        assert_eq!(c, "youtube.com/");
        assert_eq!((row.url.as_str(), row.text.as_str(), row.detail.as_str()), ("https://www.youtube.com/", "YouTube", "youtube.com"));
        assert_eq!(best_site(&hist, &marks, "YOU", now).unwrap().0, "youtube.com/");
        assert_eq!(best_site(&hist, &marks, "www.you", now).unwrap().0, "youtube.com/");
        assert_eq!(best_site(&hist, &marks, "https://you", now).unwrap().0, "youtube.com/");
        assert_eq!(best_site(&hist, &marks, "youtube.com/wa", now).unwrap().0, "youtube.com/watch?v=1");
        assert_eq!(best_site(&hist, &marks, "yourb", now).unwrap().0, "yourbank.example/");
        assert_eq!(best_site(&hist, &marks, "local", now).unwrap().0, "localhost:3000/");
        assert_eq!(best_site(&hist, &marks, "docs", now).unwrap().1.kind, Kind::Bookmark);
        // nothing to add, no match, spaces, junk
        for t in ["youtube.com/", "zzz", "you tube", "", " ", "/you", ".com", "tube"] {
            assert!(best_site(&hist, &marks, t, now).is_none(), "{t:?}");
        }
    }

    #[test]
    fn finds_and_ranks() {
        let now = 100 * DAY;
        let hist = vec![
            v("https://www.youtube.com/", "YouTube", 50, now - 60),
            v("https://news.example/you-wont-believe", "You won't believe this", 1, now - 80 * DAY),
            v("https://docs.rs/", "Docs.rs", 9, now - DAY),
        ];
        let marks = vec![("https://www.youtube.com/".to_string(), "YouTube".to_string()),
                         ("https://github.com/rust-lang".to_string(), "Rust lang on GitHub".to_string())];
        let r = find(&hist, &marks, "you", now, 5);
        assert_eq!(r[0].url, "https://www.youtube.com/");
        assert_eq!(r[0].kind, Kind::Bookmark); // in both: shown once, as a bookmark
        assert_eq!(r.iter().filter(|x| x.url.contains("youtube")).count(), 1);
        assert_eq!(r[1].url, "https://news.example/you-wont-believe");
        // every word must match, in title or address, in any order
        assert_eq!(find(&hist, &marks, "lang rust", now, 5)[0].url, "https://github.com/rust-lang");
        assert!(find(&hist, &marks, "you docs", now, 5).is_empty());
        assert!(find(&hist, &marks, "   ", now, 5).is_empty());
        assert_eq!(find(&hist, &marks, "e", now, 1).len(), 1);
        assert_eq!(r[0].detail, "youtube.com");
    }
}
