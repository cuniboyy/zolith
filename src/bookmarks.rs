//! Bookmarks as a file: the HTML format every browser exports (a "Netscape bookmark file"),
//! so bookmarks can move in and out of Zolith without a private format.

use std::{collections::HashSet, fs, path::Path};

/// Refuse absurdly large files rather than reading a video into memory.
const MAX_IMPORT_BYTES: u64 = 16 * 1024 * 1024;
/// Longer than any real bookmark URL; also keeps a broken file from bloating the store.
const MAX_URL_CHARS: usize = 4096;

/// Read a bookmark file, with size and type checks.
pub fn read_file(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("That is not a file".into());
    }
    if meta.len() > MAX_IMPORT_BYTES {
        return Err("That file is too large to be a bookmark file".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Every web bookmark in an HTML bookmark file, in file order. Folder structure is flattened,
/// which is all Zolith's bookmark list can show.
pub fn parse(html: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    let mut seen = HashSet::new();
    let mut i = 0;
    while let Some(rel) = html[i..].find('<') {
        let start = i + rel;
        let Some(end) = tag_end(html, start) else { break };
        let tag = &html[start + 1..end];
        if !tag.starts_with('/') && is_anchor(tag) {
            if let Some(href) = attr(tag, "href") {
                // addresses are HTML attributes too: `&amp;` in a query string means `&`
                let href = decode_entities(href.trim());
                let text_start = end + 1;
                // an entry that never closes is a truncated file: don't guess at it
                let Some(close) = find_ci(html, "</a", text_start) else { break };
                let (raw, next) = (&html[text_start..close], close);
                i = next;
                if is_web(&href) && seen.insert(href.to_ascii_lowercase()) {
                    let title = clean_text(raw);
                    out.push((href.clone(), if title.is_empty() { href } else { title }));
                }
                continue;
            }
        }
        i = end + 1;
    }
    out
}

/// Add imported bookmarks that are not already there; returns how many were added.
pub fn merge(existing: &mut Vec<(String, String)>, imported: Vec<(String, String)>) -> usize {
    let mut seen: HashSet<String> = existing.iter().map(|(u, _)| u.to_ascii_lowercase()).collect();
    let mut added = 0;
    for (url, title) in imported {
        if seen.insert(url.to_ascii_lowercase()) {
            existing.push((url, title));
            added += 1;
        }
    }
    added
}

/// A bookmark file another browser can import.
pub fn export(marks: &[(String, String)]) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n\
         <!-- This is an automatically generated file.\n     It will be read and overwritten.\n     DO NOT EDIT! -->\n\
         <META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n\
         <TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    for (url, title) in marks {
        out += &format!("    <DT><A HREF=\"{}\">{}</A>\n", escape(url), escape(title));
    }
    out += "</DL><p>\n";
    out
}

fn is_web(url: &str) -> bool {
    url.chars().count() <= MAX_URL_CHARS
        && (url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"))
            || url.get(..8).is_some_and(|p| p.eq_ignore_ascii_case("https://")))
}

fn escape(s: &str) -> String {
    // & first, or the entities from the later replacements would be escaped again
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The index of the `>` that closes the tag starting at `start`, ignoring quoted `>`s.
fn tag_end(s: &str, start: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut quote = 0u8;
    for (i, &c) in b.iter().enumerate().skip(start + 1) {
        match c {
            b'"' | b'\'' if quote == 0 => quote = c,
            c if quote != 0 && c == quote => quote = 0,
            b'>' if quote == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// True for `<a ...>` but not for `<abbr ...>` or a closing tag.
fn is_anchor(tag: &str) -> bool {
    let t = tag.trim_start();
    let mut chars = t.chars();
    chars.next().is_some_and(|c| c.eq_ignore_ascii_case(&'a'))
        && chars.next().is_none_or(|c| c.is_ascii_whitespace() || c == '/')
}

/// The value of one attribute in a tag, quoted or bare. Attribute names are case-insensitive.
fn attr(tag: &str, want: &str) -> Option<String> {
    let b = tag.as_bytes();
    let mut i = 1; // past '<'
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'/' && b[i] != b'>' {
        i += 1;
    }
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let name_start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' && b[i] != b'>' {
            i += 1;
        }
        if name_start == i {
            // a stray '=' or '/': skip it and keep looking
            i += 1;
            continue;
        }
        let name = &tag[name_start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            continue; // attribute without a value
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let value = if b[i] == b'"' || b[i] == b'\'' {
            let quote = b[i];
            i += 1;
            let start = i;
            while i < b.len() && b[i] != quote {
                i += 1;
            }
            let value = &tag[start..i];
            if i < b.len() {
                i += 1;
            }
            value
        } else {
            let start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                i += 1;
            }
            &tag[start..i]
        };
        if name.eq_ignore_ascii_case(want) {
            return Some(value.to_string());
        }
    }
    None
}

/// Case-insensitive search for an ASCII needle, byte-wise (safe on UTF-8 bounds because the
/// needles here are ASCII).
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || from >= h.len() || n.len() > h.len() {
        return None;
    }
    (from..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// The visible text of a title, with nested tags stripped and whitespace tidied.
fn clean_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        match rest[open..].find('>') {
            Some(close) => rest = &rest[open + close + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(end) = after.find(';').filter(|e| *e <= 12) else {
            out.push('&');
            rest = after;
            continue;
        };
        match entity(&after[..end]) {
            Some(c) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => return Some('&'),
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        "nbsp" => return Some(' '),
        "mdash" => return Some('—'),
        "ndash" => return Some('–'),
        "hellip" => return Some('…'),
        _ => {}
    }
    let number = name.strip_prefix('#')?;
    let code = match number.strip_prefix('x').or_else(|| number.strip_prefix('X')) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(code).filter(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<TITLE>Bookmarks</TITLE><H1>Bookmarks</H1>
<DL><p>
    <DT><A HREF="https://example.com/" ADD_DATE="1">Example</A>
    <DT><A HREF='https://a.example/x?a=1&amp;b=2' TITLE="x">A &amp; B</A>
    <DT><H3>Folder</H3>
    <DL><p>
        <DT><A HREF=https://nested.example/path>Nested <b>bold</b></A>
    </DL><p>
    <DT><A HREF="javascript:alert(1)">Nope</A>
    <DT><A HREF="https://example.com/">Duplicate</A>
</DL><p>
"#;

    #[test]
    fn parses_a_browser_export() {
        let marks = parse(SAMPLE);
        assert_eq!(marks, [
            ("https://example.com/".into(), "Example".into()),
            ("https://a.example/x?a=1&b=2".into(), "A & B".into()),
            ("https://nested.example/path".into(), "Nested bold".into()),
        ]);
    }

    #[test]
    fn entity_and_text_handling() {
        assert_eq!(clean_text("  caf&eacute; &amp; <i>tea</i>\n "), "caf&eacute; & tea");
        assert_eq!(clean_text("&#65;&#x42;&nbsp;C"), "AB C");
        assert_eq!(clean_text("a & b"), "a & b");
        assert_eq!(clean_text("&unknown;"), "&unknown;");
        assert!(parse("<A HREF=\"https://x.example/\">unterminated").is_empty());
        assert!(parse("").is_empty());
        assert!(parse("<a href='not-a-url'>x</a>").is_empty());
        assert!(parse(&format!("<A HREF=\"https://x.example/{}\">long</A>", "a".repeat(MAX_URL_CHARS))).is_empty());
    }

    #[test]
    fn export_round_trips() {
        let marks = vec![
            ("https://example.com/?a=1&b=2".to_string(), "A & B <c> \"quoted\"".to_string()),
            ("https://x.example/".to_string(), "X".to_string()),
        ];
        let html = export(&marks);
        assert!(html.starts_with("<!DOCTYPE NETSCAPE-Bookmark-file-1>"));
        assert!(html.contains(r#"HREF="https://example.com/?a=1&amp;b=2""#));
        assert_eq!(parse(&html), marks);
        assert!(export(&[]).ends_with("</DL><p>\n"));
    }

    #[test]
    fn merging_keeps_existing_order_and_skips_duplicates() {
        let mut marks = vec![("https://a.example/".into(), "A".into())];
        let added = merge(&mut marks, vec![
            ("https://A.EXAMPLE/".into(), "A again".into()),
            ("https://b.example/".into(), "B".into()),
        ]);
        assert_eq!(added, 1);
        assert_eq!(marks, [("https://a.example/".into(), "A".into()), ("https://b.example/".into(), "B".into())]);
    }
}
