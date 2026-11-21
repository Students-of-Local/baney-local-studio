#!/usr/bin/env nu

const tools = {
    xcodegen: {
        version: "2.46.0"
        url: "https://github.com/yonaskolb/XcodeGen/releases/download/2.46.0/xcodegen.zip"
        sha256: "4d9e34b62172d645eed6457cac13fc222569974098ef4ee9c3368bedf0196806"
        binary: ["xcodegen" "bin" "xcodegen"]
    }
    swiftlint: {
        version: "0.65.1"
        url: "https://github.com/realm/SwiftLint/releases/download/0.65.1/portable_swiftlint.zip"
        sha256: "c1e429b0599cf1b516f369a2d9ec04eaf0e436f3c12b637df8851fa52ff694d0"
        binary: ["swiftlint"]
    }
}

const schemes = ["LocalStudio" "LocalStudio Nightly" "LocalStudio Dev"]

const strict = [
    "-destination" "generic/platform=iOS Simulator"
    "CODE_SIGNING_ALLOWED=NO"
    "SWIFT_TREAT_WARNINGS_AS_ERRORS=YES"
    "GCC_TREAT_WARNINGS_AS_ERRORS=YES"
    "build-for-testing"
]

def tool [ios: path, name: string]: nothing -> path {
    let spec = ($tools | get $name)
    let dir = ($ios | path join "build" "tools" $"($name)-($spec.version)")
    let binary = ($dir | path join ...$spec.binary)
    if ($binary | path exists) {
        return $binary
    }
    let partial = $"($dir).partial"
    rm -r -f $partial
    mkdir $partial
    let archive = ($partial | path join "download.zip")
    print -e $"fetching ($spec.url)"
    http get --raw $spec.url | save -f $archive
    let actual = (open --raw $archive | hash sha256)
    if $actual != $spec.sha256 {
        rm -r -f $partial
        error make { msg: $"($name) ($spec.version): sha256 ($actual) does not match the pinned ($spec.sha256)" }
    }
    ^ditto -x -k $archive $partial
    rm $archive
    mv $partial $dir
    $binary
}

def tree-state [ios: path]: nothing -> string {
    let status = (^git -C $ios status --porcelain --untracked-files=all -- .)
    let diff = (^git -C $ios diff -- .)
    $"($status)\n($diff)"
}

def generate [ios: path] {
    let xcodegen = (tool $ios xcodegen)
    let before = (tree-state $ios)
    ^$xcodegen generate --quiet --spec ($ios | path join "project.yml") --project $ios
    if (tree-state $ios) != $before {
        ^git -C $ios status --short -- .
        error make { msg: "xcodegen generate changed ios/: run `xcodegen generate` in ios/ and commit the result" }
    }
}

def lint [ios: path] {
    let swiftlint = (tool $ios swiftlint)
    cd $ios
    ^$swiftlint lint --strict --quiet --config ($ios | path join ".swiftlint.yml")
}

def build-packages [ios: path, derived: path] {
    for manifest in (glob ($ios | path join "Packages" "*" "Package.swift") | sort) {
        let package = ($manifest | path dirname)
        cd $package
        let libraries = (^swift package dump-package | from json | get products | where { |product| "library" in ($product.type | columns) } | get name)
        for library in $libraries {
            print -e $"building package ($package | path basename) library ($library)"
            ^xcodebuild -quiet -scheme $library -derivedDataPath $derived ...$strict
        }
    }
}

def build-app [ios: path, derived: path] {
    for scheme in $schemes {
        print -e $"building scheme ($scheme)"
        ^xcodebuild -quiet -project ($ios | path join "LocalStudio.xcodeproj") -scheme $scheme -derivedDataPath $derived ...$strict
    }
}

def main [] {
    let ios = $env.FILE_PWD
    let derived = ($ios | path join "build" "DerivedData")
    generate $ios
    lint $ios
    build-packages $ios $derived
    build-app $ios $derived
    print -e "ios: generated project current, lint clean, packages and app schemes built with zero warnings"
}

def "main snapshots" [out: path] {
    let out = ($out | path expand)
    let package = ($env.FILE_PWD | path join "Packages" "LocalStudioDesign")
    mkdir $out
    cd $package
    ^swift run -c release design-snapshots $out
}
