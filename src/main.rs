use gtk::{gdk, glib, glib::Propagation, pango, prelude::*};
use std::{cell::RefCell, fs, path::PathBuf, rc::Rc};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    platform::unix::WindowExtUnix,
    window::WindowBuilder,
};
use wry::{NewWindowResponse, PageLoadEvent, WebContext, WebView,WebViewBuilder, WebViewBuilderExtUnix};

const CSS: &str = include_str!("style.css");
const NEWTAB: &str = include_str!("newtab.html");

// shortcut: no history/downloads/favicons/private mode; back/forward buttons never grey out
// (wry exposes no can_go_back). Add when needed.

#[derive(Clone, Copy)]
enum Cmd {
    NewTab, CloseTab, Reopen, Reload, Back, Forward, Home, FocusUrl,
    Find, FindNext(bool), FindTyped, CloseFind, Zoom(i8), Print, Devtools, Bookmark, NextTab(i32), Quit,
}

struct Tab {
    id: u32,
    page: gtk::Box,
    head: gtk::EventBox,
    label: gtk::Label,
    view: WebView,
    url: String,
    title: String,
    loading: bool,
    zoom: f64,
}

struct App {
    next_id: u32,
    cur: u32,
    tabs: Vec<Tab>,
    closed: Vec<String>,
    marks: Vec<(String, String)>,
    stack: gtk::Stack,
    tabs_box: gtk::Box,
    entry: gtk::Entry,
    reload_img: gtk::Image,
    star_img: gtk::Image,
    bm_bar: gtk::Box,
    find_bar: gtk::Box,
    find_entry: gtk::Entry,
    ctx: Rc<RefCell<WebContext>>,
}

type Sh = Rc<RefCell<App>>;

// Handlers fire from inside webkit/gtk callbacks; skip instead of panicking on re-entry.
fn with(app: &Sh, f: impl FnOnce(&mut App)) {
    if let Ok(mut a) = app.try_borrow_mut() {
        f(&mut a);
    }
}

fn to_url(input: &str) -> String {
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
        format!("https://duckduckgo.com/?q={q}")
    }
}

fn js_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', ""))
}

fn marks_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/browser-rs/bookmarks.tsv")
}

fn load_marks() -> Vec<(String, String)> {
    fs::read_to_string(marks_path()).unwrap_or_default().lines()
        .filter_map(|l| l.split_once('\t').map(|(u, t)| (u.into(), t.into()))).collect()
}

fn save_marks(m: &[(String, String)]) {
    let p = marks_path();
    let _ = fs::create_dir_all(p.parent().unwrap());
    let _ = fs::write(p, m.iter().map(|(u, t)| format!("{u}\t{}\n", t.replace(['\t', '\n'], " "))).collect::<String>());
}

impl App {
    fn cur(&self) -> &Tab {
        self.tabs.iter().find(|t| t.id == self.cur).expect("current tab exists")
    }

    fn refresh(&self) {
        let t = self.cur();
        if !self.entry.has_focus() {
            self.entry.set_text(t.url.strip_prefix("https://").unwrap_or(&t.url));
        }
        let icon = if t.url.starts_with("https://") { Some("channel-secure-symbolic") }
            else if t.url.starts_with("http://") { Some("channel-insecure-symbolic") }
            else if t.url.is_empty() { Some("system-search-symbolic") } else { None };
        self.entry.set_icon_from_icon_name(gtk::EntryIconPosition::Primary, icon);
        self.reload_img.set_from_icon_name(
            Some(if t.loading { "process-stop-symbolic" } else { "view-refresh-symbolic" }), gtk::IconSize::Menu);
        let marked = self.marks.iter().any(|m| m.0 == t.url);
        self.star_img.set_from_icon_name(
            Some(if marked { "starred-symbolic" } else { "non-starred-symbolic" }), gtk::IconSize::Menu);
    }

    fn select(&mut self, id: u32) {
        self.cur = id;
        self.stack.set_visible_child_name(&id.to_string());
        for t in &self.tabs {
            let ctx = t.head.style_context();
            if t.id == id { ctx.add_class("selected") } else { ctx.remove_class("selected") }
        }
        self.refresh();
        let t = self.cur();
        if t.url.is_empty() { self.entry.grab_focus() } else { let _ = t.view.focus(); }
    }

    fn page_event(&mut self, id: u32, url: String, loading: bool) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        t.loading = loading;
        t.url = if url.starts_with("about:") || url.starts_with("data:") { String::new() } else { url };
        if id == self.cur { self.refresh() }
    }

    fn set_title(&mut self, id: u32, title: &str) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else { return };
        t.title = title.to_string();
        let shown = if title.is_empty() { if t.url.is_empty() { "New Tab" } else { &t.url } } else { title };
        t.label.set_text(shown);
        t.label.set_tooltip_text(Some(shown));
    }

    fn act(&mut self, c: Cmd) {
        let id = self.cur;
        let t = self.tabs.iter_mut().find(|t| t.id == id).expect("current tab exists");
        let _ = match c {
            Cmd::Back => t.view.evaluate_script("history.back()"),
            Cmd::Forward => t.view.evaluate_script("history.forward()"),
            Cmd::Reload if t.loading => { t.loading = false; t.view.evaluate_script("window.stop()") }
            Cmd::Reload => t.view.reload(),
            Cmd::Home => t.view.load_html(NEWTAB),
            Cmd::Zoom(d) => {
                t.zoom = if d == 0 { 1.0 } else { ((t.zoom * 10.0).round() + d as f64).clamp(3.0, 30.0) / 10.0 };
                t.view.zoom(t.zoom)
            }
            Cmd::Print => t.view.print(),
            Cmd::Devtools => { t.view.open_devtools(); Ok(()) }
            Cmd::FindNext(_) | Cmd::FindTyped => {
                let q = self.find_entry.text();
                if q.is_empty() { return }
                let back = matches!(c, Cmd::FindNext(true));
                // while typing, restart from the current match so it doesn't skip ahead
                let pre = if matches!(c, Cmd::FindTyped) {
                    "if(getSelection().rangeCount)getSelection().collapseToStart();"
                } else { "" };
                t.view.evaluate_script(&format!("{pre}window.find({},false,{back},true)", js_str(&q)))
            }
            _ => Ok(()),
        };
        if matches!(c, Cmd::Reload) { self.refresh() }
    }
}

fn run(app: &Sh, c: Cmd) {
    match c {
        Cmd::NewTab => new_tab(app, None),
        Cmd::Reopen => {
            let u = app.try_borrow_mut().ok().and_then(|mut a| a.closed.pop());
            if let Some(u) = u { new_tab(app, Some(u)) }
        }
        Cmd::CloseTab => {
            let id = app.borrow().cur;
            close_tab(app, id)
        }
        Cmd::Quit => std::process::exit(0),
        Cmd::Bookmark => toggle_bookmark(app),
        Cmd::FocusUrl => with(app, |a| { a.entry.grab_focus(); a.entry.select_region(0, -1) }),
        Cmd::Find => with(app, |a| { a.find_bar.show(); a.find_entry.grab_focus() }),
        Cmd::CloseFind => with(app, |a| {
            a.find_bar.hide();
            let t = a.cur();
            let _ = t.view.evaluate_script("window.getSelection().removeAllRanges()");
            let _ = t.view.focus();
        }),
        Cmd::NextTab(d) => with(app, |a| {
            let i = a.tabs.iter().position(|t| t.id == a.cur).unwrap_or(0) as i32;
            let id = a.tabs[(i + d).rem_euclid(a.tabs.len() as i32) as usize].id;
            a.select(id)
        }),
        _ => with(app, |a| a.act(c)),
    }
}

fn go(app: &Sh, input: &str) {
    if input.trim().is_empty() { return }
    with(app, |a| {
        let t = a.cur();
        let _ = t.view.load_url(&to_url(input));
        let _ = t.view.focus();
    });
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
    let (bar, marks) = { let a = app.borrow(); (a.bm_bar.clone(), a.marks.clone()) };
    for c in bar.children() { bar.remove(&c) }
    bar.set_visible(!marks.is_empty());
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
}

fn new_tab(app: &Sh, url: Option<String>) {
    let (id, stack, tabs_box, entry, ctx) = {
        let mut a = app.borrow_mut();
        a.next_id += 1;
        (a.next_id, a.stack.clone(), a.tabs_box.clone(), a.entry.clone(), a.ctx.clone())
    };
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.add_named(&page, &id.to_string());
    page.show();

    let (a1, a2, a3) = (app.clone(), app.clone(), app.clone());
    // One shared persistent context: cookies/logins survive restarts and are shared by tabs.
    let mut ctx = ctx.borrow_mut();
    let b = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_devtools(true)
        .with_on_page_load_handler(move |ev, u| {
            let loading = matches!(ev, PageLoadEvent::Started);
            with(&a1, |a| a.page_event(id, u, loading));
        })
        .with_document_title_changed_handler(move |t| with(&a2, |a| a.set_title(id, &t)))
        .with_new_window_req_handler(move |u, _| {
            let a = a3.clone();
            glib::idle_add_local_once(move || new_tab(&a, Some(u)));
            NewWindowResponse::Deny
        });
    let b = match &url { Some(u) => b.with_url(u), None => b.with_html(NEWTAB) };
    let view = b.build_gtk(&page).expect("webview");
    drop(ctx);

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
            1 => with(&a, |x| x.select(id)),
            2 => close_tab(&a, id),
            _ => {}
        }
        Propagation::Stop
    });
    head.show_all();
    tabs_box.pack_start(&head, true, true, 0);

    {
        let mut a = app.borrow_mut();
        a.tabs.push(Tab { id, page, head, label, view, url: String::new(), title: String::new(), loading: false, zoom: 1.0 });
        a.select(id);
    }
    if url.is_none() { entry.grab_focus() }
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

    // ---- bookmarks bar, content stack, find bar ----
    let bm_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    bm_bar.style_context().add_class("bookmarks");
    bm_bar.set_no_show_all(true);
    let stack = gtk::Stack::new();
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
    vbox.pack_start(&stack, true, true, 0);
    vbox.pack_start(&find_bar, false, false, 0);

    // ---- titlebar: tabs + window controls ----
    let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tabs_box.set_homogeneous(true);
    let (plus, _) = icon_button("list-add-symbolic");
    plus.style_context().add_class("newtab-btn");
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
    let app: Sh = Rc::new(RefCell::new(App {
        next_id: 0, cur: 0, tabs: vec![], closed: vec![], marks: load_marks(),
        stack, tabs_box, entry: entry.clone(), reload_img, star_img, bm_bar, find_bar, find_entry: find_entry.clone(),
        ctx: Rc::new(RefCell::new(WebContext::new(Some(
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/zolith"),
        )))),
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
        ("Reset zoom", Cmd::Zoom(0)), ("Print…", Cmd::Print), ("Developer tools", Cmd::Devtools), ("Quit", Cmd::Quit),
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
            let cmd = if ctrl {
                match (shift, ch) {
                    (_, Some('+' | '=')) => Some(Cmd::Zoom(1)),
                    (_, Some('-' | '_')) => Some(Cmd::Zoom(-1)),
                    (_, Some('0')) => Some(Cmd::Zoom(0)),
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
            } else if alt {
                match key {
                    k::Left => Some(Cmd::Back),
                    k::Right => Some(Cmd::Forward),
                    k::Home => Some(Cmd::Home),
                    _ => None,
                }
            } else {
                match key {
                    k::F5 => Some(Cmd::Reload),
                    k::F3 => Some(Cmd::FindNext(false)),
                    k::F6 => Some(Cmd::FocusUrl),
                    k::F12 => Some(Cmd::Devtools),
                    k::Escape if a.try_borrow().map(|x| x.find_bar.is_visible()).unwrap_or(false) => Some(Cmd::CloseFind),
                    _ => None,
                }
            };
            match cmd {
                Some(c) => { run(&a, c); Propagation::Stop }
                None => Propagation::Proceed,
            }
        });
    }

    rebuild_bookmarks(&app);
    new_tab(&app, std::env::args().nth(1).map(|u| to_url(&u)));
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
    use super::to_url;

    #[test]
    fn urls() {
        assert_eq!(to_url("example.com"), "https://example.com");
        assert_eq!(to_url("localhost:3000"), "http://localhost:3000");
        assert_eq!(to_url("192.168.1.5:8080/x"), "http://192.168.1.5:8080/x");
        assert_eq!(to_url("https://a.b/c d"), "https://a.b/c d");
        assert_eq!(to_url("rust lifetimes"), "https://duckduckgo.com/?q=rust+lifetimes");
        assert_eq!(to_url("what is 1.5?"), "https://duckduckgo.com/?q=what+is+1.5%3F");
    }
}
