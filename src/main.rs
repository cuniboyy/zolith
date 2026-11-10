mod adblock;
mod settings;

use gtk::{gdk, glib, glib::Propagation, pango, prelude::*};
use settings::{quote, to_url, Settings};
use std::{cell::RefCell, fs, path::PathBuf, rc::Rc};
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
    PrintOperationExt, ResponsePolicyDecision, ResponsePolicyDecisionExt, URIRequestExt, WebInspectorExt,
    WebViewExt,
};
use wry::{NewWindowResponse, WebContext, WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};

const CSS: &str = include_str!("style.css");
const NEWTAB: &str = include_str!("newtab.html");

// shortcut: no history page, favicons, private mode, session restore or tab dragging.

#[derive(Clone, Copy)]
enum Cmd {
    NewTab, CloseTab, Reopen, Reload, Back, Forward, Home, FocusUrl, Escape,
    Find, FindNext(bool), FindTyped, CloseFind, Zoom(i8), Print, Devtools, Bookmark,
    NextTab(i32), NthTab(usize), Settings, Quit,
}

struct Tab {
    id: u32,
    page: gtk::Box,
    head: gtk::EventBox,
    label: gtk::Label,
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
    win: gtk::ApplicationWindow,
    toolbar: gtk::Box,
    stack: gtk::Stack,
    tabs_box: gtk::Box,
    entry: gtk::Entry,
    back: gtk::Button,
    fwd: gtk::Button,
    reload_img: gtk::Image,
    star_img: gtk::Image,
    bm_bar: gtk::Box,
    find_bar: gtk::Box,
    find_entry: gtk::Entry,
    status: gtk::Label,
    ctx: Rc<RefCell<WebContext>>,
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
    app.try_borrow().ok().map(|a| a.cur().wv.clone())
}

fn newtab_html(s: &Settings) -> String {
    NEWTAB.replace("__SEARCH__", &quote(s.search_template()))
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

impl App {
    fn cur(&self) -> &Tab {
        self.tabs.iter().find(|t| t.id == self.cur).expect("current tab exists")
    }

    fn show_url(&self) {
        let u = &self.cur().url;
        self.entry.set_text(u.strip_prefix("https://").unwrap_or(u));
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_visible(!text.is_empty());
    }

    fn set_chrome(&mut self, show: bool) {
        self.fullscreen = !show;
        self.toolbar.set_visible(show);
        self.bm_bar.set_visible(show && !self.marks.is_empty());
        if !show {
            self.find_bar.hide();
            self.set_status("");
        }
    }

    /// Sync the toolbar with the current tab.
    fn refresh(&self) {
        let t = self.cur();
        if !self.entry.has_focus() {
            self.show_url();
        }
        let icon = if t.url.starts_with("https://") { Some("channel-secure-symbolic") }
            else if t.url.starts_with("http://") { Some("channel-insecure-symbolic") }
            else if t.url.is_empty() { Some("system-search-symbolic") } else { None };
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
        self.show_url();
        self.set_status("");
        self.refresh();
    }

    /// A tab's page changed (address, load state or progress).
    fn tab_changed(&mut self, id: u32, loading: Option<bool>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        if let Some(u) = t.wv.uri() {
            t.url = if u.starts_with("about:") { String::new() } else { u.to_string() };
        }
        if let Some(l) = loading {
            t.loading = l;
        }
        if id == self.cur {
            self.refresh();
        }
    }

    fn set_title(&mut self, id: u32, title: &str) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        t.title = title.to_string();
        let shown = if title.is_empty() { if t.url.is_empty() { "New Tab" } else { &t.url } } else { title };
        t.label.set_text(shown);
        t.head.set_tooltip_text(Some(shown));
        if id == self.cur {
            self.refresh();
        }
    }
}

/// Show a message in the status corner for a few seconds.
fn flash(app: &Sh, msg: String) {
    with(app, |a| a.set_status(&msg));
    let a = app.clone();
    glib::timeout_add_seconds_local_once(6, move || {
        with(&a, |x| if x.status.text() == msg.as_str() { x.set_status("") })
    });
}

/// Give keyboard focus to the page, or to the URL bar on the new tab page.
fn focus_current(app: &Sh) {
    let Ok(a) = app.try_borrow() else { return };
    let (wv, entry, blank) = { let t = a.cur(); (t.wv.clone(), a.entry.clone(), t.url.is_empty()) };
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

fn run(app: &Sh, c: Cmd) {
    match c {
        Cmd::NewTab => new_tab(app, None, true),
        Cmd::Reopen => {
            let u = app.try_borrow_mut().ok().and_then(|mut a| a.closed.pop());
            if let Some(u) = u { new_tab(app, Some(u), true) }
        }
        Cmd::CloseTab => {
            let id = app.try_borrow().map(|a| a.cur);
            if let Ok(id) = id { close_tab(app, id) }
        }
        Cmd::Quit => std::process::exit(0),
        Cmd::Settings => open_settings(app),
        Cmd::Bookmark => toggle_bookmark(app),
        Cmd::FocusUrl => with(app, |a| { a.entry.grab_focus(); a.entry.select_region(0, -1) }),
        Cmd::Find => with(app, |a| {
            if !a.fullscreen {
                a.find_bar.show();
                a.find_entry.grab_focus();
                a.find_entry.select_region(0, -1);
            }
        }),
        Cmd::NextTab(d) => {
            let id = app.try_borrow().ok().map(|a| {
                let i = a.tabs.iter().position(|t| t.id == a.cur).unwrap_or(0) as i32;
                a.tabs[(i + d).rem_euclid(a.tabs.len() as i32) as usize].id
            });
            if let Some(id) = id { select_tab(app, id) }
        }
        Cmd::NthTab(n) => {
            // like Firefox: Ctrl+9 is always the last tab
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
                Cmd::Reload => if wv.is_loading() { wv.stop_loading() } else { wv.reload() },
                Cmd::Home => load_home(app, &wv),
                Cmd::Zoom(d) => wv.set_zoom_level(if d == 0 { 1.0 } else {
                    ((wv.zoom_level() * 10.0).round() + d as f64).clamp(3.0, 30.0) / 10.0
                }),
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
                    // leave the URL bar: put the real address back
                    with(app, |a| a.show_url());
                    focus_current(app);
                }
                _ => {}
            }
        }
    }
}

fn go(app: &Sh, input: &str) {
    if input.trim().is_empty() { return }
    let Some(wv) = cur_wv(app) else { return };
    let Ok(template) = app.try_borrow().map(|a| a.settings.search_template().to_string()) else { return };
    wv.load_uri(&to_url(input, &template));
    wv.grab_focus();
}

fn toggle_bookmark(app: &Sh) {
    with(app, |a| {
        let t = a.cur();
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
    let Ok((bar, marks, fullscreen)) = app.try_borrow().map(|a| (a.bm_bar.clone(), a.marks.clone(), a.fullscreen))
        else { return };
    for c in bar.children() { bar.remove(&c) }
    bar.set_visible(!marks.is_empty() && !fullscreen);
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
        }
        t
    };
    // destroy the webview only after the App borrow is released
    drop(closed);
    focus_current(app);
}

fn new_tab(app: &Sh, url: Option<String>, select: bool) {
    let (id, stack, tabs_box, ctx, s, hook_downloads, find_entry) = {
        let Ok(mut a) = app.try_borrow_mut() else { return };
        a.next_id += 1;
        let hook = !a.downloads_hooked;
        a.downloads_hooked = true;
        (a.next_id, a.stack.clone(), a.tabs_box.clone(), a.ctx.clone(), a.settings.clone(), hook, a.find_entry.clone())
    };
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.add_named(&page, &id.to_string());
    page.show();

    let (a1, a2) = (app.clone(), app.clone());
    // One shared persistent context: cookies/logins survive restarts and are shared by tabs.
    let mut ctx = ctx.borrow_mut();
    let mut b = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_devtools(true)
        .with_back_forward_navigation_gestures(true)
        .with_document_title_changed_handler(move |t| with(&a1, |a| a.set_title(id, &t)))
        .with_new_window_req_handler(move |u, _| {
            let a = a2.clone();
            glib::idle_add_local_once(move || new_tab(&a, Some(u), true));
            NewWindowResponse::Deny
        });
    if s.adblock {
        b = b.with_initialization_script_for_main_only(adblock::SCRIPT, false);
    }
    if hook_downloads {
        // wry attaches these to the shared context, so they must be registered exactly once
        let (d1, d2) = (app.clone(), app.clone());
        b = b
            .with_download_started_handler(move |_, path| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                flash(&d1, format!("Downloading {name}…"));
                true
            })
            .with_download_completed_handler(move |_, path, ok| {
                flash(&d2, match (ok, path) {
                    (true, Some(p)) => format!("Saved {}", p.display()),
                    (true, None) => "Download finished".into(),
                    (false, _) => "Download failed".into(),
                });
            });
    }
    b = match &url { Some(u) => b.with_url(u), None => b.with_html(newtab_html(&s)) };
    let view = b.build_gtk(&page).expect("webview");
    drop(ctx);
    let wv = view.webview();
    adblock::apply(&wv, s.adblock);

    {
        let a = app.clone();
        wv.connect_uri_notify(move |_| with(&a, |x| x.tab_changed(id, None)));
        let a = app.clone();
        wv.connect_load_changed(move |_, ev| with(&a, |x| x.tab_changed(id, Some(ev != LoadEvent::Finished))));
        let a = app.clone();
        wv.connect_estimated_load_progress_notify(move |_| with(&a, |x| x.tab_changed(id, None)));
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
                glib::idle_add_local_once(move || new_tab(&a, Some(u), false));
                d.ignore();
                true
            }
            _ => false,
        });
    }

    let label = gtk::Label::new(Some("New Tab"));
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_xalign(0.0);
    // no width_chars: it forces a minimum width, so many tabs would stop the window shrinking
    label.set_max_width_chars(24);
    label.set_hexpand(true);
    let close = gtk::Button::from_icon_name(Some("window-close-symbolic"), gtk::IconSize::Menu);
    close.set_relief(gtk::ReliefStyle::None);
    close.style_context().add_class("tab-close");
    let a = app.clone();
    close.connect_clicked(move |_| close_tab(&a, id));
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    inner.pack_start(&label, true, true, 0);
    inner.pack_start(&close, false, false, 0);
    let head = gtk::EventBox::new();
    head.add(&inner);
    head.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
    head.style_context().add_class("tab");
    let a = app.clone();
    head.connect_button_press_event(move |_, e| {
        match e.button() {
            1 => select_tab(&a, id),
            2 => close_tab(&a, id),
            _ => {}
        }
        Propagation::Stop
    });
    head.show_all();
    tabs_box.pack_start(&head, true, true, 0);

    let tab = Tab {
        id, page, head, label, _view: view, wv,
        // show the address right away instead of waiting for the first load event
        url: url.clone().unwrap_or_default(),
        title: String::new(),
        loading: url.is_some(),
    };
    let Ok(mut a) = app.try_borrow_mut() else { return };
    a.tabs.push(tab);
    if let Some(u) = url.as_deref().filter(|_| !select) {
        a.set_title(id, u);
    }
    if select {
        a.select(id);
        drop(a);
        focus_current(app);
    }
}

fn open_settings(app: &Sh) {
    let Ok((parent, s)) = app.try_borrow().map(|a| (a.win.clone(), a.settings.clone())) else { return };
    let w = gtk::Window::new(gtk::WindowType::Toplevel);
    w.set_title("Settings");
    w.set_transient_for(Some(&parent));
    w.set_modal(true);
    w.set_resizable(false);
    w.set_default_size(480, -1);
    w.set_type_hint(gdk::WindowTypeHint::Dialog);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(12);
    grid.set_column_spacing(16);
    grid.set_border_width(20);
    let row = |i: i32, text: &str, widget: &gtk::Widget| {
        let l = gtk::Label::new(Some(text));
        l.set_xalign(0.0);
        grid.attach(&l, 0, i, 1, 1);
        widget.set_hexpand(true);
        grid.attach(widget, 1, i, 1, 1);
    };

    let engine = gtk::ComboBoxText::new();
    for (name, _) in settings::ENGINES { engine.append(Some(name), name) }
    engine.append(Some(settings::CUSTOM), settings::CUSTOM);
    if !engine.set_active_id(Some(&s.engine)) { engine.set_active(Some(0)) }
    let custom = gtk::Entry::new();
    custom.set_placeholder_text(Some("https://example.com/search?q=%s"));
    custom.set_text(&s.custom);
    custom.set_sensitive(s.engine == settings::CUSTOM);
    let home = gtk::Entry::new();
    home.set_placeholder_text(Some("Empty = new tab page"));
    home.set_text(&s.home);
    let adblock_sw = gtk::Switch::new();
    adblock_sw.set_active(s.adblock);
    adblock_sw.set_halign(gtk::Align::Start);
    let note = gtk::Label::new(Some(&format!(
        "{}\nChanges save as you make them. The ad blocker switch fully applies to new tabs.\n\
         Extra domains to block: one per line in {}",
        if adblock::is_ready() { "Ad filter loaded." } else { "Ad filter failed to load, so only YouTube ad skipping is active." },
        settings::config_dir().join("blocklist.txt").display(),
    )));
    note.set_xalign(0.0);
    note.set_line_wrap(true);
    note.set_max_width_chars(60);
    note.style_context().add_class("dim-label");

    row(0, "Search engine", engine.upcast_ref());
    row(1, "Custom search URL", custom.upcast_ref());
    row(2, "Home page", home.upcast_ref());
    row(3, "Block ads and trackers", adblock_sw.upcast_ref());
    grid.attach(&note, 0, 4, 2, 1);
    w.add(&grid);

    let apply = {
        let (app, engine, custom, home, adblock_sw) = (app.clone(), engine.clone(), custom.clone(), home.clone(), adblock_sw.clone());
        Rc::new(move || {
            let new = Settings {
                engine: engine.active_id().map(|s| s.to_string()).unwrap_or_default(),
                custom: custom.text().trim().to_string(),
                home: home.text().trim().to_string(),
                adblock: adblock_sw.is_active(),
            };
            custom.set_sensitive(new.engine == settings::CUSTOM);
            let mut views = vec![];
            with(&app, |a| {
                if a.settings.adblock != new.adblock {
                    views = a.tabs.iter().map(|t| t.wv.clone()).collect();
                }
                a.settings = new.clone();
            });
            new.save();
            for wv in &views { adblock::apply(wv, new.adblock) }
        })
    };
    { let f = apply.clone(); engine.connect_changed(move |_| f()); }
    { let f = apply.clone(); custom.connect_changed(move |_| f()); }
    { let f = apply.clone(); home.connect_changed(move |_| f()); }
    adblock_sw.connect_active_notify(move |_| apply());
    w.connect_key_press_event(|w, e| {
        if e.keyval() == gdk::keys::constants::Escape { w.close(); Propagation::Stop } else { Propagation::Proceed }
    });
    w.show_all();
}

fn icon_button(icon: &str) -> (gtk::Button, gtk::Image) {
    let img = gtk::Image::from_icon_name(Some(icon), gtk::IconSize::Menu);
    let b = gtk::Button::new();
    b.add(&img);
    b.set_relief(gtk::ReliefStyle::None);
    (b, img)
}

fn main() {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Zolith")
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_visible(false)
        .build(&event_loop)
        .unwrap();
    let win = window.gtk_window().clone();

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
    back.set_tooltip_text(Some("Back (Alt+Left)"));
    fwd.set_tooltip_text(Some("Forward (Alt+Right)"));
    reload.set_tooltip_text(Some("Reload (Ctrl+R)"));
    home.set_tooltip_text(Some("Home (Alt+Home)"));
    star.set_tooltip_text(Some("Bookmark this page (Ctrl+D)"));
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("Search or enter address"));
    entry.style_context().add_class("urlbar");
    let menu_btn = gtk::MenuButton::new();
    menu_btn.add(&gtk::Image::from_icon_name(Some("open-menu-symbolic"), gtk::IconSize::Menu));
    menu_btn.set_relief(gtk::ReliefStyle::None);
    let pop = gtk::Popover::new(Some(&menu_btn));
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.set_border_width(6);
    menu_btn.set_popover(Some(&pop));
    for b in [&back, &fwd, &reload, &home] { toolbar.pack_start(b, false, false, 0) }
    toolbar.pack_start(&entry, true, true, 6);
    toolbar.pack_start(&star, false, false, 0);
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
    vbox.pack_start(&toolbar, false, false, 0);
    vbox.pack_start(&bm_bar, false, false, 0);
    vbox.pack_start(&overlay, true, true, 0);
    vbox.pack_start(&find_bar, false, false, 0);

    // ---- titlebar: tabs + window controls ----
    let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tabs_box.set_homogeneous(true);
    let (plus, _) = icon_button("list-add-symbolic");
    plus.style_context().add_class("newtab-btn");
    plus.set_tooltip_text(Some("New tab (Ctrl+T)"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_size_request(40, -1);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let (wmin, _) = icon_button("window-minimize-symbolic");
    let (wmax, _) = icon_button("window-maximize-symbolic");
    let (wclose, _) = icon_button("window-close-symbolic");
    for b in [&wmin, &wmax, &wclose] {
        b.style_context().add_class("winbtn");
        controls.pack_start(b, false, false, 0);
    }
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    strip.pack_start(&tabs_box, false, true, 0);
    strip.pack_start(&plus, false, false, 4);
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
        downloads_hooked: false, fullscreen: false, win: win.clone(), toolbar: toolbar.clone(),
        stack, tabs_box, entry: entry.clone(), back: back.clone(), fwd: fwd.clone(), reload_img, star_img,
        bm_bar, find_bar, find_entry: find_entry.clone(), status,
        ctx: Rc::new(RefCell::new(WebContext::new(Some(settings::data_dir())))),
    }));

    // ---- wiring ----
    let bind = |b: &gtk::Button, c: Cmd| { let a = app.clone(); b.connect_clicked(move |_| run(&a, c)); };
    bind(&back, Cmd::Back); bind(&fwd, Cmd::Forward); bind(&reload, Cmd::Reload); bind(&home, Cmd::Home);
    bind(&star, Cmd::Bookmark); bind(&plus, Cmd::NewTab);
    bind(&fprev, Cmd::FindNext(true)); bind(&fnext, Cmd::FindNext(false)); bind(&fclose, Cmd::CloseFind);
    { let a = app.clone(); entry.connect_activate(move |e| go(&a, &e.text())); }
    { let a = app.clone(); find_entry.connect_activate(move |_| run(&a, Cmd::FindNext(false))); }
    { let a = app.clone(); find_entry.connect_changed(move |_| run(&a, Cmd::FindTyped)); }
    for (label, c) in [
        ("New tab", Cmd::NewTab), ("Reopen closed tab", Cmd::Reopen), ("Bookmark this page", Cmd::Bookmark),
        ("Find in page", Cmd::Find), ("Zoom in", Cmd::Zoom(1)), ("Zoom out", Cmd::Zoom(-1)),
        ("Reset zoom", Cmd::Zoom(0)), ("Print…", Cmd::Print), ("Developer tools", Cmd::Devtools),
        ("Settings", Cmd::Settings), ("Quit", Cmd::Quit),
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
                    (false, Some('p')) => Some(Cmd::Print),
                    (true, Some('i')) => Some(Cmd::Devtools),
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
                let (finding, in_url) = a.try_borrow()
                    .map(|x| (x.find_bar.is_visible(), x.entry.has_focus())).unwrap_or((false, false));
                match key {
                    k::F5 => Some(Cmd::Reload),
                    k::F3 => Some(Cmd::FindNext(shift)),
                    k::F6 => Some(Cmd::FocusUrl),
                    k::F12 => Some(Cmd::Devtools),
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
    let start = {
        let a = app.borrow();
        let t = a.settings.search_template().to_string();
        std::env::args().nth(1).or_else(|| Some(a.settings.home.clone()).filter(|h| !h.trim().is_empty()))
            .map(|u| to_url(&u, &t))
    };
    new_tab(&app, start, true);
    titlebar.show_all();
    vbox.show_all();
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
}
