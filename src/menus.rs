//! Right-click menus: the page (links, images, text, editing) and tabs.

use super::*;
use javascriptcore::ValueExt;
use webkit2gtk::{BackForwardListExt, BackForwardListItemExt, HitTestResult};

fn item(menu: &gtk::Menu, label: &str, enabled: bool, f: impl Fn() + 'static) {
    let mi = gtk::MenuItem::with_label(label);
    mi.set_sensitive(enabled);
    mi.connect_activate(move |_| f());
    menu.append(&mi);
}

fn separator(menu: &gtk::Menu) {
    if !menu.children().is_empty() {
        menu.append(&gtk::SeparatorMenuItem::new());
    }
}

fn show(menu: gtk::Menu, ev: Option<&gdk::Event>) {
    menu.show_all();
    menu.popup_at_pointer(ev);
}

fn short(s: &str, n: usize) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > n { one_line.chars().take(n).chain("…".chars()).collect() } else { one_line }
}

/// Right-click on a tab.
pub fn tab_menu(app: &Sh, id: u32, ev: &gdk::EventButton) {
    let Ok(a) = app.try_borrow() else { return };
    let Some(i) = a.tabs.iter().position(|t| t.id == id) else { return };
    let (pinned, url, count, can_reopen) = (a.tabs[i].pinned, a.tabs[i].url.clone(), a.tabs.len(), !a.closed.is_empty());
    let others: Vec<u32> = a.tabs.iter().filter(|t| t.id != id && !t.pinned).map(|t| t.id).collect();
    let right: Vec<u32> = a.tabs.iter().skip(i + 1).filter(|t| !t.pinned).map(|t| t.id).collect();
    let wv = a.tabs[i].wv.clone();
    drop(a);

    let menu = gtk::Menu::new();
    let ap = app.clone();
    item(&menu, "New tab", true, move || run(&ap, Cmd::NewTab));
    let wv2 = wv.clone();
    item(&menu, "Reload", !url.is_empty(), move || wv2.reload());
    let (ap, u) = (app.clone(), url.clone());
    item(&menu, "Duplicate", !url.is_empty() && internal(&url).is_none(), move || { new_tab(&ap, Some(u.clone()), true); });
    let ap = app.clone();
    item(&menu, if pinned { "Unpin tab" } else { "Pin tab" }, true, move || pin_tab(&ap, id, !pinned));
    separator(&menu);
    let ap = app.clone();
    item(&menu, "Close tab", true, move || close_tab(&ap, id));
    let ap = app.clone();
    item(&menu, "Close other tabs", count > 1 && !others.is_empty(), move || {
        for t in &others { close_tab(&ap, *t) }
    });
    let ap = app.clone();
    item(&menu, "Close tabs to the right", !right.is_empty(), move || {
        for t in &right { close_tab(&ap, *t) }
    });
    separator(&menu);
    let ap = app.clone();
    item(&menu, "Reopen closed tab", can_reopen, move || run(&ap, Cmd::Reopen));
    show(menu, Some(ev));
}

/// Right-click on a page: what is offered depends on what was clicked.
pub fn page_menu(app: &Sh, wv: &webkit2gtk::WebView, hit: &HitTestResult, ev: &gdk::Event) {
    let link = hit.link_uri().map(|u| u.to_string()).filter(|_| hit.context_is_link());
    let image = hit.image_uri().map(|u| u.to_string()).filter(|_| hit.context_is_image());
    let (editable, selection) = (hit.context_is_editable(), hit.context_is_selection());
    let (app, wv, ev) = (app.clone(), wv.clone(), ev.clone());
    if selection {
        // the selected words are needed for the "Search for ..." entry
        let (app2, wv2) = (app.clone(), wv.clone());
        wv.evaluate_javascript("window.getSelection().toString()", None, None, None::<&gio::Cancellable>, move |r| {
            let text = r.ok().filter(|v| v.is_string()).map(|v| v.to_str().to_string()).unwrap_or_default();
            build(&app2, &wv2, link, image, editable, Some(text), &ev);
        });
    } else {
        build(&app, &wv, link, image, editable, None, &ev);
    }
}

fn build(app: &Sh, wv: &webkit2gtk::WebView, link: Option<String>, image: Option<String>, editable: bool, selection: Option<String>, ev: &gdk::Event) {
    let menu = gtk::Menu::new();
    if let Some(u) = link {
        let ap = app.clone();
        let u1 = u.clone();
        item(&menu, "Open link in new tab", true, move || { new_tab(&ap, Some(u1.clone()), false); });
        item(&menu, "Copy link address", true, move || copy_text(&u));
        separator(&menu);
    }
    if let Some(u) = image {
        let ap = app.clone();
        let u1 = u.clone();
        item(&menu, "Open image in new tab", true, move || { new_tab(&ap, Some(u1.clone()), false); });
        let wv1 = wv.clone();
        let u2 = u.clone();
        item(&menu, "Save image as…", true, move || { wv1.download_uri(&u2); });
        item(&menu, "Copy image address", true, move || copy_text(&u));
        separator(&menu);
    }
    if editable {
        for (label, cmd) in [("Cut", "Cut"), ("Copy", "Copy"), ("Paste", "Paste"), ("Select all", "SelectAll")] {
            let wv1 = wv.clone();
            item(&menu, label, true, move || wv1.execute_editing_command(cmd));
        }
        separator(&menu);
    } else if let Some(text) = selection.as_ref().filter(|t| !t.trim().is_empty()) {
        let wv1 = wv.clone();
        item(&menu, "Copy", true, move || wv1.execute_editing_command("Copy"));
        let (ap, q) = (app.clone(), text.clone());
        let engine = app.try_borrow().map(|a| a.settings.engine.clone()).unwrap_or_default();
        item(&menu, &format!("Search {engine} for “{}”", short(text, 24)), true, move || {
            let template = ap.try_borrow().map(|a| a.settings.search_template().to_string()).unwrap_or_default();
            new_tab(&ap, Some(template.replacen("%s", &settings::encode_query(q.trim()), 1)), false);
        });
        separator(&menu);
    } else if !editable {
        let wv1 = wv.clone();
        item(&menu, "Select all", true, move || wv1.execute_editing_command("SelectAll"));
        separator(&menu);
    }
    // always: page navigation and tools
    let wv1 = wv.clone();
    item(&menu, "Back", wv.can_go_back(), move || wv1.go_back());
    let wv1 = wv.clone();
    item(&menu, "Forward", wv.can_go_forward(), move || wv1.go_forward());
    let wv1 = wv.clone();
    item(&menu, "Reload", true, move || wv1.reload());
    separator(&menu);
    let ap = app.clone();
    item(&menu, "Bookmark this page", true, move || run(&ap, Cmd::Bookmark));
    let wv1 = wv.clone();
    item(&menu, "View page source", wv.uri().is_some_and(|u| u.starts_with("http")), move || {
        if let Some(u) = wv1.uri() { wv1.load_uri(&format!("view-source:{u}")) }
    });
    let wv1 = wv.clone();
    item(&menu, "Inspect", true, move || if let Some(i) = wv1.inspector() { i.show() });
    show(menu, Some(ev));
}

pub fn copy_text(text: &str) {
    if let Some(display) = gdk::Display::default() {
        if let Some(cb) = gtk::Clipboard::default(&display) {
            cb.set_text(text);
        }
    }
}

/// The pages Back (or Forward) can jump to, nearest first, with their labels.
pub fn nav_pages(app: &Sh, back: bool) -> Vec<(String, webkit2gtk::BackForwardListItem)> {
    let Some(wv) = cur_wv(app) else { return vec![] };
    let Some(list) = wv.back_forward_list() else { return vec![] };
    let pages = if back { list.back_list_with_limit(15) } else { list.forward_list_with_limit(15) };
    pages
        .into_iter()
        .map(|page| {
            let title = page.title().map(|t| t.to_string()).filter(|t| !t.is_empty());
            let label = title.or_else(|| page.uri().map(|u| u.to_string())).unwrap_or_else(|| "Page".into());
            (short(&label, 48), page)
        })
        .collect()
}

/// Right-click on Back or Forward.
pub fn nav_menu(app: &Sh, back: bool, ev: &gdk::EventButton) {
    let (Some(wv), pages) = (cur_wv(app), nav_pages(app, back)) else { return };
    if pages.is_empty() {
        return;
    }
    let menu = gtk::Menu::new();
    for (label, page) in pages {
        let wv = wv.clone();
        item(&menu, &label, true, move || wv.go_to_back_forward_list_item(&page));
    }
    show(menu, Some(ev));
}
