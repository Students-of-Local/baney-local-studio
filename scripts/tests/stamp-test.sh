#!/usr/bin/env bash
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$scratch/target}
commit=$(git -C "$root" rev-parse HEAD)
sources=(Cargo.toml macos/LocalStudio-Info.plist linux/src/main.rs)
before=$(cd "$root" && cat "${sources[@]}" | git hash-object --stdin)
status=0
fail() { echo "stamp-test: $*" >&2; status=1; }
report() { env "$@" cargo run -q --manifest-path "$root/Cargo.toml" -p local-studio-version --example version; }

for channel in stable nightly dev local; do
  dir=$scratch/stamp-$channel
  mkdir -p "$dir/macos" "$dir/linux/src"
  for source in "${sources[@]}"; do cp "$root/$source" "$dir/$source"; done
  identity=$(nu "$root/scripts/version.nu" "$channel" --stamp --json --root "$dir" --at 20261118070000 --commit "$commit")
  version=$(jq -r .version <<<"$identity")
  build_channel=$(jq -r .build_channel <<<"$identity")
  plist=$(tr -d '\n\t ' <"$dir/macos/LocalStudio-Info.plist")
  for field in name bundle_id scheme bundle_version base; do
    value=$(jq -r ".$field" <<<"$identity" | tr -d ' ')
    grep -qF "<string>$value</string>" <<<"$plist" || fail "$channel: the Info.plist lacks $field $value"
  done
  grep -qF "<key>LocalStudioVersion</key><string>$version</string>" <<<"$plist" || fail "$channel: the Info.plist LocalStudioVersion is not $version"
  grep -qF "<key>LocalStudioChannel</key><string>$build_channel</string>" <<<"$plist" || fail "$channel: the Info.plist LocalStudioChannel is not $build_channel"
  grep -qF "application_id(\"$(jq -r .app_id <<<"$identity")\")" "$dir/linux/src/main.rs" || fail "$channel: linux/src/main.rs lacks the app id"
  exports=()
  while IFS= read -r line; do exports+=("$line"); done < <(nu "$root/scripts/version.nu" "$channel" --build-env --at 20261118070000 --commit "$commit")
  reported=$(report "${exports[@]}")
  echo "$channel: stamped $version, the build reports $reported"
  [ "$reported" = "$version $build_channel" ] || fail "$channel: a build stamped $version reports $reported"
done

local_version=$(nu "$root/scripts/version.nu" local)
reported=$(report -u LOCAL_STUDIO_VERSION -u LOCAL_STUDIO_CHANNEL)
echo "unstamped: the build reports $reported"
[ "$reported" = "$local_version dev" ] || fail "an unstamped build reports $reported, not $local_version dev"

after=$(cd "$root" && cat "${sources[@]}" | git hash-object --stdin)
[ "$before" = "$after" ] || fail "stamping with --root changed the checkout"
exit $status
