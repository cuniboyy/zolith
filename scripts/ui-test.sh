#!/bin/bash
# End-to-end smoke test: launches Zolith with a throwaway profile, types an address and checks
# it was loaded and remembered. Needs a Hyprland (Wayland) session, `wtype`, python3 and a
# built binary (default target/debug/zolith). It only sends keys while Zolith is the active window.
set -u
BIN=${1:-target/debug/zolith}
T=$(mktemp -d); trap 'kill $SRV $PID 2>/dev/null; rm -rf "$T"' EXIT
mkdir -p "$T/home/.config/zolith"
python3 - "$T" <<'PY' &
import http.server, socket, sys
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        b = b"<!doctype html><title>ui-test-page</title><h1>ok</h1>"
        self.send_response(200); self.send_header("Content-Type", "text/html"); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
class S6(http.server.ThreadingHTTPServer): address_family = socket.AF_INET6
S6(("::", 38431), H).serve_forever()
PY
SRV=$!
HOME=$T/home ZOLITH_SINGLE_INSTANCE=0 "$BIN" > "$T/run.log" 2>&1 & PID=$!
sleep 5
active() { hyprctl activewindow -j | python3 -c "import json,sys; print(json.load(sys.stdin).get('class'))"; }
key() { hyprctl dispatch "hl.dsp.send_shortcut({ mods = \"$1\", key = \"$2\", window = \"class:zolith\" })" >/dev/null 2>&1; }
[ "$(active)" = zolith ] || hyprctl dispatch 'hl.dsp.focus({ window = "class:zolith" })' >/dev/null 2>&1; sleep 0.5
[ "$(active)" = zolith ] || { echo "FAIL: zolith is not the active window"; exit 2; }
key CTRL l; sleep 0.4; wtype "127.0.0.1:38431/"; sleep 1; key "" Return
# wait (up to 10s) for the page to load and be remembered, rather than sleeping a fixed time
for _ in $(seq 1 20); do grep -q "ui-test-page" "$T/home/.config/zolith/history.tsv" 2>/dev/null && break; sleep 0.5; done
fail=0
[ -n "${DEBUG:-}" ] && { echo "--- profile:"; ls "$T/home/.config/zolith" 2>&1; cat "$T/home/.config/zolith/session" 2>&1; hyprctl clients -j | python3 -c "import json,sys; [print(c[\"class\"], c[\"title\"]) for c in json.load(sys.stdin) if c[\"class\"]==\"zolith\"]"; }
grep -q "127.0.0.1:38431" "$T/home/.config/zolith/history.tsv" 2>/dev/null && echo "PASS: page was loaded and remembered in history" || { echo "FAIL: page missing from history"; fail=1; }
grep -q "ui-test-page" "$T/home/.config/zolith/history.tsv" 2>/dev/null && echo "PASS: page title was recorded" || { echo "FAIL: title not recorded"; fail=1; }
exit $fail
