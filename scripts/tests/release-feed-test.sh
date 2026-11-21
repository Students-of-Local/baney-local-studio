#!/usr/bin/env bash
set -uo pipefail
script=$(cd "$(dirname "$0")/.." && pwd)/release.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/web" "$T/repo"
cat > "$T/bin/curl" <<'STUB'
#!/usr/bin/env bash
out= url=
while [ $# -gt 0 ]; do case $1 in -o) out=$2; shift ;; -w) shift ;; -*) ;; *) url=$1 ;; esac; shift; done
f="$WEB/$(sed -E 's#.*/download/([^/]+)/latest.json#\1#' <<< "$url")"
if [ -f "$f" ]; then cp "$f" "$out"; printf 200; else : > "$out"; printf 404; fi
STUB
chmod +x "$T/bin/curl"
cd "$T/repo" || exit 1
git init -q
commit() { git -c user.email=t@t -c user.name=t commit -q --allow-empty -m "$1" && git rev-parse HEAD; }
B=$(commit B)
C=$(commit C)
D=$(commit D)
status=0
run() {
  local want=$1 label=$2
  shift 2
  WEB=$T/web PATH=$T/bin:$PATH REPO=t/t GH_TOKEN=t bash "$script" check-newer "$@" > /dev/null 2>&1
  local got=$?
  [ "$got" = "$want" ] || { echo "$label: exit $got, want $want"; status=1; }
}
run 0 "no feed at all" dev 0.1.0-dev.20261121000000+d "$D"
printf '{"version":"0.1.0-dev.20261120000000+c","commit":"%s"}' "$C" > "$T/web/dev"
run 1 "dev on C, publish older B" dev 0.1.0-dev.20261121000000+b "$B"
run 0 "dev on C, publish descendant D" dev 0.1.0-dev.20261121000000+d "$D"
mv "$T/web/dev" "$T/web/dev-previous"
run 1 "only dev-previous on C, publish older B" dev 0.1.0-dev.20261121000000+b "$B"
run 0 "only dev-previous on C, publish descendant D" dev 0.1.0-dev.20261121000000+d "$D"
run 1 "only dev-previous on C, publish an older version" dev 0.1.0-dev.20261119000000+d "$D"
exit $status
