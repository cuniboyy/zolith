use std::{fs, path::PathBuf};

/// (name, search URL with `%s` for the query)
pub const ENGINES: [(&str, &str); 5] = [
    ("DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("Google", "https://www.google.com/search?q=%s"),
    ("Bing", "https://www.bing.com/search?q=%s"),
    ("Brave", "https://search.brave.com/search?q=%s"),
    ("Startpage", "https://www.startpage.com/sp/search?query=%s"),
];
pub const CUSTOM: &str = "Custom";

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub engine: String,
    /// Search URL used when `engine` is `CUSTOM`.
    pub custom: String,
    /// Empty means the built-in new tab page.
    pub home: String,
    pub adblock: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { engine: ENGINES[0].0.into(), custom: String::new(), home: String::new(), adblock: true }
    }
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn config_dir() -> PathBuf {
    home_dir().join(".config/zolith")
}

pub fn data_dir() -> PathBuf {
    home_dir().join(".local/share/zolith")
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let v = v.trim();
            match k.trim() {
                "engine" => s.engine = v.into(),
                "custom" => s.custom = v.into(),
                "home" => s.home = v.into(),
                "adblock" => s.adblock = v != "false",
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        let clean = |v: &str| v.replace(['\n', '\r'], " ");
        format!(
            "engine={}\ncustom={}\nhome={}\nadblock={}\n",
            clean(&self.engine), clean(&self.custom), clean(&self.home), self.adblock
        )
    }

    pub fn load() -> Self {
        Self::parse(&fs::read_to_string(config_dir().join("settings")).unwrap_or_default())
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(config_dir());
        if let Err(e) = fs::write(config_dir().join("settings"), self.serialize()) {
            eprintln!("zolith: could not save settings: {e}");
        }
    }

    /// Search URL template; falls back to the default engine if the choice is unusable.
    pub fn search_template(&self) -> &str {
        if self.engine == CUSTOM {
            let c = self.custom.trim();
            if (c.starts_with("https://") || c.starts_with("http://")) && c.contains("%s") {
                return c;
            }
        }
        ENGINES.iter().find(|e| e.0 == self.engine).unwrap_or(&ENGINES[0]).1
    }
}

/// Turn what the user typed into a URL: pass URLs through, otherwise search with `template`.
pub fn to_url(input: &str, template: &str) -> String {
    let s = input.trim();
    let host = s.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("");
    let is_local = host == "localhost"
        || (host.contains('.') && host.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
    if s.contains("://") || s.starts_with("about:") {
        s.to_string()
    } else if !s.contains(' ') && (s.contains('.') || host == "localhost") {
        // local servers rarely speak https
        format!("{}://{s}", if is_local { "http" } else { "https" })
    } else {
        let q: String = s.bytes().map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        }).collect();
        template.replacen("%s", &q, 1)
    }
}

/// A JSON / JavaScript string literal. `<` is escaped so it is safe inside a `<script>` tag.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out += "\\\\",
            '"' => out += "\\\"",
            '\n' => out += "\\n",
            '<' => out += "\\u003c",
            c if (c as u32) < 0x20 => out += &format!("\\u{:04x}", c as u32),
            c => out.push(c),
        }
    }
    out + "\""
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let t = ENGINES[0].1;
        assert_eq!(to_url("example.com", t), "https://example.com");
        assert_eq!(to_url("localhost:3000", t), "http://localhost:3000");
        assert_eq!(to_url("192.168.1.5:8080/x", t), "http://192.168.1.5:8080/x");
        assert_eq!(to_url("https://a.b/c d", t), "https://a.b/c d");
        assert_eq!(to_url("rust lifetimes", t), "https://duckduckgo.com/?q=rust+lifetimes");
        assert_eq!(to_url("what is 1.5?", t), "https://duckduckgo.com/?q=what+is+1.5%3F");
        assert_eq!(to_url("50% off", ENGINES[1].1), "https://www.google.com/search?q=50%25+off");
        // a query containing "%s" must not be substituted twice
        assert_eq!(to_url("printf %s", t), "https://duckduckgo.com/?q=printf+%25s");
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            engine: CUSTOM.into(),
            custom: "https://s.example/?a=b&q=%s".into(),
            home: "https://example.com/?x=1".into(),
            adblock: false,
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
        assert_eq!(s.search_template(), "https://s.example/?a=b&q=%s");
        assert_eq!(Settings::parse("garbage\nengine=Nope\n"), Settings { engine: "Nope".into(), ..Default::default() });
    }

    #[test]
    fn bad_engine_falls_back() {
        let mut s = Settings { engine: "Nope".into(), ..Default::default() };
        assert_eq!(s.search_template(), ENGINES[0].1);
        s.engine = CUSTOM.into();
        for bad in ["", "example.com/%s", "https://example.com/", "javascript:alert(%s)"] {
            s.custom = bad.into();
            assert_eq!(s.search_template(), ENGINES[0].1, "{bad}");
        }
        s.engine = "Google".into();
        assert_eq!(s.search_template(), ENGINES[1].1);
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"a"b\c</script>"#), [r#""a\"b\\c\"#, "u003c/script>\""].concat());
    }
}
