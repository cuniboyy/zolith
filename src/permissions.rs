//! Site permissions: camera, microphone, screen sharing, location and notifications. A page
//! has to ask, you answer once per site, and the answer is remembered (Settings can reset it).

use super::*;
use std::collections::HashMap;
use webkit2gtk::{
    GeolocationPermissionRequest, NotificationPermissionRequest, PermissionRequest, PermissionRequestExt,
    UserMediaPermissionRequest, UserMediaPermissionRequestExt,
};

/// (site, permission) -> allowed
pub type Saved = HashMap<(String, String), bool>;

fn path() -> PathBuf {
    settings::config_dir().join("permissions.tsv")
}

pub fn load() -> Saved {
    parse(&fs::read_to_string(path()).unwrap_or_default())
}

fn parse(text: &str) -> Saved {
    text.lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let (host, kind, answer) = (p.next()?, p.next()?, p.next()?);
            (!host.is_empty()).then(|| ((host.to_string(), kind.to_string()), answer == "allow"))
        })
        .collect()
}

fn serialize(m: &Saved) -> String {
    let mut rows: Vec<_> = m.iter().collect();
    rows.sort();
    rows.iter().map(|((h, k), a)| format!("{h}\t{k}\t{}\n", if **a { "allow" } else { "block" })).collect()
}

fn save(m: &Saved) {
    let _ = fs::create_dir_all(settings::config_dir());
    if let Err(e) = fs::write(path(), serialize(m)) {
        eprintln!("zolith: could not save site permissions: {e}");
    }
}

/// Forget every saved answer.
pub fn reset(app: &Sh) {
    with(app, |a| {
        a.perms.clear();
        save(&a.perms);
    });
}

/// What the page is asking for: (key, words for the dialog). None means "not our business".
fn asked(req: &PermissionRequest) -> Option<Vec<(&'static str, &'static str)>> {
    if let Some(m) = req.downcast_ref::<UserMediaPermissionRequest>() {
        let mut v = vec![];
        if m.is_for_audio_device() { v.push(("microphone", "Use your microphone")) }
        if m.is_for_video_device() { v.push(("camera", "Use your camera")) }
        if v.is_empty() { v.push(("screen", "Share your screen")) }
        return Some(v);
    }
    if req.is::<GeolocationPermissionRequest>() {
        return Some(vec![("location", "Know your location")]);
    }
    if req.is::<NotificationPermissionRequest>() {
        return Some(vec![("notifications", "Show notifications")]);
    }
    None
}

/// The site a page belongs to (host and port), or None for pages that can't hold permissions.
fn site_of(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("https://").or_else(|| uri.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn icon(key: &str) -> &'static str {
    match key {
        "microphone" => "audio-input-microphone-symbolic",
        "camera" => "camera-web-symbolic",
        "screen" => "video-display-symbolic",
        "location" => "mark-location-symbolic",
        _ => "preferences-system-notifications-symbolic",
    }
}

pub fn hook(app: &Sh, wv: &webkit2gtk::WebView) {
    let a = app.clone();
    wv.connect_permission_request(move |wv, req| handle(&a, wv, req));
}

fn handle(app: &Sh, wv: &webkit2gtk::WebView, req: &PermissionRequest) -> bool {
    let Some(items) = asked(req) else { return false };
    // our own pages and odd addresses never get permissions
    let Some(site) = wv.uri().and_then(|u| site_of(&u)) else {
        req.deny();
        return true;
    };
    let saved: Vec<Option<bool>> = match app.try_borrow() {
        Ok(a) => items.iter().map(|i| a.perms.get(&(site.clone(), i.0.to_string())).copied()).collect(),
        // busy: don't guess, say no
        Err(_) => { req.deny(); return true }
    };
    // any saved "block" wins; everything saved as "allow" lets it through
    if saved.iter().any(|s| *s == Some(false)) {
        req.deny();
        return true;
    }
    if saved.iter().all(|s| *s == Some(true)) {
        req.allow();
        return true;
    }
    ask(app, req.clone(), site, items);
    true
}

/// Close the prompt (if one is showing), refusing the request it was about.
pub fn dismiss(a: &mut App) {
    a.perm_bubble.hide();
    if let Some(cancel) = a.perm_pending.take() {
        cancel();
    }
}

/// A bubble under the address bar, like Chrome's: who is asking, what for, and three answers.
fn ask(app: &Sh, req: PermissionRequest, site: String, items: Vec<(&'static str, &'static str)>) {
    let Ok(mut a) = app.try_borrow_mut() else { return req.deny() };
    // one question at a time: an older one still open is refused
    dismiss(&mut a);
    let bubble = a.perm_bubble.clone();
    for c in bubble.children() { bubble.remove(&c) }
    bubble.set_border_width(16);

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(Some(&format!("{site} wants to")));
    title.set_xalign(0.0);
    title.set_line_wrap(true);
    title.set_max_width_chars(34);
    title.style_context().add_class("perm-title");
    head.pack_start(&title, true, true, 0);
    let close = gtk::Button::from_icon_name(Some("window-close-symbolic"), gtk::IconSize::Menu);
    close.set_relief(gtk::ReliefStyle::None);
    close.set_tooltip_text(Some("Close"));
    head.pack_end(&close, false, false, 0);
    bubble.pack_start(&head, false, false, 0);

    for (key, phrase) in &items {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.pack_start(&gtk::Image::from_icon_name(Some(icon(key)), gtk::IconSize::Menu), false, false, 0);
        let l = gtk::Label::new(Some(phrase));
        l.set_xalign(0.0);
        row.pack_start(&l, true, true, 0);
        row.set_margin_top(10);
        bubble.pack_start(&row, false, false, 0);
    }

    // answered once, whichever way: a button, the close button, or the page going away
    let req = Rc::new(RefCell::new(Some(req)));
    let items = Rc::new(items);
    let answer = {
        let (app, req, site, items, bubble) = (app.clone(), req.clone(), site.clone(), items.clone(), bubble.clone());
        Rc::new(move |allow: bool, remember: bool| {
            let Some(r) = req.borrow_mut().take() else { return };
            if allow { r.allow() } else { r.deny() }
            bubble.hide();
            with(&app, |a| {
                a.perm_pending = None;
                if remember && !private() {
                    for i in items.iter() {
                        a.perms.insert((site.clone(), i.0.to_string()), allow);
                    }
                    save(&a.perms);
                }
            });
        })
    };
    let buttons = [("Allow while visiting the site", true, true), ("Allow this time", true, false), ("Don't allow", false, true)];
    for (i, (label, allow, remember)) in buttons.into_iter().enumerate() {
        let b = gtk::Button::with_label(label);
        b.style_context().add_class("perm-btn");
        b.set_margin_top(if i == 0 { 14 } else { 6 });
        // in a private window nothing is saved, so "while visiting" is just "this time"
        if private() && i == 0 {
            continue;
        }
        if allow {
            // ignore stray clicks and key presses in the first moment
            b.set_sensitive(false);
            let b2 = b.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || b2.set_sensitive(true));
        }
        let answer = answer.clone();
        b.connect_clicked(move |_| answer(allow, remember));
        bubble.pack_start(&b, false, false, 0);
    }
    let a2 = answer.clone();
    close.connect_clicked(move |_| a2(false, false));
    // if the tab changes or Zolith moves on, the question is refused and not remembered
    a.perm_pending = Some(Box::new(move || answer(false, false)));

    let (x, y) = a.entry.translate_coordinates(&a.top, 0, 0).unwrap_or((0, 0));
    bubble.set_margin_start(x.max(0) + 8);
    bubble.set_margin_top((y + a.entry.allocated_height() + 6).max(0));
    bubble.show_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites() {
        assert_eq!(site_of("https://Meet.Example.com/room?x=1"), Some("meet.example.com".into()));
        assert_eq!(site_of("http://localhost:3000/"), Some("localhost:3000".into()));
        for bad in ["zolith://settings/", "file:///etc/passwd", "about:blank", "", "https://"] {
            assert_eq!(site_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn saved_answers_round_trip() {
        let mut m = Saved::new();
        m.insert(("a.example".into(), "camera".into()), true);
        m.insert(("b.example".into(), "location".into()), false);
        assert_eq!(parse(&serialize(&m)), m);
        // junk lines are ignored, and an unknown answer counts as "block"
        let p = parse("nonsense\n\ta\tallow\nc.example\tmicrophone\tmaybe\n");
        assert_eq!(p.len(), 1);
        assert_eq!(p[&("c.example".to_string(), "microphone".to_string())], false);
    }
}
