# Release channels

Every product in the Local Studio family ships on three channels: **stable**, **nightly** and **dev**. This file is the one scheme for all of them: Local Studio (desktop, iPhone, and browser and computer use), Silas, Polycarp, Fonsviva, Gantrel, UntoCaesar and Corunis. A product's own RELEASES.md may add detail; where it disagrees with this file, this file wins.

`scripts/check-channels.sh` enforces the parts a machine can check: no workflow starts on a pushed tag, no workflow pushes the `stable` branch, the `stable` environment runs only from `workflow_call` or `workflow_dispatch`, the version examples sort as stated, every workflow that stamps a build also exports its full version to the build, and docs/remote.md carries the protocol window. CI runs it on every pull request, and `scripts/tests/check-channels-test.sh` proves it fails on bad workflows. `scripts/tests/stamp-test.sh` stamps each channel, builds the `local-studio-version` crate with the stamped environment and fails unless the built binary reports the full version and channel.

## The three channels

| Channel | For | Built from | When | Gate |
| --- | --- | --- | --- | --- |
| stable | Everyone. The default install. | `stable` branch, tag `vX.Y.Z` | When a nightly is promoted (rule below) | Promotion rule |
| nightly | Daily drivers who want new work first, Donovan included | `main` | Once a day at 07:00 UTC, from the newest `main` commit that passed the gate, skipped when that commit already shipped | CI green, plus the product's end-to-end checks from its AGENTS.md (Promotion rule below says what each product enforces) |
| dev | Developers and agents testing a change | `main` | Every merge to `main` | CI green |

A channel never ships half a build: CI uploads every platform into a draft release and swaps it in only when all of them succeeded (Silas's `nightly.yml` does this today; copy it). Publishing on a channel is serialized with a workflow `concurrency` group per channel (a run in progress always finishes, so a swap is never cut off halfway; GitHub keeps only the newest waiting run), and a build refuses to publish when the feed already holds a version equal to or newer than its own, or, on dev and nightly, a commit that its own commit does not contain (so re-running an old run can never move a channel back).

## Branches and tags

- `main`: all work lands here by pull request. It feeds dev and nightly.
- `stable`: moves only by `scripts/release-stable.sh`, which a repository admin runs to fast-forward it to a promoted commit after the build passed, or by a hotfix pull request merged with a merge commit. No workflow moves it: `GITHUB_TOKEN` cannot push a branch past commits that change workflow files and cannot be given that permission, so the push uses the admin's own credentials (`scripts/check-channels.sh` refuses a workflow that pushes `stable`). Protected by a ruleset: no deletion, no force pushes, and only repository admins may update it.
- `vX.Y.Z`: immutable tag on `stable` for each stable release. A tag is never reused or moved. A tag ruleset lets only repository admins create tags matching `v*`. `release-stable.sh` pushes the tag together with the `stable` move, only after the draft is built, so a failed build leaves no tag and no moved branch, and the promotion can run again.
- `nightly`: rolling tag and pre-release, moved to each new nightly. Each nightly also keeps an immutable pre-release tagged `nightly-YYYYMMDDHHMMSS` for 14 days, so any recent nightly can be reinstalled and promoted.
- `dev`: rolling tag and pre-release, moved to each dev build. No immutable dev tags.
- Feature branches: anything else. They never publish to a channel.
- No workflow triggers on a pushed `v*` tag. Stable builds start only from the promotion workflow, so no pre-release tag and no hand-made tag can start one.

Local Studio is a fork of gildrb/local-studio. Upstream changes come in by a pull request that merges `upstream/main` into `main`, like any other change.

## Version strings

One base version per product, `X.Y.Z`, in one file (Local Studio: the workspace `Cargo.toml`; Tauri apps: `tauri.conf.json`; browser and computer use: the extension's manifest). After stable `X.Y.Z` ships, `main` bumps the base to the next minor, `X.(Y+1).0`. Patch numbers on a released minor belong to hotfixes from `stable`, so `main` and a hotfix never claim the same version.

| Channel | Version | Example |
| --- | --- | --- |
| stable | `X.Y.Z` | `0.2.0`, hotfix `0.2.1` |
| nightly | `X.Y.Z-nightly.YYYYMMDDHHMMSS+<sha7>`, base = `main`'s unreleased minor | `0.3.0-nightly.20261118070000+1a2b3c4` |
| dev | `X.Y.Z-dev.YYYYMMDDHHMMSS+<sha7>` | `0.3.0-dev.20261117131042+9f8e7d6` |
| local build | `X.Y.Z-dev.0+local.<sha7>` | `0.3.0-dev.0+local.9f8e7d6` |

Timestamps are UTC build start. These are SemVer 2.0 strings, so for one base version dev sorts below nightly and nightly below stable (checked with the `semver` package: `0.3.0-dev.20261118090500 < 0.3.0-nightly.20261117070000 < 0.3.0`). A dev build made after a nightly still sorts below it. So:

- An updater compares versions **within its own channel** only. It offers an update when the feed's version on its channel is newer than the installed one.
- Switching channel is an explicit choice, never an "upgrade", and it may install a lower version. The in-app channel setting opens the other channel's download and the user installs it; no updater crosses channels, because each build trusts only its own channel's signing key (see Update feeds).
- Local builds never update themselves.

Every binary carries its full version and channel. `scripts/version.nu <channel> --stamp --build-env` prints `LOCAL_STUDIO_VERSION` and `LOCAL_STUDIO_CHANNEL`, which `build.yml` appends to `$GITHUB_ENV` before any cargo build. The `local-studio-version` crate (`version/`) reads them at compile time, refuses a version whose base is not the workspace version, and falls back to the local build version and channel `dev` when they are unset. The core re-exports them as `local_studio_core::VERSION` and `CHANNEL` (`build_version()` and `build_channel()` in LocalStudioKit); `local-studio --version` prints both on the core CLI and the Linux app, and both apps show them in Settings. The updater compares this string, never `CARGO_PKG_VERSION` or `CFBundleShortVersionString`.

Where a platform needs numbers only:

- Apple: `CFBundleShortVersionString` = `X.Y.Z`; `CFBundleVersion` = the build timestamp as `YYYYMMDD.HHMMSS` (always rising, which TestFlight also needs); `LocalStudioVersion` = the full SemVer string and `LocalStudioChannel` = the channel (`dev` for local builds).
- Browser extensions: `version` = `X.Y.Z.B` (the Windows `B` rule below), because Chrome allows only up to four integers; `version_name` = the full SemVer string.
- Windows file and product version: `X.Y.Z.B`, where `B` is 0 for stable and the CI run number modulo 65536 otherwise. Installers decide updates from the SemVer string, not this.
- Windows installers: nightly and dev ship NSIS only. An MSI rejects a pre-release identifier above 65535, so a stable MSI, where a product ships one, sets its WiX version explicitly (Tauri: `bundle.windows.wix.version`) from the `X.Y.Z.B` rule.

## Install identity

Each product picks one mode, recorded in the table below.

- **Side by side**: each channel is its own app with its own id, name and URL scheme, so stable and nightly can both be installed. This is T3 Code's model ("T3 Code" and "T3 Code (Nightly)").
- **Replace**: one app id for all channels; switching channel replaces the install. Use it for apps that hold macOS privacy grants (Accessibility, Microphone, Screen Recording), because a new id makes the user grant them again. Silas and Polycarp work this way today and keep one signing requirement across channels (Silas `docs/signing.md`).

Local Studio is side by side:

| | stable | nightly | dev |
| --- | --- | --- | --- |
| Name | Local Studio | Local Studio Nightly | Local Studio Dev |
| macOS bundle id | `org.local.studio.macos` | `org.local.studio.macos.nightly` | `org.local.studio.macos.dev` |
| Linux and Windows app id | `dev.localstudio.LocalStudio` | `dev.localstudio.LocalStudio.Nightly` | `dev.localstudio.LocalStudio.Dev` |
| iOS bundle id | `org.local.studio.ios` | `org.local.studio.ios.nightly` | `org.local.studio.ios.dev` |
| URL scheme | `localstudio` | `localstudio-nightly` | `localstudio-dev` |
| Data | shared with nightly | shared with stable | its own data dir |

The stable ids are the ones upstream already uses, so existing installs keep their data.

**Shared data.** Stable and nightly use the same Local Studio data dir and Eukhe sessions, so moving between them costs nothing while the data stays readable by both. Four rules keep it that way:

1. One owner at a time per data dir (`studio.lock`, see docs/remote.md). The second app to start attaches to the owner as a client, or says which app owns the data and offers to take over.
2. Between stable and nightly, data changes are additive: a build ignores fields it does not know and keeps them when it writes. Every write records the schema version.
3. A change an older build cannot read is a breaking migration. Nightly snapshots the data dir before running it. A build that finds a newer schema than it knows opens read-only, names the channel that wrote it, and offers to open the snapshot. The cost is stated in the release notes: stable then sees work only up to the snapshot until the migration reaches stable. At most one breaking migration and one client protocol bump land per stable release, so stable is never more than one protocol version behind nightly.
4. The newer Eukhe release wins the daemon. An older app offers the newer channel instead of restarting the daemon onto an older Eukhe.

Dev keeps its own data dir (`Local Studio Dev`) so tests never touch real threads, matching AGENTS.md.

## Update feeds

Each product publishes one JSON feed per channel as a release asset named `latest.json`:

| Channel | URL |
| --- | --- |
| stable | `https://github.com/baney75/<repo>/releases/latest/download/latest.json` |
| nightly | `https://github.com/baney75/<repo>/releases/download/nightly/latest.json` |
| dev | `https://github.com/baney75/<repo>/releases/download/dev/latest.json` |

The format is Tauri v2's updater manifest (Silas already serves it), with extra fields that Tauri ignores:

```json
{
  "product": "local-studio",
  "channel": "nightly",
  "version": "0.2.1-nightly.202611180700+1a2b3c4",
  "commit": "1a2b3c4d5e6f...",
  "pub_date": "2026-11-18T07:24:10Z",
  "notes": "Plain-words summary of what changed.",
  "protocol": 1,
  "platforms": {
    "darwin-aarch64": { "url": "...", "signature": "...", "sha256": "..." },
    "linux-x86_64": { "url": "...", "signature": "...", "sha256": "..." },
    "windows-x86_64": { "url": "...", "signature": "...", "sha256": "..." }
  }
}
```

- `signature` is a minisign signature of the artifact, as Tauri checks it. The Local Studio core checks the same feed and signature for all three desktop apps, so the native apps need no second updater format.
- `protocol` is the client protocol version the build speaks (docs/remote.md). Both sides speak their own protocol and the one before it: a server accepts both, and a client falls back to the older one when the server is older. So a nightly app or phone can attach to a stable owner and the other way round. Mixing builds two or more protocol versions apart is unsupported, and the one-bump-per-release rule keeps stable and nightly within one.
- Stable is signed with a key that only the `stable` GitHub Environment can read, and that environment only runs on the `stable` branch. Nightly and dev share a second key held by CI. Each build carries only its own channel's public key, so nothing built from `main` can install over a stable app through the updater.
- iPhone: dev builds install from Xcode; nightly goes to a TestFlight internal group; stable goes to TestFlight external testing and later the App Store, under the Apple team that signs Silas.
- Plugins and skill packs that ship as git trees (Gantrel, UntoCaesar, Corunis, Fonsviva) need no binary feed: their channel is a git ref. stable = the newest `vX.Y.Z` tag, nightly = the `nightly` tag, dev = `main`. A Local Studio install on channel C installs plugins from channel C by default. How the plugin catalog records this belongs to the plugin system lead.

## Installing and updating

**First install** is one command, `scripts/install.sh --channel nightly|stable|dev` (curl-able; README Install). It reads the channel's `latest.json`, downloads this machine's platform entry, checks its sha256 and its minisign signature against the channel key built into the script, and only then installs: macOS to `/Applications/<name>.app`, clearing quarantine on that verified app only, and Linux to `~/.local/lib/<slug>` with a `~/.local/bin` link and a desktop entry. Then it opens the app.

**In-app updates** live in the core (`core/src/update/`), so both apps share one implementation:

- With Automatic updates on, the build checks its own channel's feed 20 seconds after launch and every 6 hours. With it off, or with `LOCAL_STUDIO_NO_UPDATE_CHECK=1` in the environment, it checks only on Check Now in Settings → General → Updates. A local build, or a build that is not running from an install, shows why and never checks.
- An offer must name this build's channel, carry a newer version on that channel, list this platform with an https URL, a sha256 and a signature, and the signature's trusted comment must name the expected asset. Any other feed is refused with the reason.
- Automatic updates is on by default for nightly and stable and off for dev. While it is on, an offered build downloads at once into `<data dir>/updates/<channel>/`, checks sha256 and the minisign signature against the key compiled into it, and shows "Update ready" in the window chrome and in Settings. Nothing restarts on its own: Restart to update installs it.
- Install checks the unpacked build before touching the current one: its channel and version must match the offer (macOS `LocalStudioChannel` and `LocalStudioVersion`, Linux `local-studio --version`), and on macOS `codesign --verify --deep --strict` must pass against the running app's own designated requirement. It then swaps the new build in and keeps the old one beside it as `.<name>.previous`, and checks that the installed build reports the new version; if that fails it restores the previous build. Then it quits and relaunches.
- `local-studio update check|download|status|apply` runs the same steps from the core CLI, and `cargo run -p local-studio-core --example update_smoke <dir>` exercises them end to end against a local feed with a throwaway key (part of `scripts/e2e.sh`).

**macOS signing**: an ad-hoc signature is a new app to macOS each time, so no unsigned macOS build ships. Stable is signed with the Apple Developer ID identity and notarized, and `build.yml` skips the macOS build when those secrets are missing. Nightly and dev are signed with the self-issued `Local Studio Code Signing` identity (`MAC_SIGNING_P12`), hardened runtime on and entitlements kept, and are not notarized. Every build of a channel therefore keeps one designated requirement, which is what the updater checks the next build against. `scripts/check-channels.sh` fails a workflow that signs with `MAC_SIGNING_P12` without keeping stable on notarization.

## Promotion rule

**Dev** publishes on every merge to `main` with CI green.

**Nightly** publishes at 07:00 UTC. It builds the newest `main` commit for which all of these pass, and skips the day when that commit already is the nightly:

1. CI green. For Local Studio this is `ci.yml`'s run on the push to `main` concluding success: Linux (core, app, channels) and macOS (core and kit, app) block it; the Windows job records its failures without blocking, so Windows is not part of the gate yet.
2. The product's end-to-end checks from its AGENTS.md. For Local Studio this is a `local-studio/e2e` commit status of success on that commit (the core smoke examples on a local model, below); a commit without one is passed over.
3. No open issue labeled `regression` and `blocks-nightly`.

Nightly also stops, without publishing, when that commit does not contain the nightly feed's commit, or when its `build.yml` differs from `main`'s (the run would build it with `main`'s); the next commit that passes the gate fixes either.

The real-model checks need a GPU and a local model, which GitHub's runners never get, so they run on a Donovan machine. For Local Studio, `scripts/e2e.sh --post <commit>` runs the core smoke examples on a clean checkout of that commit against a local model and posts the `local-studio/e2e` commit status, labeled with that model; a systemd timer on abbey1 runs it on `main`'s head every day at 02:00 UTC against abbey1's Ollama (docs/e2e.md). Nightly reads that status and CI's own run for each of the last 30 `main` commits and takes the newest commit with both green. A commit nobody checked never ships as a nightly.

Dev and nightly swap the rolling release by first renaming the current one to `<channel>-previous`. If publishing the new one fails, the old one gets its tag back on its own commit. The old one is deleted only after the new one is live. A run that finds `<channel>-previous` and no rolling release restores from it, on the commit of the `<channel>-previous` tag. A GitHub error other than not found is never taken to mean a missing tag or release: it stops the swap, and the swap restores the previous release. The immutable `nightly-YYYYMMDDHHMMSS` copy stays a draft until the rolling swap succeeded, so a failed run deletes it and only a build that nightly users received can be promoted.

**Stable** promotes one nightly build when all of these hold:

1. That nightly was published at least 72 hours ago, and no later nightly found a regression in it.
2. Its commit has `stable`'s head as an ancestor, so it carries every hotfix merged back.
3. No open `regression` issue against any build since the last stable.
4. The product's TESTING.md checklist passes on that commit, and for anything with a screen, its steps were tried live on the Mac (the `live-checker` agent), not only in CI.
5. Release notes in plain words are written, in a pull request against `main` that passes the gates `baney-review` sets.

Then the product lead releases it in two steps:

1. `gh workflow run promote.yml --ref main -f nightly=nightly-YYYYMMDDHHMMSS`. It checks conditions 1 to 3 itself and stops if any fails, then builds every platform into a draft release for `vX.Y.Z` whose target is the nightly's commit. It moves no branch and creates no tag. Its build runs `main`'s `build.yml`, so it stops unless that file matches the promoted commit's; an existing `vX.Y.Z` tag is reused only when it already points at that commit, which lets a promotion be built again after a failed publish. Its last job records a manifest in the run (`promote-manifest`: the draft, version, commit, whether it is a nightly promotion or a hotfix, and each asset's sha256) and points the draft at the run. A failed build discards its draft, and running it again replaces any earlier draft for that version.
2. A repository admin runs `scripts/release-stable.sh X.Y.Z` from a checkout. It finds the one built draft and refuses unless the run it names is a finished, green `promote.yml` run on `main` whose manifest matches the draft and its current assets; for a nightly promotion it checks condition 3 again, on every retry too. It checks condition 2, fast-forwards `stable` to the draft's commit, pushes the `vX.Y.Z` tag there, checks both, and starts `stable.yml` on the `stable` ref. That run uses the promoted commit's own workflows, repeats the manifest checks, requires the tag on the draft's commit and that commit on `stable`, signs `latest.json` with the stable key only over assets whose sha256 matches the manifest, and publishes the draft. If it fails, nothing needs undoing: fix the cause and run `gh workflow run stable.yml --ref stable -f version=X.Y.Z` again, even after `stable` has moved on.

Then `main` bumps its base version. The `stable` GitHub Environment, which holds the stable signing key, allows only runs on the `stable` branch.

Set up once by hand, before the first stable release: the `stable` branch, starting at the commit that merged this scheme into `main`; the `stable` environment limited to the `stable` branch, holding `STABLE_UPDATER_SIGNING_KEY` and `STABLE_UPDATER_SIGNING_KEY_PASSWORD`; and the two rulesets above.

**Hotfix**: a pull request into `stable`, merged with a merge commit by a repository admin, with a check that fails before the fix and passes after. It skips the 72 hours and the open-regression gate, since fixing a regression is the usual reason for a hotfix and a pull request into `stable` never closes its issue: `gh workflow run promote.yml --ref main -f hotfix=true` builds `stable`'s head as `vX.Y.(Z+1)`, then `scripts/release-stable.sh X.Y.(Z+1)` tags it (`stable` is already there) and publishes. The hotfix must bump the workspace version to `X.Y.(Z+1)` and carry `main`'s `build.yml`; promote stops before building if it doesn't. `stable` then merges back into `main` with a merge commit (never a squash) the same day, so later nightlies carry it.

**Rollback**: re-publish the previous release's `latest.json` on that channel. That stops new installs from taking the bad build, but updaters never offer a lower version, so machines that already took it are repaired by rolling forward: a hotfix with a higher version that carries the good code. Never delete a release.

**Renaming a feed**: an installed build has its feed URL built in, so a channel or tag rename (Polycarp's `preview` to `nightly`) first ships one last build on the old feed that points at the new feed, and the old `latest.json` keeps being served until that bridge build has reached every install. Only then is the old tag retired. The first version on the new feed must sort above the bridge build's version; bump `main`'s base version at the rename, because `nightly` sorts below `preview`.

## Products

| Product | Mode | Today | Changes to adopt this scheme |
| --- | --- | --- | --- |
| Local Studio desktop (`Students-of-Local/baney-local-studio`) | side by side | Nightly and dev workflows, version stamping, signed in-app updates and `scripts/install.sh`; no release published yet | Publish the first nightly; add the `stable` branch at the first stable release |
| Local Studio iPhone (`ios/` in this repo) | side by side | In progress (PR #4) | Same version and tags as the desktop app, so the protocol always matches |
| Silas | replace | `vX.Y.Z` tags from the Mac, rolling `nightly` (`-nightly.YYYYMMDDHHMM`) | Add `stable` branch and `dev`; add `+<sha7>` to nightly versions |
| Polycarp | replace | Rolling `preview` pre-release | Move `preview` to `nightly` through a bridge build (Renaming a feed); add `stable` and `dev` |
| Browser and computer use (`browser/` in this repo) | side by side (extension ids per channel) | `v*` tag release workflow | Replace the `v*` tag trigger with the promotion workflow; add `stable` branch, nightly and dev; extension `version` per the numbers-only rule; Chrome Web Store gets stable only |
| Fonsviva | git ref | `main` and `stable` branches | Add `nightly` tag and `vX.Y.Z` tags |
| Gantrel | git ref | `main` and `stable` (RELEASES.md says no nightly) | Add the `nightly` tag; update RELEASES.md to this scheme |
| UntoCaesar | git ref | `main` and `stable` | Add `nightly` tag and `vX.Y.Z` tags |
| Corunis | git ref | `main` and `stable` | Add `nightly` tag and `vX.Y.Z` tags |
| Eukhe (upstream, pinned) | n/a | Pinned release tag | Every channel pins a released Eukhe tag; `EUKHE_BIN` overrides stay local |

Local Studio's stable channel opens with its first public release (target February 1, 2027). Until then nightly is the daily driver, as T3 Code nightly is today.
