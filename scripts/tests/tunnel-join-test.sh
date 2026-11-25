#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <local-studio binary> <scratch dir>"; exit 2; }
home=$2/home
dir=$2/data/tunnel
mkdir -p "$home" "$dir" && chmod 700 "$dir" || exit 1
coproc mcp {
  exec python3 -u -c '
import http.server, pathlib, sys
joined = pathlib.Path(sys.argv[1])
class Mcp(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        ok = joined.exists() or self.headers["Host"] != "studio.example.com"
        self.send_response(405 if ok else 421)
        self.end_headers()
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Mcp)
print(server.server_port)
server.serve_forever()
' "$2/joined"
}
server=$mcp_PID
trap 'kill "$server"' EXIT
read -r port <&"${mcp[0]}" || exit 1
route() {
  printf '{"v":1,"owner":"%s","route":"cloudflare","hostname":"studio.example.com","ports":{"mcp":%s,"serve":18094}}' "$1" "$port" > "$dir/route.json"
  chmod 600 "$dir/route.json"
}
fails=0
expect() {
  local got
  got=$(env -u XDG_CONFIG_HOME HOME="$home" LOCAL_STUDIO_DATA_DIR="${dir%/tunnel}" "$bin" tunnel status --json |
    python3 -c 'import json, sys; s = json.load(sys.stdin); print(s["next"], s.get("nextCommand"))')
  [ "$got" = "$2" ] && echo "ok: $1" || { echo "FAIL: $1: $got"; fails=1; }
}
bin=$1
route local-studio
expect "browser use that answers 421 for the name has still to join" "needs_join local-studio-browser remote join"
route local-studio-browser
expect "a route browser use owns is not asked" "ready None"
route local-studio
touch "$2/joined"
expect "ready once it joined" "ready None"
exit $fails
