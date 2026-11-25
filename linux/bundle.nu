#!/usr/bin/env nu

use ../eukhe.nu

def unstore [binary: path] {
    let folder = if $nu.os-info.arch == "aarch64" { "/lib" } else { "/lib64" }
    let loader = ($folder | path join (^patchelf --print-interpreter $binary | str trim | path basename))
    ^patchelf --set-interpreter $loader --remove-rpath $binary
    let left = ([--print-interpreter --print-rpath] | each {|part| ^patchelf $part $binary | str trim })
    if $left != [$loader ""] {
        error make { msg: $"($binary) still loads from the build machine: ($left | str join ' ')" }
    }
}

def main [--profile (-p): string = "debug"] {
    let linux = $env.FILE_PWD
    let repo = ($linux | path dirname)
    let target_root = if ($env.CARGO_TARGET_DIR? | is-not-empty) { $env.CARGO_TARGET_DIR | path expand } else { $linux | path join "target" }
    let app = ($target_root | path join $profile "local-studio")
    if not ($app | path exists) {
        error make { msg: $"($app) is missing: run `cargo build` first" }
    }
    let tool_target = ($target_root | path join "cli")
    let tool_profile = if $profile == "debug" { "dev" } else { $profile }
    ^cargo build --manifest-path ($repo | path join "Cargo.toml") -p local-studio-core --bin local-studio --features link --profile $tool_profile --target-dir $tool_target
    let tool = ($tool_target | path join $profile "local-studio")
    let cli = (eukhe binary $repo ($target_root | path join "eukhe"))
    let sidecars = (eukhe sidecars $cli)
    if ($sidecars | is-empty) {
        error make { msg: $"no eukhe-runtime found beside or above ($cli)" }
    }
    let bundle = ($target_root | path join $profile "bundle")
    rm -r -f $bundle
    mkdir $bundle
    for binary in [$app $cli] {
        cp $binary $bundle
    }
    cp $tool ($bundle | path join "local-studio-cli")
    if $profile == "release" and ($env.IN_NIX_SHELL? | is-not-empty) {
        for binary in ["local-studio" "local-studio-cli"] { unstore ($bundle | path join $binary) }
    }
    for sidecar in $sidecars {
        eukhe copy-sidecar $sidecar $bundle
    }
    let icons = ($bundle | path join "share" "icons" "hicolor" "scalable" "apps")
    mkdir $icons
    cp ($linux | path join "assets" "mark.svg") ($icons | path join "dev.localstudio.LocalStudio.svg")
    let shared = ($bundle | path join "share" "local-studio")
    mkdir $shared
    cp -r ($repo | path join "browser" "skills") ($shared | path join "app-skills")
    let controller = ($repo | path join "controller")
    if ($controller | path join "src" "main.ts" | path exists) {
        let target = ($shared | path join "controller")
        mkdir $target
        cp -r ($controller | path join "src") $target
        for file in ["package.json" "bun.lock" "tsconfig.json"] {
            let path = ($controller | path join $file)
            if ($path | path exists) { cp $path $target }
        }
    }
    print $bundle
}
