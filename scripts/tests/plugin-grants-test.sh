#!/usr/bin/env bash
# Plugin consent and mcpServers ownership (#48), driven through the real local-studio CLI on scratch
# HOME, LOCAL_STUDIO_DATA_DIR and EUKHE_CODING_AGENT_DIR. Each scenario prints PASS or FAIL; the exit
# code is the number of failures.
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <local-studio binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SCRATCH=$2
mkdir -p "$SCRATCH"
SCRATCH=$(cd "$SCRATCH" && pwd -P)
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com \
  GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
failures=0
TIMEFORMAT=%R

touch "$SCRATCH/.case"
if [ -e "$SCRATCH/.CASE" ]; then CASELESS=1; else CASELESS=0; fi
rm -f "$SCRATCH/.case"
ROOT=0
[ "$(id -u)" = 0 ] && ROOT=1

pass() { echo "PASS $1"; }
skip() { echo "SKIP $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }

fresh() {
  S=$SCRATCH/$1
  rm -rf "$S"
  mkdir -p "$S/h" "$S/d/plugins" "$S/a" "$S/src"
  D=$S/d
  A=$S/a
  echo '{}' > "$A/settings.json"
  mkdir -p "$S/src/syncer"
  printf 'id = "syncer"\nversion = "1.0.0"\ndescription = "Runs a sync"\n' > "$S/src/syncer/plugin.toml"
}

ls_() { HOME=$S/h LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A "$BIN" "$@"; }

sync_() { ls_ plugin install "$S/src/syncer" > /dev/null 2>&1; }

manifest() {
  mkdir -p "$1/bin"
  printf 'id = "%s"\nversion = "1.0.0"\ndescription = "A test plugin"\ncapabilities = ["mcp-server"]\n[mcp]\ncommand = "%s"\n' \
    "$(basename "$1")" "${2:-bin/server}" > "$1/plugin.toml"
}

script() { printf '#!/bin/sh\necho %s\n' "$2" > "$1"; chmod 755 "$1"; }

status() {
  ls_ plugin list 2>&1 | awk -v id="$1" '$1 == id { getline; sub(/^ +/, ""); print; found = 1; exit } END { if (!found) print "did not load" }'
}

entry() {
  python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])).get("mcpServers",{}); print("yes" if sys.argv[2] in s else "no")' \
    "$A/settings.json" "$2"
}

settings() { python3 -I -c 'import json,sys; json.dump({"mcpServers": json.loads(sys.argv[2])}, open(sys.argv[1], "w"))' "$A/settings.json" "$1"; }

edited() {
  local id=$1 label=$2
  sync_
  local after
  after=$(status "$id")
  echo "  after the edit, plugin list: $after"
  echo "  after a sync, plugin-$id in mcpServers: $(entry "$A" "plugin-$id")"
  if [ "$after" != "ready" ] && [ "$(entry "$A" "plugin-$id")" = "no" ]; then pass "$label"; else fail "$label"; fi
}

echo "== second-F8: a command symlinked outside the plugin"
fresh f8
P=$D/plugins/toolbox
manifest "$P"
mkdir -p "$S/out"
script "$S/out/server" reviewed
ln -s "$S/out/server" "$P/bin/server"
echo "  grant: $(ls_ plugin grant toolbox mcp-server 2>&1 | head -1)"
echo "  plugin-toolbox in mcpServers: $(entry "$A" plugin-toolbox)"
script "$S/out/server" swapped
edited toolbox "second-F8 command symlinked outside"

echo "== second-F8: a lib folder symlinked outside the plugin"
fresh f8lib
P=$D/plugins/libbox
manifest "$P"
printf '#!/bin/sh\n. "$(dirname "$0")/../lib/run.sh"\n' > "$P/bin/server"
chmod 755 "$P/bin/server"
mkdir -p "$S/outlib"
echo 'echo reviewed' > "$S/outlib/run.sh"
ln -s "$S/outlib" "$P/lib"
echo "  grant: $(ls_ plugin grant libbox mcp-server 2>&1 | head -1)"
echo 'echo swapped' > "$S/outlib/run.sh"
edited libbox "second-F8 lib symlinked outside"

echo "== sol-F2: a command symlinked to .DS_Store"
fresh f2ds
P=$D/plugins/dsbox
manifest "$P"
script "$P/.DS_Store" reviewed
ln -s ../.DS_Store "$P/bin/server"
echo "  grant: $(ls_ plugin grant dsbox mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited dsbox "sol-F2 command symlinked to .DS_Store"

echo "== first-F3 / sol-F1: untagged entries whose cwd, not command, is under the plugins root"
fresh f3
mytool="{\"type\": \"stdio\", \"command\": \"/usr/bin/python3\", \"cwd\": \"$D/plugins/mytool\"}"
manager="{\"type\": \"stdio\", \"command\": \"/usr/local/bin/manager\", \"cwd\": \"$D/plugins/x\"}"
escape="{\"type\": \"stdio\", \"command\": \"$D/plugins/../../h/tool\"}"
settings "{\"plugin-mytool\": $mytool, \"plugin-manager\": $manager, \"plugin-escape\": $escape}"
sync_
kept="plugin-mytool=$(entry "$A" plugin-mytool) plugin-manager=$(entry "$A" plugin-manager) plugin-escape=$(entry "$A" plugin-escape)"
echo "  after a sync: $kept"
if [ "$kept" = "plugin-mytool=yes plugin-manager=yes plugin-escape=yes" ]; then pass "first-F3/sol-F1 user entries kept"; else fail "first-F3/sol-F1 user entries kept"; fi
P=$D/plugins/toolbox
manifest "$P"
script "$P/bin/server" box
settings "{\"plugin-toolbox\": {\"type\": \"stdio\", \"command\": \"$P/bin/old-server\"}}"
sync_
echo "  an untagged entry whose command is under the root, after a sync: $(entry "$A" plugin-toolbox)"
if [ "$(entry "$A" plugin-toolbox)" = "no" ]; then pass "first-F3 pre-tag entry still migrated"; else fail "first-F3 pre-tag entry still migrated"; fi

echo "== first-F1: a plugin folder that holds a 200 MB file"
fresh f1
P=$D/plugins/bigbox
manifest "$P"
script "$P/bin/server" box
mkdir -p "$P/data"
dd if=/dev/urandom of="$P/data/blob.bin" bs=1048576 count=200 2> /dev/null
echo "  plugin grant bigbox mcp-server, seconds:"
time (ls_ plugin grant bigbox mcp-server 2>&1 | head -1 | sed 's/^/    /')
echo "  plugin list (one scan), seconds:"
time (ls_ plugin list > /dev/null 2>&1)
big=$(status bigbox)
echo "  plugin list: $big"
case $big in *"file over 10 MB: data/blob.bin"*) pass "first-F1 size limit at consent";; *) fail "first-F1 size limit at consent";; esac

echo "== first-F1: a git checkout plugin with a 200 MB pack"
mkdir -p "$S/origin/gitbig"
manifest "$S/origin/gitbig"
script "$S/origin/gitbig/bin/server" box
git -C "$S/origin/gitbig" init -q -b main && git -C "$S/origin/gitbig" add -A && git -C "$S/origin/gitbig" commit -qm v1
git clone -q "$S/origin/gitbig" "$D/plugins/gitbig"
dd if=/dev/urandom of="$D/plugins/gitbig/.git/objects/pack/big.pack" bs=1048576 count=200 2> /dev/null
ls_ plugin grant gitbig mcp-server > /dev/null 2>&1
echo "  plugin list (one scan), seconds:"
time (ls_ plugin list > /dev/null 2>&1)
gitbig=$(status gitbig)
echo "  plugin list: $gitbig"
case $gitbig in *"file over 10 MB: .git/objects/pack/big.pack"*) pass "first-F1 size limit covers .git";; *) fail "first-F1 size limit covers .git";; esac

checkout() {
  mkdir -p "$S/origin/$1"
  manifest "$S/origin/$1" "${2:-bin/server}"
  script "$S/origin/$1/bin/server" box
  git -C "$S/origin/$1" init -q -b main && git -C "$S/origin/$1" add -A && git -C "$S/origin/$1" commit -qm v1
}

upstream() {
  echo "$2" >> "$S/origin/$1/README"
  git -C "$S/origin/$1" add -A && git -C "$S/origin/$1" commit -qm "$2"
}

ended() {
  local id=$1 label=$2 now
  sync_
  now=$(status "$id")
  echo "  plugin list: $now, plugin-$id in mcpServers: $(entry "$A" "plugin-$id")"
  case $now in "needs consent"*) [ "$(entry "$A" "plugin-$id")" = "no" ] && { pass "$label"; return; } ;; esac
  fail "$label"
}

echo "== first-F2: a git command in a checkout plugin ends consent cleanly"
fresh f2git
checkout gitbox
P=$D/plugins/gitbox
git clone -q "$S/origin/gitbox" "$P"
echo "  grant: $(ls_ plugin grant gitbox mcp-server 2>&1 | head -1)"
upstream gitbox v2
git -C "$P" fetch -q
ended gitbox "first-F2 git fetch ends consent"
echo "  grant again: $(ls_ plugin grant gitbox mcp-server 2>&1 | head -1)"
touch "$P/plugin.toml"
git -C "$P" status > /dev/null
echo "  after git status: $(status gitbox)"
echo "  grant again: $(ls_ plugin grant gitbox mcp-server 2>&1 | head -1)"
git -C "$P" config core.pager "sh -c 'echo swapped'"
edited gitbox "first-F2 .git/config stays covered"

echo "== first-F2: a manifest command inside .git"
fresh f2cmd
P=$D/plugins/gitcmd
manifest "$P" .git/server
mkdir -p "$P/.git"
script "$P/.git/server" reviewed
echo "  grant: $(ls_ plugin grant gitcmd mcp-server 2>&1 | head -1)"
script "$P/.git/server" swapped
edited gitcmd "first-F2 command inside .git stays covered"

echo "== first-F2: a command symlinked into .git/objects"
fresh f2obj
mkdir -p "$S/origin/gitobj"
manifest "$S/origin/gitobj"
git -C "$S/origin/gitobj" init -q -b main && git -C "$S/origin/gitobj" add -A && git -C "$S/origin/gitobj" commit -qm v1
P=$D/plugins/gitobj
git clone -q "$S/origin/gitobj" "$P"
script "$P/.git/objects/payload" reviewed
mkdir -p "$P/bin"
ln -s ../.git/objects/payload "$P/bin/server"
echo "  grant: $(ls_ plugin grant gitobj mcp-server 2>&1 | head -1)"
script "$P/.git/objects/payload" swapped
edited gitobj "first-F2 symlink into .git/objects stays covered"

echo "== first-F2: plugin install <dir> from a checkout keeps consent through git commands"
fresh f2dev
checkout devbox
git clone -q "$S/origin/devbox" "$S/work"
echo "  install: $(ls_ plugin install "$S/work" 2>&1 | head -1)"
echo "  grant: $(ls_ plugin grant devbox mcp-server 2>&1 | head -1)"
upstream devbox v2
git -C "$S/work" fetch -q
touch "$S/work/plugin.toml"
git -C "$S/work" status > /dev/null
sync_
dev=$(status devbox)
echo "  after git fetch and git status in the checkout: $dev, plugin-devbox in mcpServers: $(entry "$A" plugin-devbox)"
if [ "$dev" = "ready" ] && [ "$(entry "$A" plugin-devbox)" = "yes" ]; then pass "first-F2 installed copy keeps consent"; else fail "first-F2 installed copy keeps consent"; fi

echo "== first-F2: files that vanish while consent reads the folder"
fresh f2race
checkout racebox
P=$D/plugins/racebox
git clone -q "$S/origin/racebox" "$P"
echo "  grant: $(ls_ plugin grant racebox mcp-server 2>&1 | head -1)"
python3 -I -c '
import os, sys
d = sys.argv[1]
while True:
    for i in range(200):
        open(os.path.join(d, "churn%d.lock" % i), "w").close()
    for i in range(200):
        os.unlink(os.path.join(d, "churn%d.lock" % i))
' "$P/.git" &
churn=$!
seen=""
for _ in $(seq 40); do
  now=$(status racebox)
  case $now in "needs consent"* | ready) ;; *) seen="$now"; break ;; esac
done
kill "$churn"
wait "$churn" 2> /dev/null
echo "  40 scans while git-like lock files come and go: ${seen:-every one ready or needs consent}"
if [ -z "$seen" ]; then pass "first-F2 vanished file is no error"; else fail "first-F2 vanished file is no error"; fi

echo "== first-F1: lib -> .git with command lib/objects/payload"
fresh hf1
checkout libgit lib/objects/payload
P=$D/plugins/libgit
git clone -q "$S/origin/libgit" "$P"
ln -s .git "$P/lib"
script "$P/.git/objects/payload" reviewed
echo "  grant: $(ls_ plugin grant libgit mcp-server 2>&1 | head -1)"
script "$P/.git/objects/payload" swapped
edited libgit "first-F1 lib -> .git payload stays covered"

echo "== sol-F1: bin/repo -> ../.git with command bin/repo/objects/server"
fresh hsol1
checkout repobox bin/repo/objects/server
P=$D/plugins/repobox
git clone -q "$S/origin/repobox" "$P"
ln -s ../.git "$P/bin/repo"
script "$P/.git/objects/server" reviewed
echo "  grant: $(ls_ plugin grant repobox mcp-server 2>&1 | head -1)"
script "$P/.git/objects/server" swapped
edited repobox "sol-F1 bin/repo -> ../.git server stays covered"

echo "== second-S1: command .Git/objects/payload on a case-insensitive disk"
if [ $CASELESS = 0 ]; then
  skip "second-S1 .Git payload stays covered (this disk is case-sensitive, so .Git does not reach .git)"
else
fresh hs1
checkout casebox .Git/objects/payload
P=$D/plugins/casebox
git clone -q "$S/origin/casebox" "$P"
script "$P/.git/objects/payload" reviewed
echo "  grant: $(ls_ plugin grant casebox mcp-server 2>&1 | head -1)"
script "$P/.git/objects/payload" swapped
edited casebox "second-S1 .Git payload stays covered"
fi

echo "== second-S1: command .ds_store and a symlink to .DS_STORE"
fresh hs1ds
P=$D/plugins/lowerds
manifest "$P" .ds_store
script "$P/.DS_Store" reviewed
echo "  grant: $(ls_ plugin grant lowerds mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited lowerds "second-S1 command .ds_store refused"
if [ $CASELESS = 0 ]; then
  skip "second-S1 symlink to .DS_STORE refused (this disk is case-sensitive, so .DS_STORE does not reach .DS_Store)"
  skip "second-S1 command .DS_Store spelled with a long s refused (this disk does not fold names)"
else
P=$D/plugins/upperds
manifest "$P"
script "$P/.DS_Store" reviewed
ln -s ../.DS_STORE "$P/bin/server"
echo "  grant: $(ls_ plugin grant upperds mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited upperds "second-S1 symlink to .DS_STORE refused"
P=$D/plugins/foldds
manifest "$P" $'.DS_\xc5\xbftore'
script "$P/.DS_Store" reviewed
echo "  grant (command .DS_ with a long s, which APFS folds to .DS_Store): $(ls_ plugin grant foldds mcp-server 2>&1 | head -1)"
script "$P/.DS_Store" swapped
edited foldds "second-S1 command .DS_Store spelled with a long s refused"
fi

echo "== second-S3: a socket and a FIFO in the plugin folder"
fresh s3
P=$D/plugins/sockbox
manifest "$P"
script "$P/bin/server" box
python3 -I -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$P/run.sock"
mkfifo "$P/run.fifo"
echo "  grant: $(ls_ plugin grant sockbox mcp-server 2>&1 | head -1)"
sync_
sock=$(status sockbox)
echo "  plugin list: $sock, plugin-sockbox in mcpServers: $(entry "$A" plugin-sockbox)"
if [ "$sock" = "ready" ] && [ "$(entry "$A" plugin-sockbox)" = "yes" ]; then pass "second-S3 socket and FIFO keep consent"; else fail "second-S3 socket and FIFO keep consent"; fi
for kind in sock fifo; do
  P=$D/plugins/run$kind
  manifest "$P" "run.$kind"
  if [ $kind = sock ]; then python3 -I -c 'import socket, sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$P/run.sock"; else mkfifo "$P/run.fifo"; fi
  ls_ plugin grant "run$kind" mcp-server > /dev/null 2>&1
  sync_
  named=$(status "run$kind")
  echo "  command run.$kind: $named, plugin-run$kind in mcpServers: $(entry "$A" "plugin-run$kind")"
  if [ "$named" != "ready" ] && [ "$(entry "$A" "plugin-run$kind")" = "no" ]; then pass "second-S3 command run.$kind gets no entry"; else fail "second-S3 command run.$kind gets no entry"; fi
done

waited() {
  local pid=$1 n=0
  while kill -0 "$pid" 2> /dev/null; do
    if [ $n -ge 100 ]; then
      kill -9 "$pid" 2> /dev/null
      wait "$pid" 2> /dev/null
      return 1
    fi
    sleep 0.1
    n=$((n + 1))
  done
  wait "$pid" 2> /dev/null
  return 0
}

gated() {
  mkdir -p "$S/gate"
  rm -f "$S/gate/walked" "$S/gate/go"
  (
    export HOME=$S/h LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A LOCAL_STUDIO_TEST_HASH_GATE=$S/gate
    exec "$BIN" "$@"
  ) > "$S/out" 2>&1 &
  local pid=$! n=0
  while [ ! -e "$S/gate/walked" ] && [ $n -lt 100 ]; do sleep 0.1; n=$((n + 1)); done
  if [ ! -e "$S/gate/walked" ]; then
    kill -9 "$pid" 2> /dev/null
    wait "$pid" 2> /dev/null
    echo "gate did not fire: build with --features test-hooks"
    return
  fi
  mkfifo "$S/swap.fifo"
  mv "$S/swap.fifo" "$GATED"
  touch "$S/gate/go"
  if waited "$pid"; then
    echo "finished: $(tr '\n' ' ' < "$S/out")"
  else
    echo "hung on the FIFO, killed after 10 s"
  fi
}

echo "== second-S3: a FIFO renamed over a regular file between the walk and the read"
echo "  LOCAL_STUDIO_TEST_HASH_GATE (builds with --features test-hooks) makes the binary touch <gate>/walked after the walk"
echo "  and wait for <gate>/go before it opens any file, so the swap below always lands between stat and open"
fresh s3race
P=$D/plugins/fifobox
manifest "$P"
script "$P/bin/server" box
echo data > "$P/data.txt"
echo "  grant: $(ls_ plugin grant fifobox mcp-server 2>&1 | head -1)"
GATED=$P/data.txt
listed=$(gated plugin list)
echo "  plugin list: $listed"
echo "  plugin-fifobox in mcpServers: $(entry "$A" plugin-fifobox)"
case $listed in "finished: "*"fifobox 1.0.0"*"refused"*) [ "$(entry "$A" plugin-fifobox)" = "no" ] && pass "second-S3 consent does not block on a FIFO" || fail "second-S3 consent does not block on a FIFO";; *) fail "second-S3 consent does not block on a FIFO";; esac
mkdir -p "$S/src/fifosrc/bin"
manifest "$S/src/fifosrc"
script "$S/src/fifosrc/bin/server" box
echo data > "$S/src/fifosrc/data.txt"
GATED=$S/src/fifosrc/data.txt
installed=$(gated plugin install "$S/src/fifosrc")
echo "  plugin install: $installed"
case $installed in "finished: error: refused"*"data.txt"*"no longer a regular file"*) pass "second-S3 install does not block on a FIFO";; *) fail "second-S3 install does not block on a FIFO";; esac
rm -f "$GATED"
echo data > "$GATED"
hashed=$(gated plugin hash "$S/src/fifosrc")
echo "  plugin hash: $hashed"
case $hashed in "finished: error: "*"data.txt"*"no longer a regular file"*) pass "second-S3 plugin hash does not block on a FIFO";; *) fail "second-S3 plugin hash does not block on a FIFO";; esac

# Lets the hash pass untouched, then swaps $GATED for $1 (fifo, or a link to $2) after the hash and
# before the copy, at the `copying` pause point.
gated_copy() {
  local swap=$1 outside=$2
  shift 2
  mkdir -p "$S/gate"
  rm -f "$S/gate/walked" "$S/gate/go" "$S/gate/copying" "$S/gate/copy-go"
  (
    export HOME=$S/h LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A LOCAL_STUDIO_TEST_HASH_GATE=$S/gate
    exec "$BIN" "$@"
  ) > "$S/out" 2>&1 &
  local pid=$! n=0
  while [ ! -e "$S/gate/walked" ] && [ $n -lt 100 ]; do sleep 0.1; n=$((n + 1)); done
  touch "$S/gate/go"
  n=0
  while [ ! -e "$S/gate/copying" ] && [ $n -lt 100 ]; do sleep 0.1; n=$((n + 1)); done
  if [ ! -e "$S/gate/copying" ]; then
    kill -9 "$pid" 2> /dev/null
    wait "$pid" 2> /dev/null
    echo "copy gate did not fire: build with --features test-hooks"
    return
  fi
  rm -f "$GATED"
  if [ "$swap" = fifo ]; then mkfifo "$GATED"; else ln -s "$outside" "$GATED"; fi
  touch "$S/gate/copy-go"
  if waited "$pid"; then
    echo "finished: $(tr '\n' ' ' < "$S/out")"
  else
    echo "hung on the FIFO, killed after 10 s"
  fi
}

echo "== r1 second-S1: a FIFO or a link swapped in after the hash and before the install copies"
fresh s3copy
mkdir -p "$S/src/copysrc/bin"
manifest "$S/src/copysrc"
script "$S/src/copysrc/bin/server" box
echo data > "$S/src/copysrc/data.txt"
GATED=$S/src/copysrc/data.txt
copied=$(gated_copy fifo "" plugin install "$S/src/copysrc")
echo "  plugin install, FIFO at copy: $copied"
case $copied in "finished: error: "*"data.txt"*"no longer a regular file"*) pass "install copy does not block on a FIFO";; *) fail "install copy does not block on a FIFO";; esac
echo "outside-secret-$$" > "$S/outside.txt"
rm -f "$GATED"
echo data > "$GATED"
copied=$(gated_copy link "$S/outside.txt" plugin install "$S/src/copysrc")
echo "  plugin install, link at copy: $copied"
leaked=$(grep -rl "outside-secret-$$" "$D/plugins" 2> /dev/null | head -1)
echo "  outside file copied into the plugins folder: ${leaked:-no}"
case $copied in "finished: error: "*"data.txt"*"no longer a regular file"*) [ -z "$leaked" ] && pass "install copy does not follow a link" || fail "install copy does not follow a link";; *) fail "install copy does not follow a link";; esac

echo "== second-S1: plugin list applies an edit by itself"
fresh s1list
P=$D/plugins/listbox
manifest "$P"
script "$P/bin/server" reviewed
echo "  grant: $(ls_ plugin grant listbox mcp-server 2>&1 | head -1)"
echo "  plugin-listbox in mcpServers: $(entry "$A" plugin-listbox)"
script "$P/bin/server" swapped
listed=$(status listbox)
echo "  plugin list (no other command): $listed, plugin-listbox in mcpServers: $(entry "$A" plugin-listbox)"
case $listed in "needs consent"*) [ "$(entry "$A" plugin-listbox)" = "no" ] && pass "second-S1 plugin list syncs" || fail "second-S1 plugin list syncs";; *) fail "second-S1 plugin list syncs";; esac

echo "== first-F1: install and plugin hash refuse a 200 MB file without reading it"
fresh f1read
mkdir -p "$S/src/bigsrc/bin" "$S/src/bigsrc/data"
manifest "$S/src/bigsrc"
script "$S/src/bigsrc/bin/server" box
python3 -I -c 'import sys; open(sys.argv[1], "wb").truncate(200 * 1048576)' "$S/src/bigsrc/data/blob.bin"
chmod 000 "$S/src/bigsrc/data/blob.bin"
[ $ROOT = 1 ] && echo "  running as root, so mode 000 does not stop a read; the size line alone shows the refusal came from a stat"
for what in install hash; do
  out=$(ls_ plugin $what "$S/src/bigsrc" 2>&1 | tr '\n' ' ')
  echo "  plugin $what: $out"
  case $out in *"cannot read"*) fail "first-F1 plugin $what refuses before reading";; *"file over 10 MB: data/blob.bin (209715200 bytes)"*) pass "first-F1 plugin $what refuses before reading";; *) fail "first-F1 plugin $what refuses before reading";; esac
done
chmod 644 "$S/src/bigsrc/data/blob.bin"

echo "== first-F2: the hash skips the same names the link check refuses, in any letter case"
fresh f2case
mkdir -p "$S/src/casesrc/bin"
manifest "$S/src/casesrc"
script "$S/src/casesrc/bin/server" box
before=$(ls_ plugin hash "$S/src/casesrc" 2>&1)
echo junk > "$S/src/casesrc/.ds_store"
mkdir -p "$S/src/casesrc/.Git"
echo junk > "$S/src/casesrc/.Git/config"
after=$(ls_ plugin hash "$S/src/casesrc" 2>&1)
echo "  plugin hash before: $before"
echo "  plugin hash with .ds_store and .Git/config added: $after"
case $before in sha256:*) [ "$before" = "$after" ] && pass "first-F2 .ds_store and .Git left out like .DS_Store and .git" || fail "first-F2 .ds_store and .Git left out like .DS_Store and .git";; *) fail "first-F2 .ds_store and .Git left out like .DS_Store and .git";; esac
ls_ plugin install "$S/src/casesrc" > /dev/null 2>&1
copied=$(cd "$D/plugins/casesrc" 2> /dev/null && ls -A | tr '\n' ' ')
echo "  installed copy holds: $copied"
case " $copied" in *" .ds_store "* | *" .Git "*) fail "first-F2 install copies neither";; *" plugin.toml "*) pass "first-F2 install copies neither";; *) fail "first-F2 install copies neither";; esac

echo "== first-F3: a plugin folder that is itself a symlink"
fresh f3link
mkdir -p "$S/elsewhere"
manifest "$S/elsewhere/linkbox"
script "$S/elsewhere/linkbox/bin/server" box
ln -s "$S/elsewhere/linkbox" "$D/plugins/linkbox"
echo "  grant: $(ls_ plugin grant linkbox mcp-server 2>&1 | head -1)"
listed=$(ls_ plugin list 2>&1 | grep -A1 'linkbox' | tr '\n' ' ')
echo "  plugin list: $listed"
echo "  plugin-linkbox in mcpServers: $(entry "$A" plugin-linkbox)"
case $listed in *"failed"*"linkbox"*"symlink"*) [ "$(entry "$A" plugin-linkbox)" = "no" ] && pass "first-F3 symlinked plugin folder refused" || fail "first-F3 symlinked plugin folder refused";; *) fail "first-F3 symlinked plugin folder refused";; esac

echo "== second-S2: a plugin with no grant record is not hashed"
if [ $ROOT = 1 ]; then
  skip "second-S2 no hash without a grant (root reads a mode 000 file)"
else
fresh s2
P=$D/plugins/nohash
manifest "$P"
script "$P/bin/server" box
echo secret > "$P/unreadable"
chmod 000 "$P/unreadable"
nohash=$(status nohash)
echo "  plugin list with a mode 000 file and no grant: $nohash"
case $nohash in "needs consent"*) pass "second-S2 no hash without a grant";; *) fail "second-S2 no hash without a grant";; esac
echo "  grant: $(ls_ plugin grant nohash mcp-server 2>&1 | head -1)"
chmod 644 "$P/unreadable"
fi

echo "== second-F9: a tagged entry copied under a name without the plugin- prefix"
fresh f9
settings '{"toolbox-debug": {"type": "stdio", "command": "/usr/local/bin/toolbox", "localStudioPlugin": true}}'
sync_
echo "  after a sync, toolbox-debug in mcpServers: $(entry "$A" toolbox-debug)"
if [ "$(entry "$A" toolbox-debug)" = "yes" ]; then pass "second-F9 copied tagged entry kept"; else fail "second-F9 copied tagged entry kept"; fi

echo "== first-F5: a grant with a relative data dir records an absolute folder"
fresh f5
P=$D/plugins/toolbox
manifest "$P"
script "$P/bin/server" box
(cd "$S" && HOME=$S/h LOCAL_STUDIO_DATA_DIR=d EUKHE_CODING_AGENT_DIR=$A "$BIN" plugin grant toolbox mcp-server > /dev/null 2>&1)
source=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["plugins"]["toolbox"]["source"])' "$D/plugins/grants.json")
echo "  grants.json source: $source"
case $source in "folder /"*) pass "first-F5 absolute folder source";; *) fail "first-F5 absolute folder source";; esac

echo "== first-F6: plugin list names a grant of unknown origin"
fresh f6
P=$D/plugins/toolbox
manifest "$P"
script "$P/bin/server" box
echo '{"plugins": {"toolbox": {"version": "1.0.0", "capabilities": ["mcp-server"]}}}' > "$D/plugins/grants.json"
ls_ plugin grant toolbox mcp-server > /dev/null 2>&1
line=$(ls_ plugin list 2>&1 | grep 'granted mcp-server')
echo "  plugin list: '$line'"
case $line in *"from ") fail "first-F6 empty source named";; *"from "?*) pass "first-F6 empty source named";; *) fail "first-F6 empty source named";; esac

echo "== #14: the consent sheet, driven through Studio's install_plugin and answer_consent"
DRIVER=$(dirname "$BIN")/examples/consent_driver
if [ ! -x "$DRIVER" ]; then
  skip "consent sheet (build the consent_driver example beside $BIN)"
else
  fresh sheet
  drive() { HOME=$S/h LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A "$DRIVER" "$D" "$S/n.sock" "$@" 2>&1; }
  granted() {
    python3 -I -c 'import json,sys; g=json.load(open(sys.argv[1])).get("plugins",{}).get(sys.argv[2]); print("none" if g is None else ",".join(sorted(g["capabilities"])))' \
      "$D/plugins/grants.json" "$1" 2> /dev/null || echo none
  }
  sheeted() {
    local src=$S/src/sheet version=$1 caps=$2
    rm -rf "$src"
    mkdir -p "$src/bin"
    printf 'id = "sheet"\nname = "Sheet"\nversion = "%s"\ndescription = "A consent sheet test plugin"\ncapabilities = [%s]\n[gates]\nscreen-capture = "page screenshots"\n[mcp]\ncommand = "bin/server"\nargs = ["mcp"]\n' \
      "$version" "$caps" > "$src/plugin.toml"
    script "$src/bin/server" "$version"
    script "$src/bin/helper" helper
  }
  sheeted 1.0.0 '"mcp-server", "ask-user", "screen-capture", "site-secrets"'
  out=$(drive "$S/src/sheet" cancel)
  echo "$out" | sed 's/^/  /'
  rows=$(echo "$out" | sed -n 's/^SHEET .* rows=//p')
  if [ "$rows" = "mcp-server:needed,ask-user:needed,screen-capture:sensitive,site-secrets:sensitive" ] \
    && echo "$out" | grep -q '^SHEET Sheet 1.0.0 from=- source=a local folder pin=- files=3 findings=1 ' \
    && echo "$out" | grep -q '^ROW mcp-server | Give agents tools | Starts server mcp when a thread uses it. | -$' \
    && echo "$out" | grep -q '^ROW screen-capture | See the windows or tabs it opened | .* | page screenshots$'; then
    pass "#14 a new install lists every capability, needed and sensitive, with the scan finding"
  else
    fail "#14 a new install lists every capability, needed and sensitive, with the scan finding"
  fi
  if echo "$out" | grep -q '^LATE sees the sheet$'; then
    pass "#14 a subscriber that joins after the sheet was raised (the Remote hub) gets it"
  else
    fail "#14 a subscriber that joins after the sheet was raised (the Remote hub) gets it"
  fi
  leftovers=$(find "$D/plugins" -mindepth 1 -maxdepth 1 ! -name grants.json | wc -l | tr -d ' ')
  echo "  after Cancel: grants $(granted sheet), entries in the plugins folder besides grants.json: $leftovers"
  if echo "$out" | grep -q '^DONE ' && [ "$(granted sheet)" = none ] && [ "$leftovers" = 0 ]; then
    pass "#14 Cancel installs nothing and grants nothing"
  else
    fail "#14 Cancel installs nothing and grants nothing"
  fi
  out=$(drive "$S/src/sheet" install:screen-capture)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  echo "  after Install with screen-capture on: grants $(granted sheet); plugin list: $(status sheet)"
  if echo "$out" | grep -q '^DONE Installed Sheet 1.0.0 with 3 new permissions, but it is off: its tools stay off until every permission it asks for is on (local-studio plugin grant sheet site-secrets)$' \
    && [ "$(granted sheet)" = "ask-user,mcp-server,screen-capture" ] \
    && [ "$(status sheet)" = "needs consent for site-secrets (local-studio plugin grant sheet site-secrets)" ]; then
    pass "#14 / second-G1 Install grants the needed capabilities plus the one switched on and says the plugin is installed but off"
  else
    fail "#14 / second-G1 Install grants the needed capabilities plus the one switched on and says the plugin is installed but off"
  fi
  sheeted 2.0.0 '"mcp-server", "ask-user", "screen-capture", "site-secrets", "synthetic-input"'
  out=$(drive "$S/src/sheet" install:site-secrets)
  version=$(awk -F'"' '/^version/ { print $2 }' "$D/plugins/sheet/plugin.toml")
  echo "  Install with site-secrets on a sheet that lists only synthetic-input: $(echo "$out" | grep "^FAILED"); installed $version"
  if echo "$out" | grep -q '^FAILED .*not a sensitive permission this sheet asked for' && [ "$version" = 1.0.0 ] \
    && [ "$(granted sheet)" = "ask-user,mcp-server,screen-capture" ]; then
    pass "#14 a sensitive id the sheet did not list is refused and installs nothing"
  else
    fail "#14 a sensitive id the sheet did not list is refused and installs nothing"
  fi
  out=$(drive "$S/src/sheet" install)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  if [ "$(echo "$out" | sed -n 's/^SHEET .* rows=//p')" = "synthetic-input:sensitive" ] && echo "$out" | grep -q '^SHEET Sheet 2.0.0 from=1.0.0 ' \
    && echo "$out" | grep -q '^DONE ' && [ "$(granted sheet)" = "ask-user,mcp-server,screen-capture" ]; then
    pass "#14 an update that adds a capability asks only for it"
  else
    fail "#14 an update that adds a capability asks only for it"
  fi
  sheeted 3.0.0 '"mcp-server", "ask-user", "screen-capture", "synthetic-input"'
  out=$(drive "$S/src/sheet" cancel)
  echo "$out" | sed 's/^/  /'
  version=$(awk -F'"' '/^version/ { print $2 }' "$D/plugins/sheet/plugin.toml")
  if ! echo "$out" | grep -q '^SHEET' && echo "$out" | grep -q '^DONE Installed Sheet 3.0.0 with no new permissions' && [ "$version" = 3.0.0 ]; then
    pass "#14 an update that removes a capability installs without asking"
  else
    fail "#14 an update that removes a capability installs without asking"
  fi
fi

if [ -x "$DRIVER" ]; then
  echo "== first-F2: install_plugin with a request id whose sheet is already open"
  fresh f2
  sheeted 1.0.0 '"mcp-server", "screen-capture"'
  out=$(CONSENT_DRIVER_AGAIN=1 drive "$S/src/sheet" cancel)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  if echo "$out" | grep -q '^AGAIN refused: A permission sheet with that request is already open$' \
    && echo "$out" | grep -q '^DONE Nothing installed or granted for Sheet$'; then
    pass "first-F2 a repeated request id fails at once and the open sheet still answers"
  else
    fail "first-F2 a repeated request id fails at once and the open sheet still answers"
  fi
  echo "== first-F3: after an answer, Studio's own scan syncs mcpServers and offers what changed"
  fresh f3
  sheeted 1.0.0 '"mcp-server", "screen-capture"'
  Q=$D/plugins/dropped
  out=$(CONSENT_DRIVER_HOOK="mkdir -p $Q/bin && printf 'id = \"dropped\"\nversion = \"1.0.0\"\ndescription = \"Dropped in\"\ncapabilities = [\"mcp-server\"]\n[mcp]\ncommand = \"bin/server\"\n' > $Q/plugin.toml && printf '#!/bin/sh\n' > $Q/bin/server && chmod 755 $Q/bin/server" \
    drive "$S/src/sheet" install:screen-capture)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  echo "  plugin-sheet in mcpServers: $(entry "$A" plugin-sheet)"
  if echo "$out" | grep -q '^DONE Installed Sheet 1.0.0 with 2 new permissions$' && echo "$out" | grep -q '^OPEN-AFTER consent:dropped@1.0.0$' \
    && [ "$(entry "$A" plugin-sheet)" = yes ]; then
    pass "first-F3 the answer's sync runs in Studio's scan: mcpServers has the plugin and the scan offers the dropped-in plugin"
  else
    fail "first-F3 the answer's sync runs in Studio's scan: mcpServers has the plugin and the scan offers the dropped-in plugin"
  fi
  echo "== second-G2: two grant sheets answered at once"
  fresh g2
  for id in biga bigb; do
    manifest "$D/plugins/$id"
    script "$D/plugins/$id/bin/server" "$id"
    head -c 9000000 /dev/zero > "$D/plugins/$id/data.bin"
  done
  out=$(drive scan consent:biga@1.0.0=install consent:bigb@1.0.0=install)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  echo "  grants: biga '$(granted biga)', bigb '$(granted bigb)'"
  if echo "$out" | grep -q '^DONE consent:biga@1.0.0: ' && echo "$out" | grep -q '^DONE consent:bigb@1.0.0: ' \
    && [ "$(granted biga)" = mcp-server ] && [ "$(granted bigb)" = mcp-server ]; then
    pass "second-G2 two answers at once both keep their grant"
  else
    fail "second-G2 two answers at once both keep their grant"
  fi

  echo "== second-G3: the grant an update would carry is revoked while its sheet is open"
  fresh g3
  sheeted 1.0.0 '"mcp-server", "screen-capture"'
  drive "$S/src/sheet" install:screen-capture > /dev/null
  echo "  1.0.0 installed, grants $(granted sheet)"
  sheeted 2.0.0 '"mcp-server", "screen-capture", "synthetic-input"'
  out=$(CONSENT_DRIVER_HOOK="HOME=$S/h LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A $BIN plugin revoke sheet" drive "$S/src/sheet" install:synthetic-input)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  version=$(awk -F'"' '/^version/ { print $2 }' "$D/plugins/sheet/plugin.toml")
  echo "  installed $version, grants '$(granted sheet)'"
  if echo "$out" | grep -q '^SHEET Sheet 2.0.0 from=1.0.0 .* rows=synthetic-input:sensitive$' \
    && echo "$out" | grep -q '^FAILED the permissions sheet needs changed after they were checked' \
    && [ "$version" = 1.0.0 ] && [ -z "$(granted sheet)" ]; then
    pass "second-G3 a sheet whose rows went stale is refused and installs nothing"
  else
    fail "second-G3 a sheet whose rows went stale is refused and installs nothing"
  fi

  echo "== sol-F4 / first-F1: plugin revoke while a grant answer sits between its load and save"
  fresh f4
  for id in kept slow; do
    manifest "$D/plugins/$id"
    script "$D/plugins/$id/bin/server" "$id"
  done
  ls_ plugin grant kept mcp-server > /dev/null
  mkdir -p "$S/gate"
  touch "$S/gate/go"
  # Every hash passes until the hook, which arms the gate: the answer's grant then pauses after loading grants.json.
  (LOCAL_STUDIO_TEST_HASH_GATE=$S/gate CONSENT_DRIVER_HOOK="rm -f $S/gate/go $S/gate/walked && touch $S/gate/armed" \
    drive scan consent:slow@1.0.0=install > "$S/out") &
  pid=$!
  n=0
  while [ ! -e "$S/gate/walked" -o ! -e "$S/gate/armed" ] && [ $n -lt 600 ]; do sleep 0.1; n=$((n + 1)); done
  ls_ plugin revoke kept > "$S/revoke.out" 2>&1 &
  rpid=$!
  n=0
  while kill -0 "$rpid" 2> /dev/null && [ $n -lt 30 ]; do sleep 0.1; n=$((n + 1)); done
  if kill -0 "$rpid" 2> /dev/null; then held=yes; else held=no; fi
  touch "$S/gate/go"
  waited "$pid"
  wait "$rpid"
  grep -v '^ROW' "$S/out" | sed 's/^/  /'
  echo "  revoke waited for the answer: $held; grants: kept '$(granted kept)', slow '$(granted slow)'"
  if grep -q '^DONE consent:slow@1.0.0: ' "$S/out" && [ -z "$(granted kept)" ] && [ "$(granted slow)" = mcp-server ]; then
    pass "sol-F4 a revoke from the CLI during a Studio answer survives the answer's save"
  else
    fail "sol-F4 a revoke from the CLI during a Studio answer survives the answer's save"
  fi

  echo "== first-F1 / sol-F2: the grant sheets Studio offers when it scans the installed plugins"
  fresh scan
  mkdir -p "$S/src/granter/bin"
  printf 'id = "granter"\nname = "Granter"\nversion = "1.0.0"\ndescription = "Installed and never granted"\ncapabilities = ["mcp-server", "ask-user", "screen-capture"]\n[mcp]\ncommand = "bin/server"\n' > "$S/src/granter/plugin.toml"
  script "$S/src/granter/bin/server" granter
  ls_ plugin install "$S/src/granter" > /dev/null
  H=$D/plugins/handmade
  manifest "$H"
  script "$H/bin/server" handmade
  mkdir -p "$H/.git/hooks"
  script "$H/.git/hooks/post-checkout" hook
  E=$D/plugins/edited
  manifest "$E"
  script "$E/bin/server" reviewed
  Q=$D/plugins/dropped
  folder() { (cd "$1" && find . -print0 | sort -z | xargs -0 ls -ld | awk '{ print $1, $NF }'; find . -type f -print0 | sort -z | xargs -0 shasum) | shasum | cut -c1-16; }
  kept=$(folder "$H")
  hook="printf '#!/bin/sh\necho swapped\n' > $E/bin/server && mkdir -p $Q/bin && printf 'id = \"dropped\"\nversion = \"1.0.0\"\ndescription = \"Dropped in\"\ncapabilities = [\"mcp-server\"]\n[mcp]\ncommand = \"bin/server\"\n' > $Q/plugin.toml && printf '#!/bin/sh\n' > $Q/bin/server && chmod 755 $Q/bin/server"
  out=$(CONSENT_DRIVER_HOOK=$hook drive scan consent:granter@1.0.0=install consent:handmade@1.0.0=cancel consent:edited@1.0.0=install)
  echo "$out" | grep -v '^ROW' | sed 's/^/  /'
  echo "  grants: granter '$(granted granter)', handmade '$(granted handmade)', edited '$(granted edited)'; handmade folder $kept before, $(folder "$H") after"
  if [ "$(echo "$out" | grep -A1 '^OPEN consent:granter@1.0.0$' | sed -n 's/^SHEET .* rows=//p')" = "mcp-server:needed,ask-user:needed,screen-capture:sensitive" ]; then
    pass "first-F1 a plugin installed with plugin install gets a consent:<id>@<version> sheet at startup"
  else
    fail "first-F1 a plugin installed with plugin install gets a consent:<id>@<version> sheet at startup"
  fi
  if echo "$out" | grep -q '^DONE consent:granter@1.0.0: Granter 1.0.0 has 2 new permissions, but it is off: .*(local-studio plugin grant granter screen-capture)$' \
    && [ "$(granted granter)" = "ask-user,mcp-server" ]; then
    pass "first-F1 Install on the scan sheet grants the needed rows and says the plugin stays off"
  else
    fail "first-F1 Install on the scan sheet grants the needed rows and says the plugin stays off"
  fi
  if echo "$out" | grep -q '^FAILED consent:edited@1.0.0: edited changed after the sheet opened, so nothing was granted$' \
    && [ "$(granted edited)" = none ]; then
    pass "first-F1 a file edited between the offer and the answer is refused"
  else
    fail "first-F1 a file edited between the offer and the answer is refused"
  fi
  if echo "$out" | grep -q '^DONE consent:handmade@1.0.0: Nothing installed or granted for handmade$' \
    && [ "$(granted handmade)" = none ] && [ "$(folder "$H")" = "$kept" ]; then
    pass "first-F1 Cancel on the scan sheet leaves the plugin untouched"
  else
    fail "first-F1 Cancel on the scan sheet leaves the plugin untouched"
  fi
  if echo "$out" | grep -q '^OPEN-AFTER consent:dropped@1.0.0$'; then
    pass "first-F1 the scan after the answers offers only the new plugin, no answered sheet again"
  else
    fail "first-F1 the scan after the answers offers only the new plugin, no answered sheet again"
  fi
  if echo "$out" | grep -A2 '^OPEN consent:handmade@1.0.0$' | grep -q '^SHEET .* findings=1 ' \
    && echo "$out" | grep -q '^FINDING executable not named in the manifest: .git/hooks/post-checkout$'; then
    pass "sol-F2 the scan sheet lists an executable under .git"
  else
    fail "sol-F2 the scan sheet lists an executable under .git"
  fi
  echo "== sol-F1: the staged copy changes while the install sheet is open"
  for change in edit add; do
    fresh "f1$change"
    mkdir -p "$S/src/staged/bin"
    printf 'id = "staged"\nversion = "1.0.0"\ndescription = "Staged"\ncapabilities = ["mcp-server"]\n[mcp]\ncommand = "bin/server"\n' > "$S/src/staged/plugin.toml"
    script "$S/src/staged/bin/server" reviewed
    if [ "$change" = edit ]; then
      hook="for f in $D/plugins/.install-*/staged/bin/server; do printf '#!/bin/sh\necho swapped\n' > \"\$f\"; done"
    else
      hook="for d in $D/plugins/.install-*/staged/bin; do printf '#!/bin/sh\necho added\n' > \"\$d/extra\"; chmod 755 \"\$d/extra\"; done"
    fi
    out=$(CONSENT_DRIVER_HOOK=$hook drive "$S/src/staged" install)
    echo "$out" | grep -v '^ROW' | sed 's/^/  /'
    echo "  installed: $(ls "$D/plugins/staged/bin" 2> /dev/null | tr '\n' ' ')server says '$(tail -1 "$D/plugins/staged/bin/server" 2> /dev/null)'; grants $(granted staged)"
    if echo "$out" | grep -q '^HOOK exit status: 0$' && echo "$out" | grep -q '^FAILED .*changed after' \
      && [ ! -e "$D/plugins/staged" ] && [ "$(granted staged)" = none ]; then
      pass "sol-F1 a staged copy that gets an $change while the sheet is open is refused, nothing installed or granted"
    else
      fail "sol-F1 a staged copy that gets an $change while the sheet is open is refused, nothing installed or granted"
    fi
  done
fi

echo "failures: $failures"
exit "$failures"
