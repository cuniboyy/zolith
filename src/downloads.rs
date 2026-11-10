//! The downloads panel: a toolbar button with a list of downloads, their progress, and
//! buttons to cancel, open, or show the file in its folder.

use super::*;
use webkit2gtk::{Download, DownloadExt};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Active,
    Done,
    Failed,
    Cancelled,
}

pub struct Dl {
    pub id: u32,
    pub name: String,
    pub path: Option<PathBuf>,
    pub state: State,
    pub progress: f64,
    dl: Download,
}

/// Start tracking every download the engine starts in this context.
pub fn hook(app: &Sh, ctx: &webkit2gtk::WebContext) {
    let a = app.clone();
    ctx.connect_download_started(move |_, d| track(&a, d));
}

fn name_of(uri: &str) -> String {
    let last = uri.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("");
    if last.is_empty() { "Download".into() } else { last.to_string() }
}

fn set(app: &Sh, id: u32, f: impl FnOnce(&mut Dl)) {
    with(app, |a| {
        if let Some(d) = a.dls.iter_mut().find(|d| d.id == id) {
            f(d);
        }
    });
    schedule_render(app);
}

fn track(app: &Sh, d: &Download) {
    let id = {
        let Ok(mut a) = app.try_borrow_mut() else { return };
        a.next_dl += 1;
        let id = a.next_dl;
        let name = d.request().and_then(|r| r.uri()).map(|u| name_of(&u)).unwrap_or_else(|| "Download".into());
        a.dls.insert(0, Dl { id, name, path: None, state: State::Active, progress: 0.0, dl: d.clone() });
        id
    };
    let a = app.clone();
    d.connect_created_destination(move |_, dest| {
        // the engine reports a file:// URI
        let Some(p) = gio::File::for_uri(dest).path() else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
        set(&a, id, |x| {
            x.path = Some(p);
            if let Some(n) = name { x.name = n }
        });
    });
    let a = app.clone();
    d.connect_received_data(move |d, _| {
        let p = d.estimated_progress();
        set(&a, id, |x| x.progress = p);
    });
    let a = app.clone();
    d.connect_failed(move |_, _| set(&a, id, |x| if x.state == State::Active { x.state = State::Failed }));
    let a = app.clone();
    d.connect_finished(move |_| set(&a, id, |x| {
        if x.state == State::Active { x.state = State::Done; x.progress = 1.0 }
    }));
    schedule_render(app);
}

/// Redraw the panel soon, once, however many progress updates arrive.
fn schedule_render(app: &Sh) {
    let Ok(mut a) = app.try_borrow_mut() else { return };
    if a.dl_pending {
        return;
    }
    a.dl_pending = true;
    let app = app.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
        with(&app, |a| a.dl_pending = false);
        render(&app);
    });
}

fn open_path(p: &Path) {
    let uri = gio::File::for_path(p).uri();
    let _ = gio::AppInfo::launch_default_for_uri(uri.as_str(), None::<&gio::AppLaunchContext>);
}

fn status_text(d: &Dl) -> String {
    match d.state {
        State::Active if d.progress > 0.0 => format!("Downloading… {}%", (d.progress * 100.0) as u32),
        State::Active => "Starting…".into(),
        State::Done => d.path.as_ref().and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_else(|| "Done".into()),
        State::Failed => "Failed".into(),
        State::Cancelled => "Cancelled".into(),
    }
}

pub fn render(app: &Sh) {
    let Ok(a) = app.try_borrow() else { return };
    let (list, btn) = (a.dl_list.clone(), a.dl_btn.clone());
    let items: Vec<(u32, String, Option<PathBuf>, State, f64, String)> =
        a.dls.iter().map(|d| (d.id, d.name.clone(), d.path.clone(), d.state, d.progress, status_text(d))).collect();
    drop(a);

    btn.set_visible(!items.is_empty());
    let busy = items.iter().any(|i| i.3 == State::Active);
    if busy { btn.style_context().add_class("busy") } else { btn.style_context().remove_class("busy") }
    for c in list.children() { list.remove(&c) }
    for (id, name, path, state, progress, status) in items {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row.set_border_width(8);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title = gtk::Label::new(Some(&name));
        title.set_xalign(0.0);
        title.set_ellipsize(pango::EllipsizeMode::Middle);
        title.set_max_width_chars(34);
        top.pack_start(&title, true, true, 0);
        let mk = |icon: &str, tip: &str| {
            let b = gtk::Button::from_icon_name(Some(icon), gtk::IconSize::Menu);
            b.set_relief(gtk::ReliefStyle::None);
            b.set_tooltip_text(Some(tip));
            b
        };
        match (state, path) {
            (State::Active, _) => {
                let b = mk("window-close-symbolic", "Cancel");
                let a = app.clone();
                b.connect_clicked(move |_| {
                    set(&a, id, |x| { x.state = State::Cancelled; x.dl.cancel() });
                });
                top.pack_start(&b, false, false, 0);
            }
            (State::Done, Some(p)) => {
                let (open, folder) = (mk("document-open-symbolic", "Open file"), mk("folder-open-symbolic", "Show in folder"));
                let p2 = p.clone();
                open.connect_clicked(move |_| open_path(&p2));
                folder.connect_clicked(move |_| if let Some(dir) = p.parent() { open_path(dir) });
                top.pack_start(&open, false, false, 0);
                top.pack_start(&folder, false, false, 0);
            }
            _ => {}
        }
        row.pack_start(&top, false, false, 0);
        if state == State::Active {
            let bar = gtk::ProgressBar::new();
            bar.set_fraction(progress.clamp(0.0, 1.0));
            row.pack_start(&bar, false, false, 0);
        }
        let st = gtk::Label::new(Some(&status));
        st.set_xalign(0.0);
        st.set_ellipsize(pango::EllipsizeMode::Start);
        st.set_max_width_chars(44);
        st.style_context().add_class("dim-label");
        row.pack_start(&st, false, false, 0);
        list.pack_start(&row, false, false, 0);
    }
    list.show_all();
}

/// Forget finished downloads (the files stay where they are).
pub fn clear(app: &Sh) {
    with(app, |a| a.dls.retain(|d| d.state == State::Active));
    render(app);
}
