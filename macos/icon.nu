#!/usr/bin/env nu

export def source-hash [package: path]: nothing -> string {
    let icon = ($package | path join "AppIcon.icon")
    glob ($icon | path join "**" "*") --no-dir | sort | each {|f| $"($f | path relative-to $icon) (open --raw $f | hash sha256)" } | str join "\n" | hash sha256
}

def main [] {
    let package = ($env.FILE_PWD)
    let prebuilt = ($package | path join "AppIcon.prebuilt")
    let out = (mktemp -d)
    let partial = (mktemp -t "local-studio-icon.XXXXXX")
    let result = (do { ^xcrun actool ($package | path join "AppIcon.icon") --compile $out --platform macosx --minimum-deployment-target 26.0 --app-icon AppIcon --output-partial-info-plist $partial --output-format human-readable-text } | complete)
    rm -f $partial
    if $result.exit_code != 0 or not ($out | path join "Assets.car" | path exists) {
        rm -r -f $out
        error make { msg: $"actool could not compile AppIcon.icon:\n($result.stdout)($result.stderr)" }
    }
    mkdir $prebuilt
    cp ($out | path join "Assets.car") ($out | path join "AppIcon.icns") $prebuilt
    rm -r -f $out
    source-hash $package | save --force ($prebuilt | path join "source.sha256")
    print $"wrote ($prebuilt) with (^xcodebuild -version | lines | first)"
}
