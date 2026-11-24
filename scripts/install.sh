#!/usr/bin/env bash
set -euo pipefail

repo=baney75/local-studio
nightly_key=dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDc4QjIxNzIyMDY2NkJFNkMKUldSc3ZtWUdJaGV5ZU1zUnB3bi9heW1vdFlnMllXZG9LOTBnTGhNL0VLQ21OdGRsN1dHcEx5SWkK
stable_key=dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDE1NkMxREQ1MDdBNzBDNzUKUldSMURLY0gxUjFzRlF0L0hnZEdRanI0WHpYTmlzWml4VlB4cCtKY1JDNU5RZ241M0k4Um9RM3MK

usage() {
  cat <<EOF
Install Local Studio from its signed release feed.

  curl -fsSL https://raw.githubusercontent.com/$repo/main/scripts/install.sh | bash -s -- --channel nightly

Options:
  --channel nightly|stable|dev   which channel to install (default nightly)
  --prefix DIR                   install under DIR instead of /Applications (macOS) or ~/.local (Linux)
  --no-open                      do not open the app after installing
  --feed URL --public-key KEY    install from another feed signed with KEY (for testing a private feed)

The download is installed only when its sha256 matches the feed and its minisign signature
verifies against the channel's key built into this script. Verifying needs one of minisign,
OpenSSL 3 or python3.
EOF
}

die() {
  echo "install.sh: $*" >&2
  exit 1
}

say() { echo "==> $*"; }

channel=nightly
prefix=
feed=
public_key=
open_app=true
while [ $# -gt 0 ]; do
  case $1 in
    --channel) [ $# -ge 2 ] || die "--channel needs a value"; channel=$2; shift ;;
    --channel=*) channel=${1#*=} ;;
    --prefix) [ $# -ge 2 ] || die "--prefix needs a value"; prefix=$2; shift ;;
    --feed) [ $# -ge 2 ] || die "--feed needs a value"; feed=$2; shift ;;
    --public-key) [ $# -ge 2 ] || die "--public-key needs a value"; public_key=$2; shift ;;
    --no-open) open_app=false ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
  shift
done

case $channel in
  stable) name="Local Studio"; slug=local-studio; bundle_id=org.local.studio.macos; app_id=dev.localstudio.LocalStudio ;;
  nightly) name="Local Studio Nightly"; slug=local-studio-nightly; bundle_id=org.local.studio.macos.nightly; app_id=dev.localstudio.LocalStudio.Nightly ;;
  dev) name="Local Studio Dev"; slug=local-studio-dev; bundle_id=org.local.studio.macos.dev; app_id=dev.localstudio.LocalStudio.Dev ;;
  *) die "unknown channel $channel; expected nightly, stable or dev" ;;
esac

if [ -n "$public_key" ] && [ -z "$feed" ]; then
  die "--public-key only applies with --feed; the release feeds use the keys built into this script"
fi
if [ -z "$feed" ]; then
  case $channel in
    stable) feed="https://github.com/$repo/releases/latest/download/latest.json"; public_key=$stable_key ;;
    *) feed="https://github.com/$repo/releases/download/$channel/latest.json"; public_key=$nightly_key ;;
  esac
fi
[ -n "$public_key" ] || die "--feed needs --public-key"

os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Darwin/arm64) platform=darwin-aarch64; asset=local-studio-darwin-aarch64.app.tar.gz ;;
  Linux/x86_64) platform=linux-x86_64; asset=local-studio-linux-x86_64.tar.gz ;;
  *) die "no Local Studio build for $os $arch yet (macOS on Apple Silicon and Linux x86_64 only)" ;;
esac

for tool in curl tar; do
  command -v "$tool" > /dev/null || die "$tool is required"
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/local-studio-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

b64d() {
  if [ "$os" = Darwin ]; then base64 -D; else base64 -d; fi
}

sha256_of() {
  if command -v sha256sum > /dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

python=
if command -v python3 > /dev/null; then
  if [ "$os" != Darwin ] || xcode-select -p > /dev/null 2>&1; then python=$(command -v python3); fi
fi

allowed_url() {
  case $1 in
    https://*) return 0 ;;
    http://127.0.0.1:* | http://127.0.0.1/* | http://localhost:* | http://localhost/* | http://\[::1\]:* | http://\[::1\]/*) return 0 ;;
    *) return 1 ;;
  esac
}

fetch() {
  curl -fsSL --proto-redir =https --retry 3 -o "$2" "$1"
}

allowed_url "$feed" || die "the feed must be https: $feed"
say "Reading the $channel feed"
code=$(curl -sSL --proto-redir =https -o "$tmp/latest.json" -w '%{http_code}' "$feed") || die "could not reach $feed"
case $code in
  200) ;;
  404) die "no $channel build has been published yet ($feed)" ;;
  *) die "the $channel feed answered HTTP $code" ;;
esac

if command -v jq > /dev/null; then
  jq -r --arg k "$platform" \
    '.product // "", .channel // "", .version // "", .platforms[$k].url // "", .platforms[$k].sha256 // "", .platforms[$k].signature // ""' \
    "$tmp/latest.json" > "$tmp/fields" 2> /dev/null || die "the feed is not JSON"
elif [ -n "$python" ]; then
  "$python" -I -c '
import json, sys
feed = json.load(open(sys.argv[1]))
entry = (feed.get("platforms") or {}).get(sys.argv[2]) or {}
for value in (feed.get("product"), feed.get("channel"), feed.get("version"), entry.get("url"), entry.get("sha256"), entry.get("signature")):
    print(value or "")
' "$tmp/latest.json" "$platform" > "$tmp/fields" 2> /dev/null || die "the feed is not JSON"
else
  die "reading the feed needs jq or python3"
fi
{
  read -r product
  read -r feed_channel
  read -r version
  read -r url
  read -r want_sha
  read -r signature
} < "$tmp/fields"

[ "$product" = local-studio ] || die "the feed is for ${product:-nothing}, not local-studio"
[ "$feed_channel" = "$channel" ] || die "the feed is for the ${feed_channel:-unnamed} channel, not $channel"
case $channel in
  stable) [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(\+.*)?$ ]] ;;
  *) [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+-$channel\. ]] ;;
esac || die "the feed's version ${version:-(none)} is not a $channel version"
[ -n "$url" ] || die "the $channel feed has no build for $platform"
[ "${url##*/}" = "$asset" ] || die "the feed's $platform build is ${url##*/}, not $asset"
allowed_url "$url" || die "the download must be https: $url"
[[ $want_sha =~ ^[0-9a-f]{64}$ ]] || die "the feed's sha256 for $platform is not 64 hex characters"
[ -n "$signature" ] || die "the feed carries no signature for $platform, so it was not installed"

say "Downloading $name $version"
fetch "$url" "$tmp/$asset" || die "could not download $url"
got_sha=$(sha256_of "$tmp/$asset")
[ "$got_sha" = "$want_sha" ] || die "the download does not match its sha256 (feed $want_sha, file $got_sha)"

printf '%s' "$public_key" | tr -d ' \n\r\t' | b64d > "$tmp/key.pub" 2> /dev/null || die "the public key is not base64"
key_line=$(grep -v '^untrusted comment:' "$tmp/key.pub" | grep -v '^$' | sed -n 1p)
printf '%s' "$signature" | tr -d ' \n\r\t' | b64d > "$tmp/asset.minisig" 2> /dev/null || die "the signature is not base64"
sig_line=$(sed -n 2p "$tmp/asset.minisig")
trusted_line=$(sed -n 3p "$tmp/asset.minisig")
global_line=$(sed -n 4p "$tmp/asset.minisig")
case $trusted_line in
  "trusted comment: "*) trusted=${trusted_line#trusted comment: } ;;
  *) die "the signature has no trusted comment" ;;
esac
[ -n "$key_line" ] && [ -n "$sig_line" ] && [ -n "$global_line" ] || die "the key or signature is incomplete"

hex_of() { od -An -tx1 -v | tr -d ' \n'; }
key_hex=$(printf '%s' "$key_line" | b64d | hex_of)
sig_hex=$(printf '%s' "$sig_line" | b64d | hex_of)
[ ${#key_hex} -eq 84 ] && [ "${key_hex:0:4}" = 4564 ] || die "the public key is not a minisign Ed25519 key"
[ ${#sig_hex} -eq 148 ] || die "the signature line has the wrong length"
[ "${sig_hex:0:4}" = 4544 ] || die "the signature is not a prehashed minisign signature"
[ "${sig_hex:4:16}" = "${key_hex:4:16}" ] || die "the download was signed with another key than this channel's"
signed_file=$(printf '%s' "$trusted" | tr '\t' '\n' | sed -n 's/^file://p')
[ "$signed_file" = "$asset" ] || die "the signature was made for ${signed_file:-another file}, not $asset"

openssl3=
for candidate in openssl /opt/homebrew/bin/openssl /opt/homebrew/opt/openssl@3/bin/openssl /usr/local/opt/openssl@3/bin/openssl /usr/local/bin/openssl; do
  path=$(command -v "$candidate" 2> /dev/null) || continue
  case $("$path" version 2> /dev/null) in
    "OpenSSL 3."*) openssl3=$path; break ;;
  esac
done

verify_minisign() {
  minisign -Vq -P "$key_line" -m "$tmp/$asset" -x "$tmp/asset.minisig" > /dev/null 2>&1
}

verify_openssl() {
  printf '%s' "$key_line" | b64d | tail -c 32 > "$tmp/raw.pub"
  { printf '\060\052\060\005\006\003\053\145\160\003\041\000'; cat "$tmp/raw.pub"; } > "$tmp/key.der"
  printf '%s' "$sig_line" | b64d | tail -c 64 > "$tmp/sig.bin"
  printf '%s' "$global_line" | b64d > "$tmp/global.bin"
  "$openssl3" dgst -blake2b512 -binary "$tmp/$asset" > "$tmp/digest.bin"
  { cat "$tmp/sig.bin"; printf '%s' "$trusted"; } > "$tmp/global.msg"
  "$openssl3" pkeyutl -verify -pubin -keyform DER -inkey "$tmp/key.der" -rawin -in "$tmp/digest.bin" -sigfile "$tmp/sig.bin" > /dev/null 2>&1 &&
    "$openssl3" pkeyutl -verify -pubin -keyform DER -inkey "$tmp/key.der" -rawin -in "$tmp/global.msg" -sigfile "$tmp/global.bin" > /dev/null 2>&1
}

verify_python() {
  "$python" -I -c '
import base64, hashlib, sys
p = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
d = -121665 * pow(121666, p - 2, p) % p
root = pow(2, (p - 1) // 4, p)
def point(raw):
    y = int.from_bytes(raw, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    if y >= p:
        return None
    u = (y * y - 1) % p
    v = (d * y * y + 1) % p
    x2 = u * pow(v, p - 2, p) % p
    x = pow(x2, (p + 3) // 8, p)
    if (x * x - x2) % p:
        x = x * root % p
    if (x * x - x2) % p:
        return None
    if x == 0 and sign:
        return None
    if x & 1 != sign:
        x = p - x
    return (x, y, 1, x * y % p)
def add(a, b):
    e = (a[1] - a[0]) * (b[1] - b[0]) % p
    f = (a[1] + a[0]) * (b[1] + b[0]) % p
    g = 2 * a[3] * b[3] * d % p
    h = 2 * a[2] * b[2] % p
    i, j, k, l = f - e, h - g, h + g, f + e
    return (i * j % p, k * l % p, j * k % p, i * l % p)
def times(s, a):
    q = (0, 1, 1, 0)
    while s:
        if s & 1:
            q = add(q, a)
        a = add(a, a)
        s >>= 1
    return q
def same(a, b):
    return (a[0] * b[2] - b[0] * a[2]) % p == 0 and (a[1] * b[2] - b[1] * a[2]) % p == 0
base = point((4 * pow(5, p - 2, p) % p).to_bytes(32, "little"))
def verify(public, message, signature):
    a = point(public)
    r = point(signature[:32])
    s = int.from_bytes(signature[32:], "little")
    if a is None or r is None or s >= L:
        return False
    h = int.from_bytes(hashlib.sha512(signature[:32] + public + message).digest(), "little") % L
    return same(times(s, base), add(r, times(h, a)))
key = base64.b64decode(sys.argv[1])[10:]
signature = base64.b64decode(sys.argv[2])[10:]
digest = hashlib.blake2b()
with open(sys.argv[5], "rb") as handle:
    for chunk in iter(lambda: handle.read(1 << 20), b""):
        digest.update(chunk)
ok = verify(key, digest.digest(), signature) and verify(key, signature + sys.argv[3].encode(), base64.b64decode(sys.argv[4]))
sys.exit(0 if ok else 1)
' "$key_line" "$sig_line" "$trusted" "$global_line" "$tmp/$asset"
}

verifier=${LOCAL_STUDIO_INSTALL_VERIFIER:-}
if [ -z "$verifier" ]; then
  if command -v minisign > /dev/null; then verifier=minisign
  elif [ -n "$openssl3" ]; then verifier=openssl
  elif [ -n "$python" ]; then verifier=python3
  elif [ "$os" = Darwin ]; then die "checking the signature needs minisign, OpenSSL 3 or python3: run xcode-select --install (python3) or brew install minisign, then run this again"
  else die "checking the signature needs minisign, OpenSSL 3 or python3; install one and run this again"
  fi
fi
say "Checking the signature ($verifier)"
case $verifier in
  minisign) command -v minisign > /dev/null || die "minisign is not installed"; verify_minisign ;;
  openssl) [ -n "$openssl3" ] || die "OpenSSL 3 is not installed"; verify_openssl ;;
  python3) [ -n "$python" ] || die "python3 is not installed"; verify_python ;;
  *) die "unknown verifier $verifier" ;;
esac || die "the signature does not verify against the $channel key, so nothing was installed"

mkdir "$tmp/unpacked"
tar -xzf "$tmp/$asset" -C "$tmp/unpacked" || die "could not unpack $asset"

only_entry() {
  local found=() entry
  for entry in "$tmp/unpacked"/*; do
    case ${entry##*/} in ._*) continue ;; esac
    [ -d "$entry" ] && found+=("$entry")
  done
  [ ${#found[@]} -eq 1 ] || die "the download holds ${#found[@]} folders, not one"
  printf '%s\n' "${found[0]}"
}

swap_in() {
  local incoming=$1 target=$2 previous=$3
  if [ -e "$target" ]; then
    rm -rf "$previous"
    mv "$target" "$previous" || die "could not move the old $name aside"
  fi
  if ! mv "$incoming" "$target"; then
    [ -e "$previous" ] && mv "$previous" "$target"
    die "could not put $name in place"
  fi
}

expect_version="local-studio $version ($channel)"

if [ "$os" = Darwin ]; then
  app=$(only_entry)
  [ "${app##*.}" = app ] || die "the download holds no app"
  plist=$app/Contents/Info.plist
  field() { /usr/libexec/PlistBuddy -c "Print :$1" "$plist" 2> /dev/null || true; }
  [ "$(field CFBundleIdentifier)" = "$bundle_id" ] || die "the app's bundle id is $(field CFBundleIdentifier), not $bundle_id"
  [ "$(field LocalStudioChannel)" = "$channel" ] || die "the app is built for the $(field LocalStudioChannel) channel, not $channel"
  [ "$(field LocalStudioVersion)" = "$version" ] || die "the app is version $(field LocalStudioVersion), not the feed's $version"
  codesign --verify --deep --strict "$app" 2> "$tmp/codesign.log" || die "the app's code signature does not verify: $(cat "$tmp/codesign.log")"
  requirement=$(codesign -d -r- "$app" 2>&1 | sed -n 's/^designated => //p')
  case $requirement in
    *certificate*) ;;
    *) die "the app is not signed with a certificate ($requirement)" ;;
  esac
  if [ -n "$prefix" ]; then
    apps=$prefix/Applications
    mkdir -p "$apps"
  else
    apps=/Applications
    [ -w "$apps" ] || { apps=$HOME/Applications; mkdir -p "$apps"; }
  fi
  target="$apps/$name.app"
  if [ -d "$target" ]; then
    installed=$(codesign -d -r- "$target" 2>&1 | sed -n 's/^designated => //p')
    case $installed in
      *certificate*) codesign --verify --deep --strict -R "=$installed" "$app" 2> /dev/null ||
        die "the download is signed by another identity than the installed $name, so it was not installed" ;;
    esac
  fi
  incoming="$apps/.$name.app.incoming"
  rm -rf "$incoming"
  ditto "$app" "$incoming"
  xattr -dr com.apple.quarantine "$incoming" 2> /dev/null || true
  codesign --verify --deep --strict "$incoming" 2> /dev/null || { rm -rf "$incoming"; die "the copied app no longer verifies"; }
  reported=$("$incoming/Contents/MacOS/$(field CFBundleExecutable)" --version 2> "$tmp/version.log" | sed -n 1p) || true
  [ "$reported" = "$expect_version" ] || { rm -rf "$incoming"; die "the app reports ${reported:-nothing}, not $expect_version $(sed -n 1p "$tmp/version.log")"; }
  swap_in "$incoming" "$target" "$apps/.$name.app.previous"
  say "Installed $name $version in $target"
  cli=$target/Contents/MacOS/local-studio-cli
  bin=${prefix:-$HOME/.local}/bin
  link=$bin/local-studio
  if [ -x "$cli" ]; then
    if [ -L "$link" ]; then
      case $(readlink "$link") in
        */"Local Studio"*.app/Contents/*) linkable=true ;;
        *) linkable=false ;;
      esac
    else
      [ -e "$link" ] && linkable=false || linkable=true
    fi
    if [ "$linkable" = true ]; then
      mkdir -p "$bin"
      ln -sfn "$cli" "$link"
      say "Linked $link to $cli"
      case ":$PATH:" in
        *":$bin:"*) ;;
        *) echo "    $bin is not on PATH; add it to run local-studio from a terminal" ;;
      esac
    else
      echo "    $link exists and is not a Local Studio link, so it was left alone; the command line tool is $cli"
    fi
  fi
  if [ "$open_app" = true ]; then open "$target"; fi
else
  bundle=$(only_entry)
  [ -x "$bundle/local-studio" ] && [ -x "$bundle/eukhe" ] || die "the download holds no local-studio bundle"
  reported=$("$bundle/local-studio" --version 2> "$tmp/version.log" | sed -n 1p) || true
  [ -n "$reported" ] || die "the build does not run on this system: $(sed -n 1p "$tmp/version.log")"
  [ "$reported" = "$expect_version" ] || die "the build reports $reported, not $expect_version"
  root=${prefix:-$HOME/.local}
  mkdir -p "$root/lib" "$root/bin" "$root/share/applications"
  target=$root/lib/$slug
  incoming=$root/lib/.$slug.incoming
  rm -rf "$incoming"
  cp -a "$bundle" "$incoming"
  swap_in "$incoming" "$target" "$root/lib/.$slug.previous"
  ln -sfn "$target/local-studio" "$root/bin/$slug"
  cat > "$root/share/applications/$app_id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$name
Exec="$target/local-studio"
Terminal=false
Categories=Development;
StartupWMClass=$app_id
EOF
  say "Installed $name $version in $target (command: $root/bin/$slug)"
  case ":$PATH:" in
    *":$root/bin:"*) ;;
    *) echo "    $root/bin is not on PATH; add it to run $slug from a terminal" ;;
  esac
  if [ "$open_app" = true ] && { [ -n "${WAYLAND_DISPLAY:-}" ] || [ -n "${DISPLAY:-}" ]; }; then
    nohup "$target/local-studio" > /dev/null 2>&1 < /dev/null &
  fi
fi
