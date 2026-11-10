//! Icons on the new tab page's most-visited tiles. The icon comes from the favicon Zolith
//! saved when the page was visited; for sites it has not seen (an imported history, say) the
//! site's own `/favicon.ico` is fetched once and cached.

use super::*;
use soup::prelude::*;
use webkit2gtk::{FaviconDatabase, FaviconDatabaseExt};

/// A fetched icon is kept this long before the site is asked again (also for "has none").
const RECHECK_SECS: u64 = 14 * 86_400;
const MAX_ICON_BYTES: usize = 128 * 1024;

fn cache_file(host: &str) -> PathBuf {
    let safe: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    settings::data_dir().join("tileicons").join(safe)
}

/// The image type of `bytes` if it is one browsers can show in a page.
fn sniff(b: &[u8]) -> Option<&'static str> {
    match b {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0, 0, 1, 0, ..] => Some("image/x-icon"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// (host, origin) of a web address.
fn site(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // a real host has a letter or digit (never just dots, which would name a folder, not a file)
    let host = rest.split(['/', '?', '#']).next().filter(|h| h.chars().any(|c| c.is_ascii_alphanumeric()))?;
    Some((host.to_ascii_lowercase(), format!("{scheme}://{host}")))
}

fn show(wv: &webkit2gtk::WebView, url: &str, mime: &str, bytes: &[u8]) {
    let src = format!("data:{mime};base64,{}", glib::base64_encode(bytes));
    run_js(wv, &format!("window.zolithIcon && zolithIcon({}, {})", quote(url), quote(&src)));
}

fn png_of(surface: gtk::cairo::Surface) -> Option<Vec<u8>> {
    let img = gtk::cairo::ImageSurface::try_from(surface.clone()).ok()?;
    let pixbuf = gdk::pixbuf_get_from_surface(&surface, 0, 0, img.width(), img.height())?;
    pixbuf.save_to_bufferv("png", &[]).ok()
}

/// Put an icon on every tile of the new tab page in `wv`.
pub fn fill(app: &Sh, wv: &webkit2gtk::WebView) {
    if private() {
        return;
    }
    let Ok(a) = app.try_borrow() else { return };
    let urls: Vec<String> = suggest::top_sites(&a.history, &a.marks, 8, suggest::now()).into_iter().map(|t| t.0).collect();
    let http = a.http.clone();
    drop(a);
    let db = wv.context().and_then(|c| c.favicon_database());
    for url in urls {
        from_database(wv.clone(), http.clone(), db.clone(), url);
    }
}

fn from_database(wv: webkit2gtk::WebView, http: soup::Session, db: Option<FaviconDatabase>, url: String) {
    let Some(db) = db else { return from_site(wv, http, url) };
    let u = url.clone();
    db.favicon(&u, None::<&gio::Cancellable>, move |r| match r.ok().and_then(png_of) {
        Some(png) => show(&wv, &url, "image/png", &png),
        None => from_site(wv, http, url),
    });
}

/// The site's own favicon.ico, cached on disk.
fn from_site(wv: webkit2gtk::WebView, http: soup::Session, url: String) {
    let Some((host, origin)) = site(&url) else { return };
    let cache = cache_file(&host);
    if let Ok(meta) = fs::metadata(&cache) {
        let fresh = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age.as_secs() < RECHECK_SECS);
        if fresh {
            // an empty file means "this site has no icon", remembered so we don't ask again
            if let Ok(bytes) = fs::read(&cache) {
                if let Some(mime) = sniff(&bytes) {
                    show(&wv, &url, mime, &bytes);
                }
            }
            return;
        }
    }
    let Ok(msg) = soup::Message::new("GET", &format!("{origin}/favicon.ico")) else { return };
    // some sites (Wikipedia, for one) refuse requests that don't say who is asking
    if let Some(h) = msg.request_headers() {
        h.append("User-Agent", concat!("Zolith/", env!("CARGO_PKG_VERSION"), " (favicon fetch)"));
    }
    let fut = http.send_and_read_future(&msg, glib::Priority::LOW);
    glib::MainContext::default().spawn_local(async move {
        let bytes = fut.await.ok().filter(|b| b.len() <= MAX_ICON_BYTES).map(|b| b.to_vec());
        let icon = bytes.filter(|b| sniff(b).is_some());
        let _ = fs::create_dir_all(cache.parent().unwrap_or(Path::new(".")));
        let _ = fs::write(&cache, icon.as_deref().unwrap_or(&[]));
        if let Some(b) = icon {
            if let Some(mime) = sniff(&b) {
                show(&wv, &url, mime, &b);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_types_are_recognised_by_content() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(&[0, 0, 1, 0, 1, 0]), Some("image/x-icon"));
        assert_eq!(sniff(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        // an HTML error page, an SVG (can run script) or nothing at all are not icons
        for bad in [&b"<!doctype html>"[..], b"<svg xmlns=", b"", b"\x89PN"] {
            assert_eq!(sniff(bad), None);
        }
    }

    #[test]
    fn sites_and_cache_names() {
        assert_eq!(site("https://www.YouTube.com/watch?v=1"), Some(("www.youtube.com".into(), "https://www.YouTube.com".into())));
        assert_eq!(site("http://localhost:3000/x"), Some(("localhost:3000".into(), "http://localhost:3000".into())));
        for bad in ["zolith://settings/", "file:///etc/passwd", "https://", "", "https://../x", "http://./"] {
            assert_eq!(site(bad), None, "{bad}");
        }
        // a host can't escape the cache folder
        let p = cache_file("../../etc/passwd");
        assert_eq!(p.parent(), Some(settings::data_dir().join("tileicons").as_path()), "{p:?}");
    }
}
