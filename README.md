<div align="center">

<img src="docs/logo.png" alt="Zolith" width="120">

# Zolith

**A fast, minimal web browser for Linux, written in Rust.**

Chrome-style tabs and settings, a smart address bar, and a built-in ad blocker.
No Electron, no Chromium: just GTK and your system's WebKit.

[Download](https://github.com/ash262049-png/zolith/releases/latest) ·
[Features](#features) ·
[Build from source](#build-from-source) ·
[Shortcuts](#keyboard-shortcuts)

![Zolith tabs](docs/screenshots/tabs.png)

</div>

## Features

**Tabs**
- Chrome-style tab strip: tabs share the width evenly, shrink as you open more, and scroll when they run out of room
- Site icons, a loading spinner, middle-click to close, reopen closed tabs, `Ctrl+1`–`9` to jump
- Session restore: pick up where you left off

**Address bar**
- Type the start of a site you visit and it completes inline: `you` becomes `youtube.com/`, and Enter goes straight there
- A dropdown of matching bookmarks, history and live search suggestions (DuckDuckGo, Google, Bing, Brave)
- Search or address, decided as you type; `localhost` and local IPs open over `http`

![Address bar suggestions](docs/screenshots/address-bar.png)

**Ad blocking**
- Blocks about 110 well-known ad and tracking domains, hides common ad boxes, and skips YouTube video ads
- Add your own domains in `~/.config/zolith/blocklist.txt`
- One switch in Settings turns it off

**Settings page** (`zolith://settings`, or `Ctrl+,`)
- Laid out like Chrome's: sidebar, search box, toggles
- Search engine (including your own URL), startup behaviour, home page, bookmarks bar, default zoom
- Clear browsing data: history, cookies and cache, over a time range
- Download folder

![Settings](docs/screenshots/settings.png)

**Everything else**
- Bookmarks and a bookmarks bar, find in page, zoom, print, developer tools
- Downloads that never overwrite existing files, with a status message
- Middle-click or `Ctrl`+click opens a link in a background tab
- Fullscreen video hides the toolbar
- Dark theme

## Install

### From a release

Download `zolith-0.1.0-linux-x86_64.tar.gz` from the [latest release](https://github.com/ash262049-png/zolith/releases/latest), then:

```bash
tar xzf zolith-0.1.0-linux-x86_64.tar.gz
cd zolith-0.1.0-linux-x86_64
./install.sh
```

This installs Zolith for your user only (no root): the program in `~/.local/bin`, plus its icon and an app-launcher entry.

You need these runtime libraries, which most desktops already have:

| Distro | Packages |
| --- | --- |
| Fedora | `webkit2gtk4.1 gtk3 libsoup3` |
| Debian / Ubuntu | `libwebkit2gtk-4.1-0 libgtk-3-0 libsoup-3.0-0` |
| Arch | `webkit2gtk-4.1 gtk3 libsoup3` |

### Build from source

You need [Rust](https://rustup.rs) and the development packages:

| Distro | Packages |
| --- | --- |
| Fedora | `webkit2gtk4.1-devel gtk3-devel libsoup3-devel` |
| Debian / Ubuntu | `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev` |
| Arch | `webkit2gtk-4.1 gtk3 libsoup3` |

```bash
git clone https://github.com/ash262049-png/zolith.git
cd zolith
cargo build --release
./install.sh
```

Or just run it without installing: `cargo run --release`.

> Developed and tested on Fedora with Wayland. Other distros and X11 should work but haven't been tried.

## Keyboard shortcuts

| | |
| --- | --- |
| `Ctrl+T` / `Ctrl+W` | New tab / close tab |
| `Ctrl+Shift+T` | Reopen closed tab |
| `Ctrl+Tab`, `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+1`–`8`, `Ctrl+9` | Jump to tab / last tab |
| `Ctrl+L`, `F6` | Focus the address bar |
| `Alt+←` / `Alt+→` | Back / forward |
| `Ctrl+R`, `F5` | Reload (stop while loading) |
| `Alt+Home` | Home |
| `Ctrl+F`, `Ctrl+G` / `F3` | Find, next / previous (`Shift`) match |
| `Ctrl+D` | Bookmark this page |
| `Ctrl+Shift+B` | Show / hide the bookmarks bar |
| `Ctrl++`, `Ctrl+-`, `Ctrl+0` | Zoom in / out / reset |
| `Ctrl+P` | Print |
| `F12`, `Ctrl+Shift+I` | Developer tools |
| `Ctrl+,` | Settings |
| `Ctrl+Q` | Quit |

## Your data

Everything stays on your machine:

| | |
| --- | --- |
| `~/.config/zolith/settings` | Your settings |
| `~/.config/zolith/bookmarks.tsv` | Bookmarks |
| `~/.config/zolith/history.tsv` | History (up to 3,000 pages) |
| `~/.config/zolith/blocklist.txt` | Your extra blocked domains, one per line |
| `~/.local/share/zolith/` | Cookies, cache and site data |

Search suggestions send what you type in the address bar to your search engine. Turn them off in Settings, under Search engine.

## Limitations

Zolith is young. Honest list of what it doesn't do yet:

- **Twitch stream ads are not blocked.** Twitch merges them into the video itself. Only its trackers are caught.
- **YouTube ad skipping is best-effort.** YouTube changes its page often, so it may need updating.
- The ad blocker uses a short curated domain list, not the large community filter lists.
- No private windows, extensions, password manager, sync, or history page.
- You can't drag tabs to reorder them or out into new windows.
- Linux only. It uses your system's WebKitGTK, so page rendering matches Epiphany and similar browsers, not Chrome or Firefox.

## Project layout

| | |
| --- | --- |
| `src/main.rs` | Window, tabs, address bar, commands |
| `src/suggest.rs` | History, ranking, inline completion, search suggestions |
| `src/settings.rs` | Settings, search engines, URL handling |
| `src/settings.html` | The settings page |
| `src/adblock.rs`, `adblock.js`, `blocklist.txt` | Ad blocking |
| `install.sh`, `packaging/` | User-level installer and launcher entry |

Run the tests with `cargo test`.

## Built with

[Rust](https://www.rust-lang.org), [GTK 3](https://www.gtk.org), [WebKitGTK](https://webkitgtk.org) via [`wry`](https://github.com/tauri-apps/wry) and [`tao`](https://github.com/tauri-apps/tao).
