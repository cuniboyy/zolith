use std::{fs, path::PathBuf};

/// (name, search URL with `%s` for the query)
pub const ENGINES: [(&str, &str); 5] = [
    ("DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("Google", "https://www.google.com/search?q=%s"),
    ("Bing", "https://www.bing.com/search?q=%s"),
    ("Brave", "https://search.brave.com/search?q=%s"),
    ("Startpage", "https://www.startpage.com/sp/search?query=%s"),
];

/// Live suggestion endpoints (all answer in OpenSearch JSON). Startpage and custom engines have none.
pub fn suggest_template(engine: &str) -> Option<&'static str> {
    Some(match engine {
        "DuckDuckGo" => "https://duckduckgo.com/ac/?type=list&q=%s",
        "Google" => "https://suggestqueries.google.com/complete/search?client=firefox&q=%s",
        "Bing" => "https://api.bing.com/osjson.aspx?query=%s",
        "Brave" => "https://search.brave.com/api/suggest?q=%s",
        _ => return None,
    })
}
pub const CUSTOM: &str = "Custom";

/// Colour themes: (id, name).
pub const THEMES: [(&str, &str); 4] = [("default", "Zolith"), ("opera", "Opera red"), ("ocean", "Ocean"), ("violet", "Violet")];

pub const ZOOMS: [u32; 9] = [67, 75, 80, 90, 100, 110, 125, 150, 200];
pub const STARTUPS: [&str; 3] = ["newtab", "restore", "page"];

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub engine: String,
    /// Search URL used when `engine` is `CUSTOM`.
    pub custom: String,
    /// Home button target. Empty means the new tab page.
    pub home: String,
    pub show_home: bool,
    pub show_bookmarks: bool,
    pub adblock: bool,
    /// Also use the downloaded community filter lists (EasyList, EasyPrivacy).
    pub community: bool,
    /// Ask the search engine for suggestions while typing (sends what you type to it).
    pub suggest: bool,
    /// One of `STARTUPS`.
    pub startup: String,
    /// Opened at startup when `startup` is "page".
    pub startup_page: String,
    /// Default page zoom in percent, one of `ZOOMS`.
    pub zoom: u32,
    /// Empty means ~/Downloads.
    pub download_dir: String,
    /// One of `THEMES`.
    pub theme: String,
    /// The first-run setup guide has been completed.
    pub welcome_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: ENGINES[0].0.into(), custom: String::new(), home: String::new(),
            show_home: true, show_bookmarks: true, adblock: true, community: true, suggest: true,
            startup: STARTUPS[0].into(), startup_page: String::new(), zoom: 100, download_dir: String::new(),
            theme: THEMES[0].0.into(), welcome_done: false,
        }
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
    /// Apply one change. Values come from the settings page, so anything invalid is refused.
    pub fn set(&mut self, key: &str, v: &str) -> bool {
        let v = v.trim();
        let flag = |v: &str| match v { "true" => Some(true), "false" => Some(false), _ => None };
        match key {
            "engine" if v == CUSTOM || ENGINES.iter().any(|e| e.0 == v) => self.engine = v.into(),
            "custom" => self.custom = v.into(),
            "home" => self.home = v.into(),
            "startup" if STARTUPS.contains(&v) => self.startup = v.into(),
            "startup_page" => self.startup_page = v.into(),
            "zoom" => match v.parse() { Ok(z) if ZOOMS.contains(&z) => self.zoom = z, _ => return false },
            "download_dir" if v.is_empty() || v.starts_with('/') => self.download_dir = v.into(),
            "theme" if THEMES.iter().any(|t| t.0 == v) => self.theme = v.into(),
            "show_home" | "show_bookmarks" | "adblock" | "community" | "suggest" | "welcome_done" => {
                let Some(b) = flag(v) else { return false };
                match key {
                    "show_home" => self.show_home = b,
                    "show_bookmarks" => self.show_bookmarks = b,
                    "suggest" => self.suggest = b,
                    "community" => self.community = b,
                    "welcome_done" => self.welcome_done = b,
                    _ => self.adblock = b,
                }
            }
            _ => return false,
        }
        true
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        let (mut saw_startup, mut saw_welcome) = (false, false);
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            saw_startup |= k.trim() == "startup";
            saw_welcome |= k.trim() == "welcome_done";
            s.set(k.trim(), v);
        }
        // settings saved before the setup guide existed: these users are not new
        if !saw_welcome && text.lines().any(|l| l.contains('=')) {
            s.welcome_done = true;
        }
        // older versions opened the home page at startup
        if !saw_startup && !s.home.is_empty() {
            s.startup = "page".into();
            s.startup_page = s.home.clone();
        }
        s
    }

    pub fn serialize(&self) -> String {
        self.pairs().iter().map(|(k, v)| format!("{k}={}\n", v.replace(['\n', '\r'], " "))).collect()
    }

    fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("engine", self.engine.clone()), ("custom", self.custom.clone()), ("home", self.home.clone()),
            ("show_home", self.show_home.to_string()), ("show_bookmarks", self.show_bookmarks.to_string()),
            ("adblock", self.adblock.to_string()), ("community", self.community.to_string()), ("suggest", self.suggest.to_string()),
            ("startup", self.startup.clone()),
            ("startup_page", self.startup_page.clone()), ("zoom", self.zoom.to_string()),
            ("download_dir", self.download_dir.clone()), ("theme", self.theme.clone()),
            ("welcome_done", self.welcome_done.to_string()),
        ]
    }

    /// Settings as a JSON object for the settings page (all values as strings).
    pub fn to_json(&self) -> String {
        let body: Vec<String> = self.pairs().iter().map(|(k, v)| format!("{}:{}", quote(k), quote(v))).collect();
        format!("{{{}}}", body.join(","))
    }

    pub fn download_dir(&self) -> PathBuf {
        if self.download_dir.is_empty() { home_dir().join("Downloads") } else { PathBuf::from(&self.download_dir) }
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

/// True if `input` should be opened as an address rather than searched for.
pub fn is_address(input: &str) -> bool {
    let s = input.trim();
    let host = s.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("");
    s.contains("://") || s.starts_with("about:") || (!s.contains(' ') && (s.contains('.') || host == "localhost"))
}

/// Percent-encode text for use inside a URL query value.
pub fn encode_query(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        b' ' => "+".into(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// Turn what the user typed into a URL: pass URLs through, otherwise search with `template`.
pub fn to_url(input: &str, template: &str) -> String {
    let s = input.trim();
    let host = s.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("");
    let is_local = host == "localhost"
        || (host.contains('.') && host.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
    if !is_address(s) {
        template.replacen("%s", &encode_query(s), 1)
    } else if s.contains("://") || s.starts_with("about:") {
        s.to_string()
    } else {
        // local servers rarely speak https
        format!("{}://{s}", if is_local { "http" } else { "https" })
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
        assert!(is_address("example.com/a b") == false && is_address("github.com") && is_address("localhost:8080"));
        assert!(!is_address("how to cook rice") && !is_address("rust") && is_address("about:blank"));
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            engine: CUSTOM.into(),
            custom: "https://s.example/?a=b&q=%s".into(),
            home: "https://example.com/?x=1".into(),
            adblock: false,
            show_home: false,
            startup: "restore".into(),
            zoom: 125,
            download_dir: "/tmp/dl".into(),
            ..Default::default()
        };
        assert_eq!(Settings::parse(&s.serialize()), s);
        assert_eq!(s.search_template(), "https://s.example/?a=b&q=%s");
        // invalid values are ignored, not stored
        assert_eq!(Settings::parse("garbage\nengine=Nope\nzoom=7\nadblock=yes\nstartup=x\n"), Settings { welcome_done: true, ..Settings::default() });
        // old files: a home page used to be the startup page too
        let old = Settings::parse("home=https://a.example\n");
        assert_eq!((old.startup.as_str(), old.startup_page.as_str()), ("page", "https://a.example"));
        assert_eq!(Settings::parse("home=https://a.example\nstartup=newtab\n").startup, "newtab");
    }

    #[test]
    fn existing_users_skip_the_setup_guide() {
        assert!(!Settings::parse("").welcome_done);
        assert!(Settings::parse("engine=Google\n").welcome_done);
        assert!(!Settings::parse("engine=Google\nwelcome_done=false\n").welcome_done);
        assert!(Settings::parse("welcome_done=true\n").welcome_done);
        assert_eq!(Settings::parse("theme=opera\n").theme, "opera");
        assert_eq!(Settings::parse("theme=nope\n").theme, "default");
    }

    #[test]
    fn bad_engine_falls_back() {
        let mut s = Settings { engine: "Nope".into(), ..Default::default() };
        assert!(!s.clone().set("download_dir", "relative/path"));
        assert!(!s.clone().set("unknown", "x"));
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
