//! Turns community ad-block lists (EasyList syntax) into WebKit content-blocker rules.
//!
//! Only the well-understood core of the format is converted: domain and URL patterns, the
//! usual request-type and third-party options, exceptions, and generic element hiding.
//! Anything else is skipped rather than guessed at, so a list can't make Zolith block
//! more than its authors meant.

use crate::settings::quote;

/// Where the lists come from, and what they are called in Settings.
pub const LISTS: [(&str, &str); 2] = [
    ("EasyList", "https://easylist.to/easylist/easylist.txt"),
    ("EasyPrivacy", "https://easylist.to/easylist/easyprivacy.txt"),
];

/// WebKit refuses lists above this many rules.
const MAX_RULES: usize = 140_000;
/// Selectors per element-hiding rule.
const SELECTORS_PER_RULE: usize = 250;

#[derive(Default, Debug)]
pub struct Converted {
    /// Blocking rules (JSON objects).
    pub network: Vec<String>,
    /// Exceptions: must be applied after the rules they cancel.
    pub exceptions: Vec<String>,
    /// Element-hiding rules, kept apart so a bad selector can be dropped without losing the rest.
    pub hiding: Vec<String>,
    pub skipped: usize,
}

impl Converted {
    pub fn count(&self) -> usize {
        self.network.len() + self.exceptions.len() + self.hiding.len()
    }

    /// The rules as one JSON array, hiding rules left out unless `with_hiding`.
    pub fn json(&self, extra: &[String], with_hiding: bool) -> String {
        let mut all: Vec<&str> = extra.iter().map(String::as_str).collect();
        all.extend(self.network.iter().map(String::as_str));
        if with_hiding {
            all.extend(self.hiding.iter().map(String::as_str));
        }
        all.extend(self.exceptions.iter().map(String::as_str));
        all.truncate(MAX_RULES);
        format!("[{}]", all.join(","))
    }
}

/// A selector WebKit is sure to accept: tags, classes, ids, simple attribute tests, and
/// descendant/child combinators. No pseudo-classes or extended syntax.
pub fn simple_selector(s: &str) -> bool {
    if s.is_empty() || s.len() > 200 || s.starts_with(['>', '+', '~', ',']) || s.ends_with(['>', '+', '~', ',', ' ']) {
        return false;
    }
    let mut in_attr = false;
    let mut in_quote = false;
    for c in s.chars() {
        if in_quote {
            if c == '"' { in_quote = false }
            continue;
        }
        match c {
            '"' if in_attr => in_quote = true,
            '[' if !in_attr => in_attr = true,
            ']' if in_attr => in_attr = false,
            _ if in_attr => if !(c.is_ascii_alphanumeric() || "-_=^$*~|".contains(c)) { return false },
            c if c.is_ascii_alphanumeric() || "-_.# >+~".contains(c) => {}
            _ => return false,
        }
    }
    !in_attr && !in_quote
}

fn resource_type(opt: &str) -> Option<&'static str> {
    Some(match opt {
        "script" => "script",
        "image" => "image",
        "stylesheet" => "style-sheet",
        "xmlhttprequest" => "raw",
        "subdocument" => "document",
        "media" => "media",
        "font" => "font",
        "popup" => "popup",
        "ping" => "ping",
        _ => return None,
    })
}

/// A list pattern as a WebKit URL regex, or None if it needs features WebKit's lacks.
fn url_regex(pattern: &str) -> Option<String> {
    let (anchor, rest) = if let Some(r) = pattern.strip_prefix("||") {
        (r"^[^:]+://+([^:/]+\.)?", r)
    } else if let Some(r) = pattern.strip_prefix('|') {
        ("^", r)
    } else {
        ("", pattern)
    };
    let (rest, end) = match rest.strip_suffix('|') {
        Some(r) => (r, "$"),
        None => (rest, ""),
    };
    // too generic to be a safe rule on its own
    if rest.trim_matches(['*', '^']).len() < 3 {
        return None;
    }
    let mut out = String::from(anchor);
    for c in rest.chars() {
        match c {
            '*' => out.push_str(".*"),
            '^' => out.push_str("[/:?&=#]"),
            '|' | '$' | '\\' | ' ' => return None,
            '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' => { out.push('\\'); out.push(c) }
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            _ => return None,
        }
    }
    out.push_str(end);
    Some(out)
}

/// One network rule line. Returns (is_exception, json).
fn network_rule(line: &str) -> Option<(bool, String)> {
    let (exception, body) = match line.strip_prefix("@@") {
        Some(b) => (true, b),
        None => (false, line),
    };
    // /regex/ rules use syntax WebKit doesn't have
    if body.starts_with('/') && body.ends_with('/') && body.len() > 2 {
        return None;
    }
    let (pattern, opts) = match body.rsplit_once('$') {
        Some((p, o)) if !o.contains(['/', '*']) => (p, o),
        _ => (body, ""),
    };
    let re = url_regex(pattern)?;

    let (mut load, mut types) = (None, vec![]);
    let (mut if_domain, mut unless_domain, mut case) = (vec![], vec![], false);
    for opt in opts.split(',').filter(|o| !o.is_empty()) {
        match opt {
            "third-party" | "3p" => load = Some("third-party"),
            "~third-party" | "first-party" | "1p" => load = Some("first-party"),
            "match-case" => case = true,
            o if o.starts_with("domain=") => {
                for d in o["domain=".len()..].split('|') {
                    match d.strip_prefix('~') {
                        Some(n) => unless_domain.push(format!("*{n}")),
                        None => if_domain.push(format!("*{d}")),
                    }
                }
            }
            o => types.push(resource_type(o)?),
        }
    }
    // WebKit can't combine "only on" and "except on"
    if !if_domain.is_empty() && !unless_domain.is_empty() {
        return None;
    }
    let list = |v: &[String]| format!("[{}]", v.iter().map(|d| quote(d)).collect::<Vec<_>>().join(","));
    let mut trigger = format!(r#""url-filter":{}"#, quote(&re));
    if case {
        trigger += r#","url-filter-is-case-sensitive":true"#;
    }
    if let Some(l) = load {
        trigger += &format!(r#","load-type":["{l}"]"#);
    }
    if !types.is_empty() {
        trigger += &format!(r#","resource-type":[{}]"#, types.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(","));
    }
    if !if_domain.is_empty() {
        trigger += &format!(r#","if-domain":{}"#, list(&if_domain));
    }
    if !unless_domain.is_empty() {
        trigger += &format!(r#","unless-domain":{}"#, list(&unless_domain));
    }
    let action = if exception { "ignore-previous-rules" } else { "block" };
    Some((exception, format!(r#"{{"trigger":{{{trigger}}},"action":{{"type":"{action}"}}}}"#)))
}

pub fn convert(text: &str) -> Converted {
    let mut out = Converted::default();
    let mut selectors: Vec<String> = vec![];
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
            continue;
        }
        if let Some((domains, sel)) = line.split_once("##") {
            // only generic hiding (no site list), and only selectors we know are safe
            if domains.is_empty() && simple_selector(sel) {
                selectors.push(sel.to_string());
            } else {
                out.skipped += 1;
            }
            continue;
        }
        if line.contains("#@#") || line.contains("#?#") || line.contains("#$#") || line.contains("#%#") {
            out.skipped += 1;
            continue;
        }
        match network_rule(line) {
            Some((true, r)) => out.exceptions.push(r),
            Some((false, r)) => out.network.push(r),
            None => out.skipped += 1,
        }
    }
    selectors.sort();
    selectors.dedup();
    for chunk in selectors.chunks(SELECTORS_PER_RULE) {
        out.hiding.push(format!(
            r#"{{"trigger":{{"url-filter":".*"}},"action":{{"type":"css-display-none","selector":{}}}}}"#,
            quote(&chunk.join(", "))
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(line: &str) -> Option<String> {
        network_rule(line).map(|r| r.1)
    }

    #[test]
    fn domain_rules() {
        let r = one("||ads.example.com^").unwrap();
        assert!(r.contains(r#""url-filter":"^[^:]+://+([^:/]+\\.)?ads\\.example\\.com[/:?&=#]""#), "{r}");
        assert!(r.contains(r#""type":"block""#));
        let r = one("||example.com/banner/*$third-party,script,domain=a.com|b.org").unwrap();
        assert!(r.contains(r#""load-type":["third-party"]"#) && r.contains(r#""resource-type":["script"]"#));
        assert!(r.contains(r#""if-domain":["*a.com","*b.org"]"#), "{r}");
        assert!(r.contains(r"example\\.com/banner/.*"), "{r}");
        let r = one("@@||cdn.example.com/lib.js$script").unwrap();
        assert!(r.contains("ignore-previous-rules"));
        assert!(one("|https://x.example/a.js|").unwrap().contains(r#""^https://x\\.example/a\\.js$""#));
        assert!(one("-banner-ad-").unwrap().contains("-banner-ad-"));
    }

    #[test]
    fn unsafe_or_unsupported_rules_are_skipped() {
        for bad in [
            "/ads/", "/banner\\d+/", "||ex.com^$redirect=noop.js", "||ex.com^$csp=script-src", "||ex.com^$removeparam=x",
            "||ex.com^$domain=a.com|~b.com", "||ex.com^$~script", "||e", "a", "*", "^^", "||ex.com^|x", "||ex.com^$badfilter",
            "||ex.com^$important", "ad s", "||ex.com/ü", "||ex.com/a$b",
        ] {
            assert!(one(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn user_text_cannot_break_the_json() {
        let r = one(r#"||ex.com/a"b\c"#);
        assert!(r.is_none()); // backslash rejected outright
        let r = one(r#"||ex.com/a"b"#).unwrap();
        assert!(r.contains(r#"a\"b"#), "{r}");
        // every produced rule is a flat, balanced object
        for l in ["||a.example^$third-party", "@@||b.example^", "|http://c.example/x|"] {
            let r = one(l).unwrap();
            assert_eq!(r.matches('{').count(), r.matches('}').count());
            assert_eq!(r.replace(r#"\""#, "").matches('"').count() % 2, 0);
        }
    }

    #[test]
    fn selectors() {
        for ok in [".ad", "#banner", "div.ad-box", "a[href^=\"https://ads.example/\"]", ".a > .b", "div .c.d", "img[src*=\"/ad/\"]"] {
            assert!(simple_selector(ok), "{ok}");
        }
        for bad in ["", ">a", "a >", "a,", "div:has(.ad)", "a:not(.x)", "[data-x", "a[href=\"x]", ".a{color:red}", "a;b", "a\\b", "+js(x)", "div::before", "a b'"] {
            assert!(!simple_selector(bad), "{bad}");
        }
    }

    #[test]
    fn whole_list() {
        let list = "[Adblock Plus 2.0]\n! comment\n\n||ads.example.com^\n@@||ads.example.com/ok.js\n\
                    ##.ad-banner\n##div:has(.x)\nsite.com##.sidebar-ad\nsite.com#@#.ad\n##.ad-banner\n/regex/\n\
                    ||tracker.example^$third-party\n";
        let c = convert(list);
        assert_eq!(c.network.len(), 2);
        assert_eq!(c.exceptions.len(), 1);
        assert_eq!(c.hiding.len(), 1); // one generic selector (deduplicated); site-specific and :has skipped
        assert!(c.hiding[0].contains(r#""selector":".ad-banner""#));
        assert_eq!(c.skipped, 4);
        let json = c.json(&[], true);
        // exceptions come after the rules they cancel
        assert!(json.find("ignore-previous-rules").unwrap() > json.find("tracker").unwrap());
        assert!(!c.json(&[], false).contains("css-display-none"));
        assert!(json.starts_with("[{") && json.ends_with("}]"));
    }

    #[test]
    fn rule_budget_is_respected() {
        let list: String = (0..MAX_RULES + 50).map(|i| format!("||host{i}.example^\n")).collect();
        let json = convert(&list).json(&[], true);
        assert_eq!(json.matches(r#""type":"block""#).count(), MAX_RULES);
    }
}
