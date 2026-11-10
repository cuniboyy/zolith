#!/usr/bin/env python3
"""Import bookmarks, history and cookies from Firefox into Zolith.

Reads a COPY of Firefox's files (Firefox itself is never modified) and merges the data into
Zolith's, keeping what Zolith already has. Zolith's files are backed up first.

Saved passwords are NOT imported: Firefox encrypts them, and Zolith has no password manager.

Usage:  scripts/import-firefox.py [--profile DIR] [--dry-run] [--no-cookies]
Close Zolith (and ideally Firefox) before running it.
"""
import argparse, glob, os, shutil, sqlite3, subprocess, sys, tempfile, time

MAX_HISTORY = 3000  # same limit Zolith keeps
# result pages of Zolith's built-in search engines are not remembered by Zolith either
SEARCH_BASES = ["https://duckduckgo.com/", "https://www.google.com/search", "https://www.bing.com/search",
                "https://search.brave.com/search", "https://www.startpage.com/sp/search"]


def find_profile():
    roots = ["~/.config/mozilla/firefox", "~/.mozilla/firefox", "~/.var/app/org.mozilla.firefox/.mozilla/firefox",
             "~/snap/firefox/common/.mozilla/firefox"]
    found = []
    for r in roots:
        found += glob.glob(os.path.join(os.path.expanduser(r), "*", "places.sqlite"))
    if not found:
        sys.exit("No Firefox profile found. Pass one with --profile.")
    return os.path.dirname(max(found, key=os.path.getmtime))


def is_web(url):
    return url.startswith("http://") or url.startswith("https://")


def is_search_page(url):
    base, _, query = url.partition("?")
    return bool(query) and base in SEARCH_BASES


def clean(s):
    return (s or "").replace("\t", " ").replace("\n", " ").replace("\r", " ")


def copy_db(profile, name, tmp):
    src = os.path.join(profile, name)
    if not os.path.exists(src):
        return None
    dst = os.path.join(tmp, name)
    shutil.copy2(src, dst)
    for ext in ("-wal", "-shm"):  # include uncommitted changes Firefox left in its write-ahead log
        if os.path.exists(src + ext):
            shutil.copy2(src + ext, dst + ext)
    return sqlite3.connect(dst)


def read_bookmarks(db):
    rows = db.execute(
        "SELECT p.url, COALESCE(NULLIF(b.title,''), p.title, p.url) FROM moz_bookmarks b "
        "JOIN moz_places p ON p.id = b.fk WHERE b.type = 1 ORDER BY b.dateAdded"
    ).fetchall()
    return [(u, clean(t)) for u, t in rows if u and is_web(u)]


def read_history(db):
    rows = db.execute(
        "SELECT url, title, visit_count, last_visit_date FROM moz_places "
        "WHERE visit_count > 0 AND last_visit_date IS NOT NULL"
    ).fetchall()
    out = []
    for url, title, count, last in rows:
        if url and is_web(url) and not is_search_page(url):
            out.append({"url": url.split("#")[0], "title": clean(title), "count": int(count), "last": int(last) // 1_000_000})
    return out


def read_cookies(db, now):
    lines = []
    for host, path, name, value, expiry, secure, httponly in db.execute(
        "SELECT host, path, name, value, expiry, isSecure, isHttpOnly FROM moz_cookies"
    ):
        if expiry > 10**12:  # newer profiles store milliseconds
            expiry //= 1000
        # session cookies and expired ones are left behind
        if expiry <= now or any(c in (host + path + name + value) for c in "\t\n\r") or not host:
            continue
        subdomains = "TRUE" if host.startswith(".") else "FALSE"
        prefix = "#HttpOnly_" if httponly else ""
        lines.append(f"{prefix}{host}\t{subdomains}\t{path}\t{'TRUE' if secure else 'FALSE'}\t{expiry}\t{name}\t{value}")
    return lines


def merge_marks(existing, imported):
    seen = {u.lower() for u, _ in existing}
    added = [(u, t) for u, t in imported if not (u.lower() in seen or seen.add(u.lower()))]
    return existing + added, len(added)


def merge_history(existing, imported, now):
    by_url = {e["url"]: dict(e) for e in existing}
    added = 0
    for e in imported:
        cur = by_url.get(e["url"])
        if cur:
            cur["count"] += e["count"]
            cur["last"] = max(cur["last"], e["last"])
            cur["title"] = cur["title"] or e["title"]
        else:
            by_url[e["url"]] = dict(e)
            added += 1
    def score(e):  # keeps frequent and recent pages when trimming (same idea as Zolith)
        import math
        days = max(0, now - e["last"]) // 86400
        return math.log(e["count"] + 1) * 10 + 20 / (1 + days)
    kept = sorted(by_url.values(), key=score, reverse=True)[:MAX_HISTORY]
    return kept, added


def load_marks(path):
    if not os.path.exists(path):
        return []
    return [tuple(l.rstrip("\n").split("\t", 1)) for l in open(path, encoding="utf-8", errors="replace") if "\t" in l]


def load_history(path):
    out = []
    if os.path.exists(path):
        for l in open(path, encoding="utf-8", errors="replace"):
            p = l.rstrip("\n").split("\t", 3)
            if len(p) >= 3 and p[0].isdigit() and p[1].isdigit():
                out.append({"last": int(p[0]), "count": int(p[1]), "url": p[2], "title": p[3] if len(p) > 3 else ""})
    return out


def backup(path):
    if os.path.exists(path):
        shutil.copy2(path, f"{path}.bak-{time.strftime('%Y%m%d-%H%M%S')}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--profile", help="Firefox profile folder (default: the most recently used one)")
    ap.add_argument("--zolith-home", default=os.path.expanduser("~"), help=argparse.SUPPRESS)  # for tests
    ap.add_argument("--dry-run", action="store_true", help="show what would be imported, change nothing")
    ap.add_argument("--no-cookies", action="store_true", help="skip cookies (they keep you signed in to sites)")
    args = ap.parse_args()

    if not args.dry_run and subprocess.run(["pgrep", "-x", "zolith"], capture_output=True).returncode == 0:
        sys.exit("Zolith is running. Close it first: it rewrites these files when it exits.")
    profile = args.profile or find_profile()
    cfg = os.path.join(args.zolith_home, ".config/zolith")
    data = os.path.join(args.zolith_home, ".local/share/zolith")
    now = int(time.time())
    print(f"Firefox profile: {profile}")

    with tempfile.TemporaryDirectory() as tmp:
        places = copy_db(profile, "places.sqlite", tmp)
        if places is None:
            sys.exit("No places.sqlite in that profile.")
        marks_in, hist_in = read_bookmarks(places), read_history(places)
        cookies_in = []
        if not args.no_cookies:
            cdb = copy_db(profile, "cookies.sqlite", tmp)
            cookies_in = read_cookies(cdb, now) if cdb else []

    marks, marks_added = merge_marks(load_marks(os.path.join(cfg, "bookmarks.tsv")), marks_in)
    hist, hist_added = merge_history(load_history(os.path.join(cfg, "history.tsv")), hist_in, now)
    print(f"Bookmarks: {len(marks_in)} found in Firefox, {marks_added} new")
    print(f"History:   {len(hist_in)} pages found in Firefox, {hist_added} new (Zolith keeps the best {MAX_HISTORY})")
    if not args.no_cookies:
        print(f"Cookies:   {len(cookies_in)} unexpired cookies found")
    print("Passwords: not imported (Firefox encrypts them, and Zolith has no password manager)")
    if args.dry_run:
        print("Dry run: nothing was changed.")
        return

    os.makedirs(cfg, exist_ok=True)
    os.makedirs(data, exist_ok=True)
    bm_path, hi_path, ck_path = (os.path.join(cfg, "bookmarks.tsv"), os.path.join(cfg, "history.tsv"), os.path.join(data, "cookies"))
    for p in (bm_path, hi_path, ck_path):
        backup(p)
    with open(bm_path, "w", encoding="utf-8") as f:
        f.writelines(f"{u}\t{t}\n" for u, t in marks)
    with open(hi_path, "w", encoding="utf-8") as f:
        f.writelines(f"{e['last']}\t{e['count']}\t{e['url']}\t{e['title']}\n" for e in hist)
    if cookies_in:
        # keep cookies Zolith already has; Firefox's replace ones for the same site, name and path
        mine = {}
        if os.path.exists(ck_path):
            for l in open(ck_path, encoding="utf-8", errors="replace"):
                p = l.rstrip("\n").split("\t")
                if len(p) == 7:
                    mine[(p[0].removeprefix("#HttpOnly_"), p[2], p[5])] = l.rstrip("\n")
        for l in cookies_in:
            p = l.split("\t")
            mine[(p[0].removeprefix("#HttpOnly_"), p[2], p[5])] = l
        with open(ck_path, "w", encoding="utf-8") as f:
            f.write("# Netscape HTTP Cookie File\n" + "\n".join(mine.values()) + "\n")
        os.chmod(ck_path, 0o600)
    print("Done. Start Zolith to see your data. Backups of the old files end in .bak-<date>.")


if __name__ == "__main__":
    main()
