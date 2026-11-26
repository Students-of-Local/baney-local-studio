#!/usr/bin/env nu

const skipped_actions = ["actions/checkout" "Swatinem/rust-cache" "cachix/install-nix-action"]
const pinned_tools = {"mlugg/setup-zig": "zig" "hustcer/setup-nu": "nu"}

def tool-version [tool: string, bin: string] {
    if $tool == "zig" { ^$bin version | str trim } else { ^$bin --version | str trim }
}

def find-tool [tool: string, version: string, --series] {
    let on_path = (which $tool | where type == external | get -o 0.path)
    let opt = (glob ($env.HOME | path join ".local" "opt" $"($tool)-*" $tool))
    let candidates = ([$on_path] | append $opt | compact | uniq | where {|p| $p | path exists })
    let fits = ($candidates | where {|p|
        let v = (tool-version $tool $p)
        if $series { $v | str starts-with $"($version)." } else { $v == $version }
    })
    if ($fits | is-empty) {
        let want = (if $series { $"($version).x" } else { $version })
        error make {msg: $"($tool) ($want) is not installed here; put it on PATH or in ~/.local/opt/($tool)-<version>/"}
    }
    $fits.0 | path dirname
}

def nix-shell-tools [flake: string] {
    if not ($flake | path exists) { return [] }
    open --raw $flake | parse --regex 'pkgs\.zig_(?<major>\d+)_(?<minor>\d+)' | each {|z| find-tool zig $"($z.major).($z.minor)" --series } | uniq
}

def post [repo: string, sha: string, context: string, state: string, description: string] {
    let text = ($description | str substring 0..139)
    ^gh api $"repos/($repo)/statuses/($sha)" -f $"state=($state)" -f $"context=($context)" -f $"description=($text)" | ignore
}

def run-step [cmd: string, cwd: string, log: string, envs: record] {
    let file = (mktemp -t local-ci-step.XXXXXX)
    $cmd | save --force $file
    $"\n$ ($cmd | lines | first)\n" | save --append $log
    let code = (with-env $envs {
        ^bash --noprofile --norc -c 'exec >>"$1" 2>&1; cd "$2" && exec bash --noprofile --norc -eo pipefail "$3"' local-ci $log $cwd $file | complete | get exit_code
    })
    rm $file
    $code
}

def unsupported [job: record] {
    let job_keys = ($job | columns | where {|k| $k in ["if" "defaults" "container" "services" "strategy"] })
    let step_keys = ($job.steps | each {|s|
        let shell = ($s | get -o shell | default "bash")
        let keys = ($s | columns | where {|k| $k == "if" })
        if $shell != "bash" { $keys | append $"shell: ($shell)" } else { $keys }
    } | flatten)
    $job_keys | append $step_keys | uniq
}

def run-job [id: string, job: record, root: string, sha: string, log: string, has_nix: bool, workflow_env: record] {
    "" | save --force $log
    let refused = (unsupported $job)
    if ($refused | is-not-empty) {
        $"local CI can't reproduce: ($refused | str join ', ')\n" | save --append $log
        return {code: 1, notes: []}
    }
    let temp = (mktemp -d -t local-ci-runner.XXXXXX)
    mut path = $env.PATH
    mut notes = []
    mut code = 0
    let base = {
        CI: "true"
        GITHUB_SHA: $sha
        GITHUB_WORKSPACE: $root
        RUNNER_TEMP: $temp
        CARGO_TARGET_DIR: ($env.HOME | path join ".cache" "local-ci" $"target-($id)")
    }
    for step in ($job.steps) {
        let uses = ($step.uses? | default "")
        if $uses != "" {
            let action = ($uses | split row "@" | first)
            if $action in $skipped_actions { continue }
            if $action in $pinned_tools {
                let tool = ($pinned_tools | get $action)
                let dir = (try { find-tool $tool ($step.with.version | into string) } catch {|e| $"($e.msg)\n" | save --append $log; null })
                if $dir == null { $code = 1; break }
                $path = ($path | prepend $dir)
                continue
            }
            $"local CI can't run the action ($uses)\n" | save --append $log
            $code = 1
            break
        }
        mut cmd = ($step.run? | default "")
        if ($cmd | str contains '${{') {
            $"local CI can't evaluate an expression in: ($cmd | lines | first)\n" | save --append $log
            $code = 1
            break
        }
        let shell = ($cmd | parse --regex '^nix develop (?<flake>\S+) --command ')
        if not $has_nix and ($shell | is-not-empty) {
            let flake = ($root | path join $shell.0.flake "flake.nix")
            let tools = (try { nix-shell-tools $flake } catch {|e| $"($e.msg)\n" | save --append $log; null })
            if $tools == null { $code = 1; break }
            $path = ($path | prepend $tools)
            $cmd = ($cmd | str replace --regex '^nix develop \S+ --command ' "")
            $notes = ($notes | append "host libs for the nix shell")
        }
        let cwd = ($root | path join ($step | get -o "working-directory" | default "."))
        let envs = ($base | merge $workflow_env | merge ($job.env? | default {}) | merge ($step.env? | default {}) | merge {PATH: $path})
        if ($envs | values | any {|v| $v | into string | str contains '${{' }) {
            $"local CI can't evaluate an expression in the env of: ($cmd | lines | first)\n" | save --append $log
            $code = 1
            break
        }
        let step_code = (run-step $cmd $cwd $log $envs)
        if $step_code != 0 and not ($step | get -o "continue-on-error" | default false) {
            $code = $step_code
            break
        }
    }
    rm -rf $temp
    {code: $code, notes: ($notes | uniq)}
}

def main [
    pr: int
    --repo: string = "Students-of-Local/baney-local-studio"
    --owner: string = "baney75"
    --post
] {
    let info = (^gh pr view $pr --repo $repo --json headRefOid,isCrossRepository,author,state | from json)
    if $info.isCrossRepository or $info.author.login != $owner {
        error make {msg: $"local CI runs only the owner's own branches; #($pr) is from ($info.author.login)"}
    }
    if $info.state != "OPEN" { error make {msg: $"#($pr) is ($info.state)"} }
    let sha = $info.headRefOid
    let checkout = (^git rev-parse --show-toplevel | str trim)
    ^git -C $checkout fetch --quiet origin $"pull/($pr)/head"
    if (^git -C $checkout rev-parse FETCH_HEAD | str trim) != $sha {
        error make {msg: $"#($pr) moved while starting; run again"}
    }
    let logs = ($env.HOME | path join ".local" "state" "local-ci" $sha)
    mkdir $logs
    let host = (sys host | get hostname)
    let has_nix = (which nix | is-not-empty)
    let workflow = (^git -C $checkout show $"($sha):.github/workflows/ci.yml" | from yaml)
    if ($workflow | get -o defaults | is-not-empty) { error make {msg: "local CI can't reproduce ci.yml's defaults"} }
    let workflow_env = ($workflow | get -o env | default {})
    let jobs = ($workflow.jobs | transpose id job)
    mut failed = []
    for entry in $jobs {
        let name = ($entry.job.name? | default $entry.id)
        let runs_on = ($entry.job | get "runs-on" | into string)
        if not ($runs_on | str starts-with "ubuntu") {
            print $"skip  ($name): needs ($runs_on)"
            continue
        }
        let context = $"local/($name)"
        let log = ($logs | path join $"($entry.id).log")
        if $post { post $repo $sha $context "pending" $"running on ($host)" }
        let started = (date now)
        let root = (mktemp -d -t local-ci.XXXXXX)
        ^git -C $checkout worktree add --quiet --detach $root $sha
        let result = (run-job $entry.id $entry.job $root $sha $log $has_nix $workflow_env)
        ^git -C $checkout worktree remove --force $root
        let secs = (((date now) - $started) / 1sec | math round)
        let extra = (if ($result.notes | is-empty) { "" } else { $", ($result.notes | str join ', ')" })
        let state = (if $result.code == 0 { "success" } else { "failure" })
        if $result.code != 0 { $failed = ($failed | append $name) }
        if $post { post $repo $sha $context $state $"($host), ($secs)s($extra)" }
        print $"($state)  ($name) in ($secs)s, log ($log)"
    }
    if ($failed | is-not-empty) {
        error make {msg: $"failed on ($sha | str substring 0..6): ($failed | str join ', ')"}
    }
}
