mod adblock;
mod downloads;
mod menus;
mod settings;
mod suggest;

use gtk::{cairo, gdk, gdk_pixbuf, gio, glib, glib::Propagation, pango, prelude::*};
use settings::{is_address, quote, to_url, Settings};
use suggest::{Kind, Row, Visit};
use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    platform::unix::WindowExtUnix,
    window::WindowBuilder,
};
use webkit2gtk::{
    FindControllerExt, FindOptions, HitTestResultExt, LoadEvent, NavigationPolicyDecision,
    NavigationPolicyDecisionExt, NavigationType, PolicyDecisionExt, PolicyDecisionType, PrintOperation,
    PrintOperationExt, ResponsePolicyDecision, ResponsePolicyDecisionExt, URIRequestExt, WebContextExt,
    WebInspectorExt, WebViewExt, WebsiteDataManagerExtManual, WebsiteDataTypes,
};
use wry::{NewWindowResponse, WebContext, WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

const CSS: &str = include_str!("style.css");
const NEWTAB: &str = include_str!("newtab.html");
const SETTINGS: &str = include_str!("settings.html");
const HISTORY_PAGE: &str = include_str!("history.html");
/// The settings page is loaded under this address; only it may send IPC messages.
const SETTINGS_URI: &str = "zolith://settings/";
const HISTORY_URI: &str = "zolith://history/";

/// Tab width limits, like Chrome: tabs share the strip evenly between these.
const TAB_MAX: i32 = 240;
const TAB_MIN: i32 = 72;
/// Below this width, inactive tabs hide their close button.
const TAB_CLOSE_MIN: i32 = 110;
/// Room kept for the new-tab button and the drag area beside it.
const PLUS_W: i32 = 40;
/// Pinned tabs show just their icon.
const PIN_W: i32 = 40;
const DRAG_W: i32 = 48;

// shortcut: no history page, private mode or tab dragging.

#[derive(Clone, Copy)]
enum Cmd {
    NewTab, CloseTab, Reopen, Reload, Back, Forward, Home, FocusUrl, Escape,
    Find, FindNext(bool), FindTyped, CloseFind, Zoom(i8), Print, Devtools, Bookmark,
    ToggleBookmarksBar, NextTab(i32), NthTab(usize), SuggestMove(i32), EnterUrl, Settings, History, Quit,
}

struct Tab {
    id: u32,
    page: gtk::Box,
    head: gtk::EventBox,
    label: gtk::Label,
    icon: gtk::Image,
    spinner: gtk::Spinner,
    close: gtk::Button,
    sep: gtk::Separator,
    pinned: bool,
    /// Owns the webview: dropping it destroys the widget.
    _view: wry::WebView,
    wv: webkit2gtk::WebView,
    /// Shown in the URL bar; empty for the new tab page.
    url: String,
    title: String,
    loading: bool,
}

struct App {
    next_id: u32,
    cur: u32,
    tabs: Vec<Tab>,
    closed: Vec<String>,
    marks: Vec<(String, String)>,
    settings: Settings,
    downloads_hooked: bool,
    fullscreen: bool,
    layout_pending: bool,
    win: gtk::ApplicationWindow,
    toolbar: gtk::Box,
    stack: gtk::Stack,
    tabs_scroll: gtk::ScrolledWindow,
    tabs_box: gtk::Box,
    strip: gtk::Box,
    controls: gtk::Box,
    entry: gtk::Entry,
    back: gtk::Button,
    fwd: gtk::Button,
    home_btn: gtk::Button,
    reload_img: gtk::Image,
    star_img: gtk::Image,
    bm_bar: gtk::Box,
    find_bar: gtk::Box,
    find_entry: gtk::Entry,
    status: gtk::Label,
    ctx: Rc<RefCell<WebContext>>,
    history: Vec<Visit>,
    /// Address bar dropdown: a panel drawn over the page (a popover would swallow editing keys).
    sug_pop: gtk::Box,
    /// The overlay the dropdown is placed in, for positioning it under the address bar.
    top: gtk::Overlay,
    sug_list: gtk::ListBox,
    sug_rows: Vec<Row>,
    /// Bumped on every keystroke so slow suggestion replies for old text are ignored.
    sug_seq: u32,
    /// What the user had typed before the last keystroke (without any completion we added).
    last_typed: String,
    /// Whether the current keystroke may inline-complete (not after deleting).
    inline_ok: bool,
    http: soup::Session,
    dls: Vec<downloads::Dl>,
    next_dl: u32,
    dl_pending: bool,
    dl_btn: gtk::Button,
    dl_list: gtk::Box,
}

type Sh = Rc<RefCell<App>>;

// Handlers fire from inside webkit/gtk callbacks; skip instead of panicking on re-entry.
// To keep that skip from ever dropping an update, nothing below calls into a webview
// (load, focus, navigate) while the App is borrowed: clone the handle out first.
fn with(app: &Sh, f: impl FnOnce(&mut App)) {
    if let Ok(mut a) = app.try_borrow_mut() {
        f(&mut a);
    }
}

fn cur_wv(app: &Sh) -> Option<webkit2gtk::WebView> {
    app.try_borrow().ok().and_then(|a| a.cur_tab().map(|t| t.wv.clone()))
}

fn run_js(wv: &webkit2gtk::WebView, js: &str) {
    wv.evaluate_javascript(js, None, None, None::<&gio::Cancellable>, |_| {});
}

/// Pages Zolith draws itself. Only they may talk to the browser.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Internal {
    Settings,
    History,
}

impl Internal {
    fn uri(self) -> &'static str {
        match self { Internal::Settings => SETTINGS_URI, Internal::History => HISTORY_URI }
    }
    fn shown(self) -> &'static str {
        match self { Internal::Settings => "zolith://settings", Internal::History => "zolith://history" }
    }
    fn icon(self) -> &'static str {
        match self { Internal::Settings => "emblem-system-symbolic", Internal::History => "document-open-recent-symbolic" }
    }
}

/// Which internal page an address is (several spellings work), if any.
fn internal(u: &str) -> Option<Internal> {
    let u = u.trim().to_ascii_lowercase();
    [
        ("zolith://settings", Internal::Settings), ("about:settings", Internal::Settings), ("chrome://settings", Internal::Settings),
        ("zolith://history", Internal::History), ("about:history", Internal::History), ("chrome://history", Internal::History),
    ]
    .into_iter()
    .find(|(p, _)| u.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '#', '?'])))
    .map(|(_, page)| page)
}

fn newtab_html(s: &Settings) -> String {
    NEWTAB.replace("__SEARCH__", &quote(s.search_template()))
}

fn settings_html(s: &Settings) -> String {
    let engines: Vec<String> = settings::ENGINES.iter().map(|e| e.0).chain([settings::CUSTOM]).map(quote).collect();
    let zooms: Vec<String> = settings::ZOOMS.iter().map(|z| z.to_string()).collect();
    SETTINGS
        .replace("__STATE__", &s.to_json())
        .replace("__ENGINES__", &format!("[{}]", engines.join(",")))
        .replace("__ZOOMS__", &format!("[{}]", zooms.join(",")))
        .replace("__ADBLOCK_READY__", if adblock::is_ready() { "true" } else { "false" })
        .replace("__VERSION__", env!("CARGO_PKG_VERSION"))
}

fn history_html(history: &[Visit]) -> String {
    let mut v: Vec<&Visit> = history.iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.last));
    let items: Vec<String> = v.iter().map(|e| format!("[{},{},{}]", quote(&e.url), quote(&e.title), e.last)).collect();
    HISTORY_PAGE.replace("__HISTORY__", &format!("[{}]", items.join(",")))
}

/// Draw an internal page into a tab from the current settings and history.
fn load_internal(app: &Sh, wv: &webkit2gtk::WebView, page: Internal) {
    let Ok((s, h)) = app.try_borrow().map(|a| (a.settings.clone(), if page == Internal::History { a.history.clone() } else { vec![] }))
        else { return };
    let html = match page { Internal::Settings => settings_html(&s), Internal::History => history_html(&h) };
    wv.load_html(&html, Some(page.uri()));
}

fn marks_path() -> PathBuf {
    settings::config_dir().join("bookmarks.tsv")
}

fn load_marks() -> Vec<(String, String)> {
    // before the rename to Zolith, bookmarks lived under browser-rs
    let old = settings::config_dir().with_file_name("browser-rs").join("bookmarks.tsv");
    fs::read_to_string(marks_path()).or_else(|_| fs::read_to_string(old)).unwrap_or_default().lines()
        .filter_map(|l| l.split_once('\t').map(|(u, t)| (u.into(), t.into()))).collect()
}

fn save_marks(m: &[(String, String)]) {
    let _ = fs::create_dir_all(settings::config_dir());
    let text: String = m.iter().map(|(u, t)| format!("{u}\t{}\n", t.replace(['\t', '\n'], " "))).collect();
    if let Err(e) = fs::write(marks_path(), text) {
        eprintln!("zolith: could not save bookmarks: {e}");
    }
}

fn session_path() -> PathBuf {
    settings::config_dir().join("session")
}

/// Saved tabs: (index of the selected tab, addresses with "" for the new tab page).
fn load_session() -> (usize, Vec<String>) {
    let text = fs::read_to_string(session_path()).unwrap_or_default();
    let mut lines = text.lines();
    let cur = lines.next().and_then(|l| l.parse().ok()).unwrap_or(0);
    (cur, lines.map(str::to_string).collect())
}

/// A free file name in `dir`, adding " (1)", " (2)"... like other browsers.
fn unique_path(dir: &Path, name: &OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let n = Path::new(name);
    let stem = n.file_stem().unwrap_or(name).to_string_lossy();
    let ext = n.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists()).expect("some name is free")
}

impl App {
    fn cur_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.cur)
    }

    fn show_url(&self) {
        let Some(t) = self.cur_tab() else { return };
        let u = &t.url;
        let shown: &str = match internal(u) {
            Some(p) => p.shown(),
            None => u.strip_prefix("https://").unwrap_or(u),
        };
        self.entry.set_text(shown);
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_visible(!text.is_empty());
    }

    /// Show/hide the optional bars to match settings and fullscreen.
    fn sync_bars(&self) {
        self.toolbar.set_visible(!self.fullscreen);
        self.bm_bar.set_visible(!self.fullscreen && self.settings.show_bookmarks && !self.marks.is_empty());
        self.home_btn.set_visible(self.settings.show_home);
    }

    fn set_chrome(&mut self, show: bool) {
        self.fullscreen = !show;
        self.sync_bars();
        if !show {
            self.find_bar.hide();
            self.set_status("");
        }
    }

    fn save_session(&self) {
        let cur = self.tabs.iter().position(|t| t.id == self.cur).unwrap_or(0);
        let mut text = format!("{cur}\n");
        for t in &self.tabs {
            if t.pinned {
                text.push('*');
            }
            text += &t.url.replace(['\n', '\r'], "");
            text.push('\n');
        }
        let _ = fs::create_dir_all(settings::config_dir());
        let _ = fs::write(session_path(), text);
    }

    /// Sync the toolbar with the current tab.
    fn refresh(&self) {
        let Some(t) = self.cur_tab() else { return };
        if !self.entry.has_focus() {
            self.show_url();
        }
        let icon = if t.url.starts_with("https://") { Some("channel-secure-symbolic") }
            else if t.url.starts_with("http://") { Some("channel-insecure-symbolic") }
            else if t.url.is_empty() { Some("system-search-symbolic") }
            else if let Some(p) = internal(&t.url) { Some(p.icon()) } else { None };
        self.entry.set_icon_from_icon_name(gtk::EntryIconPosition::Primary, icon);
        let p = t.wv.estimated_load_progress();
        self.entry.set_progress_fraction(if t.loading && p < 1.0 { p.max(0.1) } else { 0.0 });
        self.back.set_sensitive(t.wv.can_go_back());
        self.fwd.set_sensitive(t.wv.can_go_forward());
        self.reload_img.set_from_icon_name(
            Some(if t.loading { "process-stop-symbolic" } else { "view-refresh-symbolic" }), gtk::IconSize::Menu);
        let marked = self.marks.iter().any(|m| m.0 == t.url);
        self.star_img.set_from_icon_name(
            Some(if marked { "starred-symbolic" } else { "non-starred-symbolic" }), gtk::IconSize::Menu);
        self.win.set_title(&if t.title.is_empty() { "Zolith".to_string() } else { format!("{} — Zolith", t.title) });
    }

    fn select(&mut self, id: u32) {
        if !self.tabs.iter().any(|t| t.id == id) {
            return;
        }
        self.cur = id;
        self.stack.set_visible_child_name(&id.to_string());
        for t in &self.tabs {
            let ctx = t.head.style_context();
            if t.id == id { ctx.add_class("selected") } else { ctx.remove_class("selected") }
        }
        self.sug_pop.hide();
        self.show_url();
        self.set_status("");
        self.refresh();
        self.layout_tabs();
        self.save_session();
    }

    /// Size tabs like Chrome: equal widths, as wide as fits up to TAB_MAX, never below TAB_MIN
    /// (the strip scrolls instead).
    fn layout_tabs(&self) {
        let pinned = self.tabs.iter().filter(|t| t.pinned).count() as i32;
        let n = (self.tabs.len() as i32 - pinned).max(1);
        // measured from the whole strip: the scroller itself is sized from the tabs
        let avail = self.strip.allocated_width() - self.controls.allocated_width() - PLUS_W - DRAG_W - pinned * PIN_W;
        let w = (avail / n).clamp(TAB_MIN, TAB_MAX);
        let sel = self.tabs.iter().position(|t| t.id == self.cur);
        for (i, t) in self.tabs.iter().enumerate() {
            let want = if t.pinned { PIN_W } else { w };
            if t.head.width_request() != want {
                t.head.set_size_request(want, -1);
            }
            let selected = Some(i) == sel;
            t.label.set_visible(!t.pinned);
            t.close.set_visible(!t.pinned && (selected || w >= TAB_CLOSE_MIN));
            // separators sit between tabs, never touching the selected one
            t.sep.set_visible(!selected && Some(i + 1) != sel && i + 1 < self.tabs.len());
        }
        self.scroll_to_selected();
    }

    /// Move a tab to position `to`, staying inside its group (pinned tabs come first).
    fn move_tab(&mut self, id: u32, to: usize) {
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        let pinned_n = self.tabs.iter().filter(|t| t.pinned).count();
        let (lo, hi) = if self.tabs[from].pinned { (0, pinned_n.saturating_sub(1)) } else { (pinned_n, self.tabs.len() - 1) };
        let to = to.clamp(lo, hi);
        if to == from {
            return;
        }
        let t = self.tabs.remove(from);
        self.tabs_box.reorder_child(&t.head, to as i32);
        self.tabs.insert(to, t);
        self.layout_tabs();
    }

    fn scroll_to_selected(&self) {
        let Some(i) = self.tabs.iter().position(|t| t.id == self.cur) else { return };
        let w = self.tabs[i].head.width_request().max(PIN_W) as f64;
        let adj = self.tabs_scroll.hadjustment();
        let x: f64 = self.tabs[..i].iter().map(|t| t.head.width_request().max(PIN_W) as f64).sum();
        if x < adj.value() {
            adj.set_value(x);
        } else if x + w > adj.value() + adj.page_size() {
            adj.set_value(x + w - adj.page_size());
        }
    }

    /// A tab's page changed (address, load state or progress).
    fn tab_changed(&mut self, id: u32, loading: Option<bool>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        let old_url = t.url.clone();
        if let Some(u) = t.wv.uri() {
            t.url = if u.starts_with("about:") { String::new() } else { u.to_string() };
        }
        if let Some(l) = loading {
            t.loading = l;
            t.spinner.set_visible(l);
            t.icon.set_visible(!l);
            if l { t.spinner.start() } else { t.spinner.stop() }
        }
        let url_changed = t.url != old_url;
        if url_changed {
            set_favicon(t);
        }
        let visited = (loading == Some(false)).then(|| (t.url.clone(), t.title.clone()));
        if id == self.cur {
            self.refresh();
        }
        if url_changed {
            self.save_session();
        }
        if let Some((url, title)) = visited.filter(|(u, _)| suggest::recordable(u)) {
            suggest::record(&mut self.history, &url, &title, suggest::now());
            suggest::save(&self.history);
        }
    }

    fn set_title(&mut self, id: u32, title: &str) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        t.title = title.to_string();
        let shown = if title.is_empty() { if t.url.is_empty() { "New Tab" } else { &t.url } } else { title };
        t.label.set_text(shown);
        t.head.set_tooltip_text(Some(shown));
        let (url, titled) = (t.url.clone(), !title.is_empty());
        if id == self.cur {
            self.refresh();
        }
        if titled && suggest::recordable(&url) {
            suggest::set_title(&mut self.history, &url, title);
            suggest::save(&self.history);
        }
    }
}

/// Show the site's icon in its tab, or a generic one.
fn set_favicon(t: &Tab) {
    let pixbuf = t.wv.favicon().and_then(|s| {
        let img = cairo::ImageSurface::try_from(s.clone()).ok()?;
        gdk::pixbuf_get_from_surface(&s, 0, 0, img.width(), img.height())
    });
    // shortcut: scaled to 16px, so icons are slightly soft on HiDPI screens
    match pixbuf.and_then(|p| p.scale_simple(16, 16, gdk_pixbuf::InterpType::Bilinear)) {
        Some(p) => t.icon.set_from_pixbuf(Some(&p)),
        None => {
            let name = internal(&t.url).map_or("web-browser-symbolic", Internal::icon);
            t.icon.set_from_icon_name(Some(name), gtk::IconSize::Menu);
        }
    }
}

/// Re-run the tab layout once GTK is idle (sizes are only known after allocation).
fn schedule_layout(app: &Sh) {
    let Ok(mut a) = app.try_borrow_mut() else { return };
    if a.layout_pending {
        return;
    }
    a.layout_pending = true;
    let app = app.clone();
    glib::idle_add_local(move || match app.try_borrow_mut() {
        Ok(mut a) => {
            a.layout_pending = false;
            a.layout_tabs();
            glib::ControlFlow::Break
        }
        Err(_) => glib::ControlFlow::Continue,
    });
}

/// Give keyboard focus to the page, or to the URL bar on the new tab page.
fn focus_current(app: &Sh) {
    let Ok(a) = app.try_borrow() else { return };
    let Some(t) = a.cur_tab() else { return };
    let (wv, entry, blank) = (t.wv.clone(), a.entry.clone(), t.url.is_empty());
    drop(a);
    if blank { entry.grab_focus() } else { wv.grab_focus() }
}

fn select_tab(app: &Sh, id: u32) {
    with(app, |a| a.select(id));
    focus_current(app);
}

fn load_home(app: &Sh, wv: &webkit2gtk::WebView) {
    let Ok(s) = app.try_borrow().map(|a| a.settings.clone()) else { return };
    if s.home.trim().is_empty() {
        wv.load_html(&newtab_html(&s), None);
    } else if let Some(p) = internal(&s.home) {
        load_internal(app, wv, p);
    } else {
        wv.load_uri(&to_url(&s.home, s.search_template()));
    }
}

fn find(app: &Sh, wv: &webkit2gtk::WebView, c: Cmd) {
    let Ok(q) = app.try_borrow().map(|a| a.find_entry.text().to_string()) else { return };
    let Some(fc) = wv.find_controller() else { return };
    if q.is_empty() {
        fc.search_finish();
    } else if matches!(c, Cmd::FindNext(_)) && fc.search_text().as_deref() == Some(q.as_str()) {
        if matches!(c, Cmd::FindNext(true)) { fc.search_previous() } else { fc.search_next() }
    } else {
        fc.search(&q, (FindOptions::CASE_INSENSITIVE | FindOptions::WRAP_AROUND).bits(), u32::MAX);
    }
}

/// Open an internal page, reusing its tab if it is already open.
fn open_internal(app: &Sh, page: Internal) {
    let existing = app.try_borrow().ok().and_then(|a| a.tabs.iter().find(|t| internal(&t.url) == Some(page)).map(|t| t.id));
    match existing {
        Some(id) => {
            // a fresh look: history changes while you browse
            if page == Internal::History {
                if let Some(wv) = app.try_borrow().ok().and_then(|a| a.tabs.iter().find(|t| t.id == id).map(|t| t.wv.clone())) {
                    load_internal(app, &wv, page);
                }
            }
            select_tab(app, id)
        }
        None => { new_tab(app, Some(page.uri().into()), true); }
    }
}

fn run(app: &Sh, c: Cmd) {
    match c {
        Cmd::NewTab => { new_tab(app, None, true); }
        Cmd::Reopen => {
            let u = app.try_borrow_mut().ok().and_then(|mut a| a.closed.pop());
            if let Some(u) = u { new_tab(app, Some(u), true); }
        }
        Cmd::CloseTab => {
            let id = app.try_borrow().map(|a| a.cur);
            if let Ok(id) = id { close_tab(app, id) }
        }
        Cmd::Quit => std::process::exit(0),
        Cmd::Settings => open_internal(app, Internal::Settings),
        Cmd::History => open_internal(app, Internal::History),
        Cmd::SuggestMove(d) => suggest_move(app, d),
        Cmd::EnterUrl => {
            let typed = app.try_borrow().map(|a| a.entry.text().to_string());
            if let Ok(t) = typed { suggest_enter(app, &t) }
        }
        Cmd::Bookmark => toggle_bookmark(app),
        Cmd::ToggleBookmarksBar => {
            let v = app.try_borrow().map(|a| !a.settings.show_bookmarks);
            if let Ok(v) = v { apply_setting(app, "show_bookmarks", &v.to_string()) }
        }
        Cmd::FocusUrl => with(app, |a| { a.entry.grab_focus(); a.entry.select_region(0, -1) }),
        Cmd::Find => with(app, |a| {
            if !a.fullscreen {
                a.find_bar.show();
                a.find_entry.grab_focus();
                a.find_entry.select_region(0, -1);
            }
        }),
        Cmd::NextTab(d) => {
            let id = app.try_borrow().ok().and_then(|a| {
                let i = a.tabs.iter().position(|t| t.id == a.cur)? as i32;
                Some(a.tabs[(i + d).rem_euclid(a.tabs.len() as i32) as usize].id)
            });
            if let Some(id) = id { select_tab(app, id) }
        }
        Cmd::NthTab(n) => {
            // like Firefox and Chrome: Ctrl+9 is always the last tab
            let id = app.try_borrow().ok().and_then(|a| {
                if n >= 8 { a.tabs.last() } else { a.tabs.get(n) }.map(|t| t.id)
            });
            if let Some(id) = id { select_tab(app, id) }
        }
        _ => {
            let Some(wv) = cur_wv(app) else { return };
            match c {
                Cmd::Back => wv.go_back(),
                Cmd::Forward => wv.go_forward(),
                Cmd::Reload if wv.is_loading() => wv.stop_loading(),
                Cmd::Reload => {
                    // pages loaded from a string have nothing to re-fetch: rebuild them
                    let url = app.try_borrow().ok().and_then(|a| a.cur_tab().map(|t| t.url.clone()));
                    match url.as_deref() {
                        Some("") => load_home_newtab(app, &wv),
                        Some(u) => match internal(u) { Some(p) => load_internal(app, &wv, p), None => wv.reload() },
                        None => wv.reload(),
                    }
                }
                Cmd::Home => load_home(app, &wv),
                Cmd::Zoom(d) => {
                    let default = app.try_borrow().map(|a| a.settings.zoom as f64 / 100.0).unwrap_or(1.0);
                    wv.set_zoom_level(if d == 0 { default } else {
                        ((wv.zoom_level() * 10.0).round() + d as f64).clamp(3.0, 30.0) / 10.0
                    });
                }
                Cmd::Print => {
                    let win = app.try_borrow().ok().map(|a| a.win.clone());
                    PrintOperation::new(&wv).run_dialog(win.as_ref());
                }
                Cmd::Devtools => if let Some(i) = wv.inspector() { i.show() },
                Cmd::FindNext(_) | Cmd::FindTyped => find(app, &wv, c),
                Cmd::CloseFind => {
                    with(app, |a| a.find_bar.hide());
                    if let Some(fc) = wv.find_controller() { fc.search_finish() }
                    wv.grab_focus();
                }
                Cmd::Escape => {
                    // first Escape closes the suggestions, the next leaves the address bar
                    let open = app.try_borrow().map(|a| a.sug_pop.is_visible()).unwrap_or(false);
                    if open {
                        with(app, |a| a.sug_pop.hide());
                    } else {
                        with(app, |a| a.show_url());
                        focus_current(app);
                    }
                }
                _ => {}
            }
        }
    }
}

fn load_home_newtab(app: &Sh, wv: &webkit2gtk::WebView) {
    let Ok(s) = app.try_borrow().map(|a| a.settings.clone()) else { return };
    wv.load_html(&newtab_html(&s), None);
}

fn go(app: &Sh, input: &str) {
    if input.trim().is_empty() { return }
    with(app, |a| { a.sug_seq += 1; a.sug_pop.hide() });
    let Some(wv) = cur_wv(app) else { return };
    if let Some(p) = internal(input) {
        load_internal(app, &wv, p);
        return;
    }
    let Ok(template) = app.try_borrow().map(|a| a.settings.search_template().to_string()) else { return };
    wv.load_uri(&to_url(input, &template));
    wv.grab_focus();
}

fn kind_icon(k: Kind) -> &'static str {
    match k {
        Kind::Go => "web-browser-symbolic",
        Kind::Search => "system-search-symbolic",
        Kind::History => "document-open-recent-symbolic",
        Kind::Bookmark => "starred-symbolic",
    }
}

fn hide_suggestions(app: &Sh) {
    with(app, |a| { a.sug_seq += 1; a.sug_pop.hide() });
}

/// The user typed in the address bar: show matching bookmarks/history now, and ask the
/// search engine for more shortly after (if the user allows it).
fn suggest_typed(app: &Sh, typed: &str) {
    let text = typed.trim().to_string();
    if text.is_empty() {
        with(app, |a| a.last_typed.clear());
        return hide_suggestions(app);
    }
    let Ok(mut a) = app.try_borrow_mut() else { return };
    a.sug_seq += 1;
    // deleting (Backspace/Delete, or removing the completion) must never re-complete
    let deleted = a.last_typed.len() >= typed.len() && a.last_typed.starts_with(typed);
    a.inline_ok = !deleted;
    a.last_typed = typed.to_string();
    let (seq, engine, ask) = (a.sug_seq, a.settings.engine.clone(), a.settings.suggest && !is_address(&text));
    drop(a);
    suggest_render(app, &text, &[], true);
    if ask && settings::suggest_template(&engine).is_some() {
        let app = app.clone();
        // wait for a pause in typing so one query goes out per word, not per key
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            let Ok(a) = app.try_borrow() else { return };
            if a.sug_seq != seq { return }
            let http = a.http.clone();
            drop(a);
            let app2 = app.clone();
            let text2 = text.clone();
            suggest::fetch(&http, &engine, &text, move |list| {
                let current = app2.try_borrow().map(|a| a.sug_seq == seq && a.entry.has_focus()).unwrap_or(false);
                if current && !list.is_empty() { suggest_render(&app2, &text2, &list, false) }
            });
        });
    }
}

/// Build and show the dropdown: the typed text first, then bookmarks/history, then the
/// search engine's suggestions.
fn suggest_render(app: &Sh, text: &str, online: &[String], complete: bool) {
    let Ok(mut a) = app.try_borrow_mut() else { return };
    if !a.entry.has_focus() {
        return;
    }
    let template = a.settings.search_template().to_string();
    let typed_row = if is_address(text) {
        Row { kind: Kind::Go, text: text.into(), detail: String::new(), url: text.into() }
    } else {
        Row {
            kind: Kind::Search, text: text.into(), detail: format!("Search with {}", a.settings.engine),
            url: to_url(text, &template),
        }
    };
    // a site you visit often: complete it in the bar and put it first, so Enter goes straight there
    let site = if a.inline_ok { suggest::best_site(&a.history, &a.marks, text, suggest::now()) } else { None };
    let mut rows = vec![];
    match site {
        Some((completion, row)) => {
            if complete {
                // text is the user's own prefix; show the rest selected so typing on replaces it.
                // Done once this keystroke has finished: GTK resets the selection right after
                // the "changed" signal returns, which would also garble fast typing.
                let n = text.chars().count() as i32;
                let shown = format!("{text}{}", completion.chars().skip(n as usize).collect::<String>());
                let (seq, typed_now, app2) = (a.sug_seq, text.to_string(), app.clone());
                glib::idle_add_local_once(move || {
                    // holding the borrow also keeps our own "changed" handler from re-entering
                    let Ok(a) = app2.try_borrow_mut() else { return };
                    if a.sug_seq == seq && a.entry.has_focus() && a.entry.text().as_str() == typed_now {
                        a.entry.set_text(&shown);
                        a.entry.select_region(n, -1);
                    }
                });
            }
            rows.push(row);
            rows.push(typed_row);
        }
        None => rows.push(typed_row),
    }
    for r in suggest::find(&a.history, &a.marks, text, suggest::now(), 5) {
        if !rows.iter().any(|x| x.url == r.url) { rows.push(r) }
    }
    let typed = text.to_lowercase();
    for s in online.iter().filter(|s| s.to_lowercase() != typed).take(5) {
        rows.push(Row { kind: Kind::Search, text: s.clone(), detail: String::new(), url: to_url(s, &template) });
    }
    for c in a.sug_list.children() { a.sug_list.remove(&c) }
    for r in &rows {
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        bx.pack_start(&gtk::Image::from_icon_name(Some(kind_icon(r.kind)), gtk::IconSize::Menu), false, false, 0);
        let main = gtk::Label::new(Some(&r.text));
        main.set_xalign(0.0);
        main.set_ellipsize(pango::EllipsizeMode::End);
        bx.pack_start(&main, false, true, 0);
        if !r.detail.is_empty() {
            let d = gtk::Label::new(Some(&r.detail));
            d.set_xalign(0.0);
            d.set_ellipsize(pango::EllipsizeMode::End);
            d.style_context().add_class("detail");
            bx.pack_start(&d, true, true, 0);
        }
        let row = gtk::ListBoxRow::new();
        row.set_can_focus(false);
        row.add(&bx);
        a.sug_list.add(&row);
    }
    a.sug_list.show_all();
    a.sug_rows = rows;
    // sit right under the address bar, as wide as it is
    let (x, y) = a.entry.translate_coordinates(&a.top, 0, 0).unwrap_or((0, 0));
    a.sug_pop.set_margin_start(x.max(0));
    a.sug_pop.set_margin_top((y + a.entry.allocated_height() + 2).max(0));
    a.sug_list.set_size_request(a.entry.allocated_width(), -1);
    a.sug_list.select_row(a.sug_list.row_at_index(0).as_ref());
    a.sug_pop.show();
}

/// Up/Down in the address bar move through the dropdown.
fn suggest_move(app: &Sh, d: i32) {
    let Ok(a) = app.try_borrow() else { return };
    let n = a.sug_rows.len() as i32;
    if n == 0 { return }
    let at = a.sug_list.selected_row().map_or(-1, |r| r.index());
    let to = (at + d).rem_euclid(n);
    a.sug_list.select_row(a.sug_list.row_at_index(to).as_ref());
}

/// Enter in the address bar: open the highlighted suggestion, or what was typed.
fn suggest_enter(app: &Sh, typed: &str) {
    let pick = app.try_borrow().ok().and_then(|a| {
        if !a.sug_pop.is_visible() { return None }
        a.sug_list.selected_row().and_then(|r| a.sug_rows.get(r.index() as usize).map(|x| x.url.clone()))
    });
    go(app, &pick.unwrap_or_else(|| typed.to_string()));
}

/// Change one setting, save it, and update everything that depends on it.
fn apply_setting(app: &Sh, key: &str, value: &str) {
    let (views, adblock, zoom) = {
        let Ok(mut a) = app.try_borrow_mut() else { return };
        let old = a.settings.clone();
        if !a.settings.set(key, value) {
            return;
        }
        a.settings.save();
        a.sync_bars();
        let views: Vec<_> = a.tabs.iter().map(|t| t.wv.clone()).collect();
        let adblock = (old.adblock != a.settings.adblock).then_some(a.settings.adblock);
        let zoom = (old.zoom != a.settings.zoom).then_some((old.zoom as f64 / 100.0, a.settings.zoom as f64 / 100.0));
        (views, adblock, zoom)
    };
    for wv in &views {
        if let Some(on) = adblock {
            adblock::apply(wv, on);
        }
        // tabs the user zoomed by hand keep their zoom
        if let Some((old, new)) = zoom {
            if (wv.zoom_level() - old).abs() < 0.001 {
                wv.set_zoom_level(new);
            }
        }
    }
}

/// Messages from our own pages. Settings: "set\nkey\nvalue", "clear\nkinds\nrange",
/// "pick-download-dir". History: "history-delete\nurl", "history-clear".
fn handle_ipc(app: &Sh, id: u32, msg: &str) {
    // re-check who sent it now that we're on the main loop, and let each page only do its own jobs
    let Some((wv, page)) = app.try_borrow().ok().and_then(|a| {
        a.tabs.iter().find(|t| t.id == id).and_then(|t| internal(&t.url).map(|p| (t.wv.clone(), p)))
    }) else { return };
    let mut parts = msg.splitn(3, '\n');
    match (page, parts.next(), parts.next(), parts.next()) {
        // the download folder only changes through the folder picker below
        (Internal::Settings, Some("set"), Some(k), Some(v)) if k != "download_dir" => apply_setting(app, k, v),
        (Internal::Settings, Some("clear"), Some(kinds), Some(range)) => {
            let (history, rest): (Vec<&str>, Vec<&str>) = kinds.split(',').partition(|k| *k == "history");
            if !history.is_empty() {
                with(app, |a| {
                    suggest::clear(&mut a.history, range_secs(range), suggest::now());
                    suggest::save(&a.history);
                });
            }
            if rest.is_empty() {
                run_js(&wv, "window.zolithCleared && zolithCleared(true)");
            } else {
                clear_data(&wv, &rest.join(","), range);
            }
        }
        (Internal::Settings, Some("pick-download-dir"), _, _) => pick_download_dir(app, &wv),
        (Internal::History, Some("history-delete"), Some(url), _) => with(app, |a| {
            suggest::delete(&mut a.history, url);
            suggest::save(&a.history);
        }),
        (Internal::History, Some("history-clear"), _, _) => with(app, |a| {
            suggest::clear(&mut a.history, 0, suggest::now());
            suggest::save(&a.history);
        }),
        _ => {}
    }
}

/// Length of a "time range" choice in seconds; 0 means all time.
fn range_secs(range: &str) -> u64 {
    match range { "hour" => 3600, "day" => 86_400, "week" => 604_800, "month" => 2_419_200, _ => 0 }
}

fn clear_data(wv: &webkit2gtk::WebView, kinds: &str, range: &str) {
    let Some(manager) = wv.website_data_manager() else { return };
    let mut types = WebsiteDataTypes::empty();
    for k in kinds.split(',') {
        types |= match k {
            "cookies" => WebsiteDataTypes::COOKIES | WebsiteDataTypes::LOCAL_STORAGE
                | WebsiteDataTypes::SESSION_STORAGE | WebsiteDataTypes::INDEXEDDB_DATABASES
                | WebsiteDataTypes::WEBSQL_DATABASES | WebsiteDataTypes::DOM_CACHE
                | WebsiteDataTypes::SERVICE_WORKER_REGISTRATIONS | WebsiteDataTypes::OFFLINE_APPLICATION_CACHE,
            "cache" => WebsiteDataTypes::DISK_CACHE | WebsiteDataTypes::MEMORY_CACHE,
            _ => WebsiteDataTypes::empty(),
        };
    }
    if types.is_empty() {
        return;
    }
    let secs = range_secs(range) as i64;
    // the callback must be Send; it still runs on this (the main) thread
    let page = glib::thread_guard::ThreadGuard::new(wv.clone());
    manager.clear(types, glib::TimeSpan::from_seconds(secs), None::<&gio::Cancellable>, move |r| {
        if let Err(e) = &r {
            eprintln!("zolith: clearing data failed: {e}");
        }
        run_js(page.get_ref(), &format!("window.zolithCleared && zolithCleared({})", r.is_ok()));
    });
}

fn pick_download_dir(app: &Sh, wv: &webkit2gtk::WebView) {
    let Ok((win, current)) = app.try_borrow().map(|a| (a.win.clone(), a.settings.download_dir())) else { return };
    let dialog = gtk::FileChooserNative::new(
        Some("Choose download folder"), Some(&win), gtk::FileChooserAction::SelectFolder, Some("Select"), Some("Cancel"));
    let _ = dialog.set_current_folder(&current);
    if dialog.run() != gtk::ResponseType::Accept {
        return;
    }
    let Some(dir) = dialog.filename().and_then(|p| p.to_str().map(str::to_string)) else { return };
    apply_setting(app, "download_dir", &dir);
    run_js(wv, &format!("window.zolithDownloadDir && zolithDownloadDir({})", quote(&dir)));
}

fn toggle_bookmark(app: &Sh) {
    with(app, |a| {
        let Some(t) = a.cur_tab() else { return };
        if t.url.is_empty() { return }
        let url = t.url.clone();
        let title = if t.title.is_empty() { url.clone() } else { t.title.clone() };
        match a.marks.iter().position(|m| m.0 == url) {
            Some(i) => { a.marks.remove(i); }
            None => a.marks.push((url, title)),
        }
        save_marks(&a.marks);
        a.refresh();
    });
    rebuild_bookmarks(app);
}

fn rebuild_bookmarks(app: &Sh) {
    let Ok((bar, marks)) = app.try_borrow().map(|a| (a.bm_bar.clone(), a.marks.clone())) else { return };
    for c in bar.children() { bar.remove(&c) }
    for (url, title) in marks {
        let short: String = if title.chars().count() > 24 { title.chars().take(24).chain("…".chars()).collect() } else { title };
        let b = gtk::Button::with_label(&short);
        b.set_relief(gtk::ReliefStyle::None);
        b.set_tooltip_text(Some(&url));
        let a = app.clone();
        b.connect_clicked(move |_| go(&a, &url));
        b.show_all();
        bar.pack_start(&b, false, false, 0);
    }
    with(app, |a| a.sync_bars());
}

fn pin_tab(app: &Sh, id: u32, pin: bool) {
    with(app, |a| {
        let Some(i) = a.tabs.iter().position(|t| t.id == id) else { return };
        if a.tabs[i].pinned == pin {
            return;
        }
        a.tabs[i].pinned = pin;
        let pinned_n = a.tabs.iter().filter(|t| t.pinned).count();
        // pinned: join the end of the pinned group; unpinned: first of the rest
        a.move_tab(id, if pin { pinned_n - 1 } else { pinned_n });
        a.layout_tabs();
        a.save_session();
    });
}

fn close_tab(app: &Sh, id: u32) {
    let closed = {
        let Ok(mut a) = app.try_borrow_mut() else { return };
        let Some(i) = a.tabs.iter().position(|t| t.id == id) else { return };
        if a.tabs.len() == 1 { std::process::exit(0) }
        let t = a.tabs.remove(i);
        if !t.url.is_empty() { a.closed.push(t.url.clone()) }
        a.tabs_box.remove(&t.head);
        a.stack.remove(&t.page);
        if a.cur == id {
            let nid = a.tabs[i.min(a.tabs.len() - 1)].id;
            a.select(nid);
        } else {
            a.save_session();
        }
        t
    };
    // destroy the webview only after the App borrow is released
    drop(closed);
    schedule_layout(app);
    focus_current(app);
}

fn new_tab(app: &Sh, url: Option<String>, select: bool) -> Option<u32> {
    let (id, stack, tabs_box, ctx, s, first, find_entry) = {
        let Ok(mut a) = app.try_borrow_mut() else { return None };
        a.next_id += 1;
        let first = !a.downloads_hooked;
        a.downloads_hooked = true;
        (a.next_id, a.stack.clone(), a.tabs_box.clone(), a.ctx.clone(), a.settings.clone(), first, a.find_entry.clone())
    };
    let internal_page = url.as_deref().and_then(internal);
    let is_internal = internal_page.is_some();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.add_named(&page, &id.to_string());
    page.show();

    let (a1, a2, a3) = (app.clone(), app.clone(), app.clone());
    // One shared persistent context: cookies/logins survive restarts and are shared by tabs.
    let mut ctx = ctx.borrow_mut();
    let mut b = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_devtools(true)
        .with_back_forward_navigation_gestures(true)
        .with_document_title_changed_handler(move |t| with(&a1, |a| a.set_title(id, &t)))
        .with_new_window_req_handler(move |u, _| {
            let a = a2.clone();
            glib::idle_add_local_once(move || { new_tab(&a, Some(u), true); });
            NewWindowResponse::Deny
        })
        .with_ipc_handler(move |req| {
            // wry gives every page window.ipc; only our own settings page may use it
            if internal(&req.uri().to_string()).is_none() {
                return;
            }
            let (a, msg) = (a3.clone(), req.into_body());
            glib::idle_add_local_once(move || handle_ipc(&a, id, &msg));
        });
    if s.adblock {
        b = b.with_initialization_script_for_main_only(adblock::SCRIPT, false);
    }
    if first {
        // wry attaches these to the shared context, so they must be registered exactly once
        let d1 = app.clone();
        b = b
            .with_download_started_handler(move |_, path| {
                let dir = d1.try_borrow().map(|a| a.settings.download_dir());
                if let (Ok(dir), Some(name)) = (dir, path.file_name().map(OsStr::to_owned)) {
                    if path.parent() != Some(dir.as_path()) && fs::create_dir_all(&dir).is_ok() {
                        *path = unique_path(&dir, &name);
                    }
                }
                true
            });
    }
    b = match &url {
        Some(_) if is_internal => b,
        Some(u) => b.with_url(u),
        None => b.with_html(newtab_html(&s)),
    };
    let view = b.build_gtk(&page).expect("webview");
    drop(ctx);
    let wv = view.webview();
    adblock::apply(&wv, s.adblock);
    wv.set_zoom_level(s.zoom as f64 / 100.0);
    if first {
        if let Some(c) = wv.context() {
            downloads::hook(app, &c);
            if c.favicon_database_directory().is_none() {
                c.set_favicon_database_directory(Some(&settings::data_dir().join("favicons").to_string_lossy()));
            }
        }
    }
    if let Some(p) = internal_page {
        load_internal(app, &wv, p);
    }

    {
        let a = app.clone();
        wv.connect_context_menu(move |wv, _menu, event, hit| { menus::page_menu(&a, wv, hit, event); true });
        let a = app.clone();
        wv.connect_uri_notify(move |_| with(&a, |x| x.tab_changed(id, None)));
        let a = app.clone();
        wv.connect_load_changed(move |_, ev| with(&a, |x| x.tab_changed(id, Some(ev != LoadEvent::Finished))));
        let a = app.clone();
        wv.connect_estimated_load_progress_notify(move |_| with(&a, |x| x.tab_changed(id, None)));
        let a = app.clone();
        wv.connect_favicon_notify(move |_| with(&a, |x| {
            if let Some(t) = x.tabs.iter().find(|t| t.id == id) { set_favicon(t) }
        }));
        let a = app.clone();
        wv.connect_mouse_target_changed(move |_, hit, _| {
            let link = hit.link_uri().map(|u| u.to_string()).unwrap_or_default();
            with(&a, |x| if x.cur == id { x.set_status(&link) });
        });
        // tint the find box red when nothing matches
        if let Some(fc) = wv.find_controller() {
            let fe = find_entry.clone();
            fc.connect_failed_to_find_text(move |_| fe.style_context().add_class("notfound"));
            fc.connect_found_text(move |_, _| find_entry.style_context().remove_class("notfound"));
        }
        // video fullscreen: get our toolbar out of the way
        let a = app.clone();
        wv.connect_enter_fullscreen(move |_| { with(&a, |x| x.set_chrome(false)); false });
        let a = app.clone();
        wv.connect_leave_fullscreen(move |_| { with(&a, |x| x.set_chrome(true)); false });
        let a = app.clone();
        wv.connect_decide_policy(move |_, d, ty| match ty {
            // things the engine can't display (zip, exe, ...) become downloads
            PolicyDecisionType::Response => match d.downcast_ref::<ResponsePolicyDecision>() {
                Some(r) if !r.is_mime_type_supported() => { d.download(); true }
                _ => false,
            },
            // middle-click / Ctrl+click opens the link in a background tab
            PolicyDecisionType::NavigationAction => {
                let Some(n) = d.downcast_ref::<NavigationPolicyDecision>().and_then(|n| n.navigation_action())
                    else { return false };
                let ctrl = n.modifiers() & gdk::ModifierType::CONTROL_MASK.bits() != 0;
                if n.navigation_type() != NavigationType::LinkClicked || !(ctrl || n.mouse_button() == 2) {
                    return false;
                }
                let Some(u) = n.request().and_then(|r| r.uri()) else { return false };
                let (a, u) = (a.clone(), u.to_string());
                glib::idle_add_local_once(move || { new_tab(&a, Some(u), false); });
                d.ignore();
                true
            }
            _ => false,
        });
    }

    // ---- tab head: [spinner|icon] title [x] | separator ----
    let label = gtk::Label::new(Some("New Tab"));
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_xalign(0.0);
    // tiny natural width + hexpand: the label fills whatever width the layout gives the tab,
    // so every tab is exactly the same size regardless of its title
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    let icon = gtk::Image::from_icon_name(Some("web-browser-symbolic"), gtk::IconSize::Menu);
    icon.set_size_request(16, 16);
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(16, 16);
    let close = gtk::Button::from_icon_name(Some("window-close-symbolic"), gtk::IconSize::Menu);
    close.set_relief(gtk::ReliefStyle::None);
    close.set_valign(gtk::Align::Center);
    close.style_context().add_class("tab-close");
    let a = app.clone();
    close.connect_clicked(move |_| close_tab(&a, id));
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // GTK3 boxes ignore CSS padding, so spacing inside the tab is set here
    inner.set_margin_start(10);
    inner.set_margin_end(6);
    inner.pack_start(&spinner, false, false, 0);
    inner.pack_start(&icon, false, false, 0);
    inner.pack_start(&label, true, true, 0);
    inner.pack_start(&close, false, false, 0);
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.style_context().add_class("tab-sep");
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.pack_start(&inner, true, true, 0);
    outer.pack_start(&sep, false, false, 0);
    let head = gtk::EventBox::new();
    head.add(&outer);
    head.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON1_MOTION_MASK | gdk::EventMask::BUTTON_RELEASE_MASK);
    head.style_context().add_class("tab");
    // dragging: where the pointer was when pressed, and whether it has moved far enough to count
    let drag: Rc<Cell<Option<(f64, bool)>>> = Rc::new(Cell::new(None));
    let (a, d) = (app.clone(), drag.clone());
    head.connect_button_press_event(move |_, e| {
        match e.button() {
            1 => { d.set(Some((e.root().0, false))); select_tab(&a, id) }
            2 => close_tab(&a, id),
            3 => menus::tab_menu(&a, id, e),
            _ => {}
        }
        Propagation::Stop
    });
    let (app2, d) = (app.clone(), drag.clone());
    head.connect_motion_notify_event(move |h, e| {
        let Some((x0, moving)) = d.get() else { return Propagation::Proceed };
        if !moving && (e.root().0 - x0).abs() < 8.0 {
            return Propagation::Stop;
        }
        d.set(Some((x0, true)));
        // the tab under the pointer is where this one should go
        let to = {
            let Ok(a) = app2.try_borrow() else { return Propagation::Stop };
            let Some(px) = h.translate_coordinates(&a.tabs_box, e.position().0 as i32, 0).map(|p| p.0) else {
                return Propagation::Stop;
            };
            let (mut x, mut to) = (0, a.tabs.len().saturating_sub(1));
            for (i, t) in a.tabs.iter().enumerate() {
                x += t.head.width_request().max(PIN_W);
                if px < x {
                    to = i;
                    break;
                }
            }
            to
        };
        with(&app2, |x| x.move_tab(id, to));
        Propagation::Stop
    });
    let (a, d) = (app.clone(), drag);
    head.connect_button_release_event(move |_, _| {
        if d.take().is_some_and(|(_, moving)| moving) {
            with(&a, |x| x.save_session());
        }
        Propagation::Proceed
    });
    head.show_all();
    spinner.hide();
    head.set_size_request(TAB_MAX, -1);
    tabs_box.pack_start(&head, false, false, 0);

    let start_url = match (&url, internal_page) {
        (Some(_), Some(p)) => p.uri().to_string(),
        (Some(u), None) => u.clone(),
        (None, _) => String::new(),
    };
    let tab = Tab {
        id, page, head, label, icon, spinner, close, sep, pinned: false, _view: view, wv,
        // show the address right away instead of waiting for the first load event
        loading: !start_url.is_empty() && !is_internal,
        url: start_url,
        title: String::new(),
    };
    set_favicon(&tab);
    {
        let Ok(mut a) = app.try_borrow_mut() else { return Some(id) };
        a.tabs.push(tab);
        if let Some(u) = url.as_deref().filter(|_| !select && !is_internal) {
            a.set_title(id, u);
        }
        if select {
            a.select(id);
        } else {
            a.save_session();
        }
    }
    schedule_layout(app);
    if select {
        focus_current(app);
    }
    Some(id)
}

fn icon_button(icon: &str) -> (gtk::Button, gtk::Image) {
    let img = gtk::Image::from_icon_name(Some(icon), gtk::IconSize::Menu);
    let b = gtk::Button::new();
    b.add(&img);
    b.set_relief(gtk::ReliefStyle::None);
    (b, img)
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("-V" | "--version") => return println!("zolith {}", env!("CARGO_PKG_VERSION")),
        Some("-h" | "--help") => {
            return println!(
                "zolith {}\nA fast, minimal web browser.\n\nUSAGE:\n    zolith [URL]\n\nOPTIONS:\n    -h, --help       Show this help\n    -V, --version    Show the version\n\nSettings: open zolith://settings (Ctrl+,)",
                env!("CARGO_PKG_VERSION")
            )
        }
        _ => {}
    }
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Zolith")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_visible(false)
        .build(&event_loop)
        .unwrap();
    let win = window.gtk_window().clone();
    // matches the launcher's icon (~/.local/share/icons/hicolor/*/apps/zolith.png)
    win.set_icon_name(Some("zolith"));

    if let Some(s) = gtk::Settings::default() { s.set_gtk_application_prefer_dark_theme(true) }
    let css = gtk::CssProvider::new();
    css.load_from_data(CSS.as_bytes()).expect("valid css");
    gtk::StyleContext::add_provider_for_screen(
        &gdk::Screen::default().expect("screen"), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

    // ---- toolbar ----
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    toolbar.style_context().add_class("toolbar");
    let (back, _) = icon_button("go-previous-symbolic");
    let (fwd, _) = icon_button("go-next-symbolic");
    let (reload, reload_img) = icon_button("view-refresh-symbolic");
    let (home, _) = icon_button("go-home-symbolic");
    let (star, star_img) = icon_button("non-starred-symbolic");
    back.set_tooltip_text(Some("Back (Alt+Left). Right-click for history"));
    fwd.set_tooltip_text(Some("Forward (Alt+Right). Right-click for history"));
    reload.set_tooltip_text(Some("Reload (Ctrl+R)"));
    home.set_tooltip_text(Some("Home (Alt+Home)"));
    star.set_tooltip_text(Some("Bookmark this page (Ctrl+D)"));
    home.show_all();
    home.set_no_show_all(true);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Search or enter address"));
    entry.style_context().add_class("urlbar");
    let sug_list = gtk::ListBox::new();
    sug_list.set_can_focus(false);
    sug_list.set_selection_mode(gtk::SelectionMode::Single);
    sug_list.set_activate_on_single_click(true);
    let sug_pop = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sug_pop.set_halign(gtk::Align::Start);
    sug_pop.set_valign(gtk::Align::Start);
    sug_pop.style_context().add_class("suggestpop");
    sug_pop.add(&sug_list);
    sug_list.show();
    sug_pop.set_no_show_all(true);
    sug_list.style_context().add_class("suggest");
    let menu_btn = gtk::MenuButton::new();
    menu_btn.add(&gtk::Image::from_icon_name(Some("open-menu-symbolic"), gtk::IconSize::Menu));
    menu_btn.set_relief(gtk::ReliefStyle::None);
    let pop = gtk::Popover::new(Some(&menu_btn));
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.set_border_width(6);
    menu_btn.set_popover(Some(&pop));
    for b in [&back, &fwd, &reload, &home] { toolbar.pack_start(b, false, false, 0) }
    let (dl_btn, _) = icon_button("folder-download-symbolic");
    dl_btn.set_tooltip_text(Some("Downloads"));
    dl_btn.show_all();
    dl_btn.set_no_show_all(true);
    dl_btn.hide();
    let dl_pop = gtk::Popover::new(Some(&dl_btn));
    let dl_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dl_box.set_size_request(380, -1);
    let dl_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let dl_clear = gtk::Button::with_label("Clear list");
    dl_clear.set_relief(gtk::ReliefStyle::None);
    dl_box.pack_start(&dl_list, false, false, 0);
    dl_box.pack_start(&dl_clear, false, false, 4);
    dl_box.show_all();
    dl_pop.add(&dl_box);
    toolbar.pack_start(&entry, true, true, 6);
    toolbar.pack_start(&star, false, false, 0);
    toolbar.pack_start(&dl_btn, false, false, 0);
    toolbar.pack_start(&menu_btn, false, false, 0);

    // ---- bookmarks bar, content stack (+ status corner), find bar ----
    let bm_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    bm_bar.style_context().add_class("bookmarks");
    bm_bar.set_no_show_all(true);
    let stack = gtk::Stack::new();
    let status = gtk::Label::new(None);
    status.style_context().add_class("status");
    status.set_halign(gtk::Align::Start);
    status.set_valign(gtk::Align::End);
    status.set_ellipsize(pango::EllipsizeMode::Middle);
    status.set_max_width_chars(90);
    status.set_no_show_all(true);
    let overlay = gtk::Overlay::new();
    overlay.add(&stack);
    overlay.add_overlay(&status);
    overlay.set_overlay_pass_through(&status, true);
    let find_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    find_bar.style_context().add_class("findbar");
    let find_entry = gtk::Entry::new();
    find_entry.set_placeholder_text(Some("Find in page"));
    find_entry.set_width_chars(30);
    let (fprev, _) = icon_button("go-up-symbolic");
    let (fnext, _) = icon_button("go-down-symbolic");
    let (fclose, _) = icon_button("window-close-symbolic");
    for w in [find_entry.upcast_ref::<gtk::Widget>(), fprev.upcast_ref(), fnext.upcast_ref(), fclose.upcast_ref()] {
        find_bar.pack_start(w, false, false, 0);
    }
    find_bar.show_all();
    find_bar.hide();
    find_bar.set_no_show_all(true);

    let vbox = window.default_vbox().expect("vbox");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.pack_start(&toolbar, false, false, 0);
    content.pack_start(&bm_bar, false, false, 0);
    content.pack_start(&overlay, true, true, 0);
    content.pack_start(&find_bar, false, false, 0);
    // the address-bar dropdown floats over everything below the toolbar
    let top = gtk::Overlay::new();
    top.add(&content);
    top.add_overlay(&sug_pop);
    vbox.pack_start(&top, true, true, 0);

    // ---- titlebar: scrollable tab strip + window controls ----
    let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let (plus, _) = icon_button("list-add-symbolic");
    plus.style_context().add_class("newtab-btn");
    plus.set_tooltip_text(Some("New tab (Ctrl+T)"));
    plus.set_valign(gtk::Align::Center);
    let tabs_scroll = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
    // External: scrollable, no scrollbar, and the strip never forces the window wider
    tabs_scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
    tabs_scroll.set_kinetic_scrolling(false);
    tabs_scroll.set_shadow_type(gtk::ShadowType::None);
    // natural width = the tabs' width, so + sits right after the last tab; when they don't
    // fit, GTK shrinks the scroller (its minimum is tiny) and the strip scrolls
    tabs_scroll.set_propagate_natural_width(true);
    // explicit: otherwise the tab labels' hexpand propagates up and the scroller grabs spare room
    tabs_scroll.set_hexpand(false);
    tabs_scroll.style_context().add_class("tabs-scroll");
    tabs_scroll.add(&tabs_box);
    if let Some(vp) = tabs_scroll.child().and_then(|c| c.downcast::<gtk::Viewport>().ok()) {
        vp.set_shadow_type(gtk::ShadowType::None);
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_size_request(DRAG_W, -1);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.set_valign(gtk::Align::Center);
    let (wmin, _) = icon_button("window-minimize-symbolic");
    let (wmax, _) = icon_button("window-maximize-symbolic");
    let (wclose, _) = icon_button("window-close-symbolic");
    for b in [&wmin, &wmax, &wclose] {
        b.style_context().add_class("winbtn");
        controls.pack_start(b, false, false, 0);
    }
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    strip.set_margin_top(8);
    strip.set_margin_start(8);
    strip.set_margin_end(4);
    strip.pack_start(&tabs_scroll, false, false, 0);
    strip.pack_start(&plus, false, false, 0);
    strip.pack_start(&spacer, true, true, 0);
    strip.pack_end(&controls, false, false, 0);
    let titlebar = gtk::EventBox::new();
    titlebar.add(&strip);
    titlebar.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
    titlebar.style_context().add_class("titlebar-box");
    let toggle_max = { let w = win.clone(); move || if w.is_maximized() { w.unmaximize() } else { w.maximize() } };
    {
        let (w, tm) = (win.clone(), toggle_max.clone());
        titlebar.connect_button_press_event(move |_, e| {
            if e.button() == 1 {
                if e.event_type() == gdk::EventType::DoubleButtonPress { tm() } else {
                    let (x, y) = e.root();
                    w.begin_move_drag(1, x as i32, y as i32, e.time());
                }
            }
            Propagation::Stop
        });
    }
    { let w = win.clone(); wmin.connect_clicked(move |_| w.iconify()); }
    wmax.connect_clicked(move |_| toggle_max());
    wclose.connect_clicked(|_| std::process::exit(0));
    win.set_titlebar(Some(&titlebar));

    // ---- state ----
    let settings = Settings::load();
    let extra = fs::read_to_string(settings::config_dir().join("blocklist.txt")).unwrap_or_default();
    adblock::init(&settings::data_dir(), &extra);
    let app: Sh = Rc::new(RefCell::new(App {
        next_id: 0, cur: 0, tabs: vec![], closed: vec![], marks: load_marks(), settings,
        downloads_hooked: false, fullscreen: false, layout_pending: false, win: win.clone(),
        toolbar: toolbar.clone(), stack, tabs_scroll: tabs_scroll.clone(), tabs_box: tabs_box.clone(),
        strip: strip.clone(), controls: controls.clone(),
        entry: entry.clone(), back: back.clone(), fwd: fwd.clone(), home_btn: home.clone(), reload_img, star_img,
        bm_bar, find_bar, find_entry: find_entry.clone(), status,
        ctx: Rc::new(RefCell::new(WebContext::new(Some(settings::data_dir())))),
        history: suggest::load(), sug_pop, top: top.clone(), sug_list: sug_list.clone(), sug_rows: vec![], sug_seq: 0,
        last_typed: String::new(), inline_ok: false,
        http: suggest::new_session(),
        dls: vec![], next_dl: 0, dl_pending: false, dl_btn: dl_btn.clone(), dl_list: dl_list.clone(),
    }));

    // ---- wiring ----
    {
        // re-fit tabs whenever the window changes width
        let (a, last) = (app.clone(), Cell::new(-1));
        strip.connect_size_allocate(move |_, alloc| {
            if alloc.width() != last.replace(alloc.width()) { schedule_layout(&a) }
        });
        // mouse wheel scrolls an overflowing strip sideways
        tabs_scroll.connect_scroll_event(|sw, e| {
            let (dx, dy) = e.delta();
            let step = match e.direction() {
                gdk::ScrollDirection::Up | gdk::ScrollDirection::Left => -1.0,
                gdk::ScrollDirection::Down | gdk::ScrollDirection::Right => 1.0,
                _ => if dy.abs() > dx.abs() { dy } else { dx },
            };
            let adj = sw.hadjustment();
            adj.set_value(adj.value() + step * 60.0);
            Propagation::Stop
        });
        let a = app.clone();
        tabs_box.connect_size_allocate(move |_, _| { if let Ok(x) = a.try_borrow() { x.scroll_to_selected() } });
    }
    let bind = |b: &gtk::Button, c: Cmd| { let a = app.clone(); b.connect_clicked(move |_| run(&a, c)); };
    bind(&back, Cmd::Back); bind(&fwd, Cmd::Forward); bind(&reload, Cmd::Reload); bind(&home, Cmd::Home);
    bind(&star, Cmd::Bookmark); bind(&plus, Cmd::NewTab);
    for (b, is_back) in [(&back, true), (&fwd, false)] {
        let a = app.clone();
        b.connect_button_press_event(move |_, e| {
            if e.button() == 3 { menus::nav_menu(&a, is_back, e); Propagation::Stop } else { Propagation::Proceed }
        });
    }
    {
        let (a, p) = (app.clone(), dl_pop.clone());
        dl_btn.connect_clicked(move |_| { downloads::render(&a); p.popup() });
        let a = app.clone();
        dl_clear.connect_clicked(move |_| downloads::clear(&a));
    }
    bind(&fprev, Cmd::FindNext(true)); bind(&fnext, Cmd::FindNext(false)); bind(&fclose, Cmd::CloseFind);
    {
        // only user typing opens suggestions: programmatic updates happen while the App is borrowed
        let a = app.clone();
        entry.connect_changed(move |e| {
            if e.has_focus() && a.try_borrow().is_ok() { suggest_typed(&a, &e.text()) }
        });
        let a = app.clone();
        entry.connect_focus_out_event(move |_, _| { hide_suggestions(&a); Propagation::Proceed });
        let a = app.clone();
        sug_list.connect_row_activated(move |_, row| {
            let url = a.try_borrow().ok().and_then(|x| x.sug_rows.get(row.index() as usize).map(|r| r.url.clone()));
            if let Some(u) = url { go(&a, &u) }
        });
    }
    { let a = app.clone(); find_entry.connect_activate(move |_| run(&a, Cmd::FindNext(false))); }
    { let a = app.clone(); find_entry.connect_changed(move |_| run(&a, Cmd::FindTyped)); }
    for (label, c) in [
        ("New tab", Cmd::NewTab), ("Reopen closed tab", Cmd::Reopen), ("Bookmark this page", Cmd::Bookmark),
        ("Find in page", Cmd::Find), ("Zoom in", Cmd::Zoom(1)), ("Zoom out", Cmd::Zoom(-1)),
        ("Reset zoom", Cmd::Zoom(0)), ("Print…", Cmd::Print), ("Developer tools", Cmd::Devtools),
        ("History", Cmd::History), ("Settings", Cmd::Settings), ("Quit", Cmd::Quit),
    ] {
        let b = gtk::Button::with_label(label);
        b.set_relief(gtk::ReliefStyle::None);
        if let Some(l) = b.child().and_then(|c| c.downcast::<gtk::Label>().ok()) { l.set_xalign(0.0) }
        let (a, p) = (app.clone(), pop.clone());
        b.connect_clicked(move |_| { p.popdown(); run(&a, c) });
        menu.pack_start(&b, false, false, 0);
    }
    menu.show_all();
    pop.add(&menu);

    {
        let a = app.clone();
        win.connect_key_press_event(move |_, ev| {
            use gdk::keys::constants as k;
            let (key, st) = (ev.keyval(), ev.state());
            let ctrl = st.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = st.contains(gdk::ModifierType::SHIFT_MASK);
            let alt = st.contains(gdk::ModifierType::MOD1_MASK);
            let ch = key.to_unicode().map(|c| c.to_ascii_lowercase());
            let cmd = if ctrl && !alt {
                match (shift, ch) {
                    (_, Some('+' | '=')) => Some(Cmd::Zoom(1)),
                    (_, Some('-' | '_')) => Some(Cmd::Zoom(-1)),
                    (_, Some('0')) => Some(Cmd::Zoom(0)),
                    (false, Some(d @ '1'..='9')) => Some(Cmd::NthTab(d as usize - '1' as usize)),
                    (false, Some('t')) => Some(Cmd::NewTab),
                    (true, Some('t')) => Some(Cmd::Reopen),
                    (false, Some('w')) => Some(Cmd::CloseTab),
                    (false, Some('l')) => Some(Cmd::FocusUrl),
                    (false, Some('r')) => Some(Cmd::Reload),
                    (false, Some('f')) => Some(Cmd::Find),
                    (false, Some('g')) => Some(Cmd::FindNext(false)),
                    (true, Some('g')) => Some(Cmd::FindNext(true)),
                    (false, Some('d')) => Some(Cmd::Bookmark),
                    (true, Some('b')) => Some(Cmd::ToggleBookmarksBar),
                    (false, Some('p')) => Some(Cmd::Print),
                    (true, Some('i')) => Some(Cmd::Devtools),
                    (false, Some(',')) => Some(Cmd::Settings),
                    (false, Some('h')) => Some(Cmd::History),
                    (false, Some('q')) => Some(Cmd::Quit),
                    _ if key == k::Tab || key == k::Page_Down => Some(Cmd::NextTab(1)),
                    _ if key == k::ISO_Left_Tab || key == k::Page_Up => Some(Cmd::NextTab(-1)),
                    _ => None,
                }
            } else if alt && !ctrl {
                match key {
                    k::Left => Some(Cmd::Back),
                    k::Right => Some(Cmd::Forward),
                    k::Home => Some(Cmd::Home),
                    _ => None,
                }
            } else if !ctrl && !alt {
                let (finding, in_url, dropdown) = a.try_borrow()
                    .map(|x| (x.find_bar.is_visible(), x.entry.has_focus(), x.sug_pop.is_visible()))
                    .unwrap_or((false, false, false));
                match key {
                    k::F5 => Some(Cmd::Reload),
                    k::F3 => Some(Cmd::FindNext(shift)),
                    k::F6 => Some(Cmd::FocusUrl),
                    k::F12 => Some(Cmd::Devtools),
                    // handled here because the open dropdown swallows Enter before the entry sees it
                    k::Return | k::KP_Enter if in_url => Some(Cmd::EnterUrl),
                    k::Down if in_url && dropdown => Some(Cmd::SuggestMove(1)),
                    k::Up if in_url && dropdown => Some(Cmd::SuggestMove(-1)),
                    k::Escape if finding => Some(Cmd::CloseFind),
                    k::Escape if in_url => Some(Cmd::Escape),
                    _ => None,
                }
            } else {
                None
            };
            match cmd {
                Some(c) => { run(&a, c); Propagation::Stop }
                None => Propagation::Proceed,
            }
        });
    }

    // an empty find box is never "not found"
    {
        let fe = find_entry.clone();
        find_entry.connect_changed(move |e| if e.text().is_empty() { fe.style_context().remove_class("notfound") });
    }

    rebuild_bookmarks(&app);

    // ---- startup tabs ----
    let (start, sel): (Vec<Option<String>>, usize) = {
        let a = app.borrow();
        let s = &a.settings;
        let t = s.search_template().to_string();
        let page = |u: &str| match internal(u) { Some(p) => p.uri().to_string(), None => to_url(u, &t) };
        match std::env::args().nth(1) {
            Some(arg) => (vec![Some(page(&arg))], 0),
            None => match s.startup.as_str() {
                "restore" => {
                    let (cur, urls) = load_session();
                    let tabs: Vec<_> = urls.into_iter().map(|u| (!u.trim_start_matches('*').is_empty()).then_some(u)).collect();
                    if tabs.is_empty() { (vec![None], 0) } else { let n = tabs.len(); (tabs, cur.min(n - 1)) }
                }
                "page" if !s.startup_page.trim().is_empty() => (vec![Some(page(&s.startup_page))], 0),
                _ => (vec![None], 0),
            },
        }
    };
    // a leading "*" in a saved line means the tab was pinned
    let ids: Vec<u32> = start.into_iter().enumerate().filter_map(|(i, u)| {
        let pinned = u.as_deref().is_some_and(|u| u.starts_with('*'));
        let id = new_tab(&app, u.map(|u| u.trim_start_matches('*').to_string()), i == 0)?;
        if pinned { pin_tab(&app, id, true) }
        Some(id)
    }).collect();
    if let Some(&id) = ids.get(sel) { select_tab(&app, id) }

    titlebar.show_all();
    vbox.show_all();
    with(&app, |a| a.sync_bars());
    window.set_visible(true);

    event_loop.run(move |event, _, flow| {
        let _keep = &app;
        *flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *flow = ControlFlow::Exit;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtab_uses_chosen_engine() {
        let s = Settings { engine: "Google".into(), ..Default::default() };
        let html = newtab_html(&s);
        assert!(!html.contains("__SEARCH__;"));
        assert!(html.contains(r#"const template = "https://www.google.com/search?q=%s";"#));
    }

    #[test]
    fn settings_page_is_filled_in() {
        let html = settings_html(&Settings::default());
        for p in ["__STATE__", "__ENGINES__", "__ZOOMS__", "__ADBLOCK_READY__", "__VERSION__"] {
            assert!(!html.contains(p), "{p} left in page");
        }
        assert!(html.contains(r#"const ENGINES = ["DuckDuckGo","Google","Bing","Brave","Startpage","Custom"];"#));
        assert!(html.contains("const ZOOMS = [67,75,80,90,100,110,125,150,200];"));
        assert!(html.contains(r#""engine":"DuckDuckGo""#));
    }

    #[test]
    fn settings_urls() {
        for u in ["zolith://settings", "zolith://settings/", "ZOLITH://Settings/", "about:settings",
                  "chrome://settings", " chrome://settings/#x "] {
            assert_eq!(internal(u), Some(Internal::Settings), "{u}");
        }
        for u in ["zolith://history", "ZOLITH://History/", "about:history", "chrome://history/?q=1"] {
            assert_eq!(internal(u), Some(Internal::History), "{u}");
        }
        for u in ["", "https://zolith://settings", "zolith://settingsx", "zolith://settings.evil.com/",
                  "https://example.com/zolith://settings"] {
            assert!(internal(u).is_none(), "{u}");
        }
        // what wry hands the IPC handler for our page must pass the check
        let req = wry::http::Request::builder().uri(SETTINGS_URI).body(()).unwrap();
        assert_eq!(internal(&req.uri().to_string()), Some(Internal::Settings));
        let req = wry::http::Request::builder().uri(HISTORY_URI).body(()).unwrap();
        assert_eq!(internal(&req.uri().to_string()), Some(Internal::History));
    }

    #[test]
    fn download_names_do_not_collide() {
        let dir = std::env::temp_dir().join(format!("zolith-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, OsStr::new("a.tar.gz")), dir.join("a.tar.gz"));
        fs::write(dir.join("a.zip"), "").unwrap();
        fs::write(dir.join("a (1).zip"), "").unwrap();
        assert_eq!(unique_path(&dir, OsStr::new("a.zip")), dir.join("a (2).zip"));
        fs::write(dir.join("noext"), "").unwrap();
        assert_eq!(unique_path(&dir, OsStr::new("noext")), dir.join("noext (1)"));
        fs::remove_dir_all(&dir).unwrap();
    }
}

