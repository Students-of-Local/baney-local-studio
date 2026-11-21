# Plugin consent

What a capability grant covers, when it carries over to an update, and which `mcpServers` entries Local Studio owns. The code is `core/src/plugins/grants.rs`, `core/src/plugins/install.rs` and `sync_mcp_servers` in `core/src/plugins.rs`.

## Grants

`<data dir>/plugins/grants.json` holds one record per plugin id: the version, the granted capabilities, the installed tree hash (`tree`, the same sha256 as `local-studio plugin hash`) and the source the code came from (`catalog <id>`, `git <url>` or `folder <path>`). A plugin that asks for no capabilities is always ready. A plugin that asks for any is ready only when:

- the record's `tree` equals the hash of the plugin folder as it is now, and
- every capability it asks for is in the record.

Anything else is "needs consent": its skills, prompts and commands still work, and it gets no MCP server. Local Studio hashes the folder each time it checks consent (once per scan) for a plugin whose record holds a capability it asks for; with no such record there is nothing to compare, so it skips the hash. A folder changed in place, or replaced by hand, needs consent again. The change takes effect at the next sync, not at the moment of the edit: until then, sessions can still start the old entry. These sync: a Studio scan, `plugin list`, `plugin install`, `plugin grant` and `plugin revoke`. `plugin validate`, `plugin hash` and `plugin capabilities` do not. A plugin that keeps state should keep it outside its own folder; Local Studio's entry for its server sets `PYTHONDONTWRITEBYTECODE`, so Python writes no `__pycache__` there.

The consent hash covers the whole folder, `.git` included, and skips only `.DS_Store` files (in any letter case), which Finder writes and no runtime loads. Git rewrites its own state (`.git/index`, `FETCH_HEAD`, `logs/`, `objects/`, refs) during `git status`, `git fetch`, `git commit` and the rest, so any git command in a checkout plugin can end consent. A file that disappears while consent reads the folder (git's `index.lock`, say) counts as absent: consent ends if the folder changed, with no error.

To grant a plugin you are developing, keep the checkout outside the plugins folder and run `local-studio plugin install <dir>` after each change. Install copies the tree without `.git`, so git commands in the checkout never touch the grant, and a reinstall from the same folder is an update: it keeps every granted capability the new version still asks for.

(`plugin hash` and install skip a top-level `.git` entirely, in any letter case, for catalog pins.) A plugin whose `[mcp]` command or args name a `.DS_Store` file does not load, whether the name differs in letter case or is any other spelling the disk resolves to a `.DS_Store`. A plugin with capabilities is refused, and can't be granted, while its folder holds any of these: a symlink that leaves the folder, dangles, or points at a `.DS_Store` (in any letter case); or a file over 10 MB, which also refuses a checkout whose `.git` holds a pack that big. `plugin list` shows `refused` with the reason. Consent, install and `plugin hash` run these checks before they hash, so a file over 10 MB costs a stat, not a read. Sockets and FIFOs are left out of the hash and do not refuse consent: they hold no code, and a command that names one does not load. Each file is opened without waiting and without following a symlink, and only a regular file is read: a path that turned into a FIFO, a folder or a symlink after the walk saw a regular file refuses that check (consent, grant, install or hash) instead of blocking, and a file that vanished counts as absent.

A plugin folder that is itself a symlink does not load, and consent refuses it: install the folder it points at with `local-studio plugin install <dir>` instead.

Grants from before this rule have no `tree`, so each plugin with capabilities asks once more after the upgrade. Granting it again records the tree and leaves the source empty (unknown origin). The next catalog install of that id counts as an update of it; an install from git or a folder drops it.

## Installs and updates

Every `local-studio plugin install` records the new tree and source:

- **Same tree** (a reinstall of the same bytes, from any source): the grant stays as it was, source included.
- **Same source, new tree** (an update): the grant keeps only the capabilities that both the granted version and the new version ask for. An update that asks for nothing new stays ready without a prompt, as slice 2 promised. A capability the new version asks for that the grant does not hold needs consent, including one that an earlier version had and a later one dropped: 1.0.0 granted `mcp-server` and `screen-capture`, 2.0.0 dropped `screen-capture`, so 3.0.0 asks for `screen-capture` again.
- **Another source, new tree**: the grant is dropped and every capability needs consent. Installing `gantrel` from a folder or another git URL never inherits the grant given to the catalog's `gantrel`. Install prints a line naming both sources when it drops a grant.

The source is the catalog id, the git URL without its commit, or the folder's absolute path. `plugin grant` on a folder whose tree has no install record (one copied in by hand) records it as `folder <plugin dir>`, unless the record is a grant from before this rule (see above).

`plugin revoke <id>` with no capabilities clears every capability and keeps the tree and source, so a later update from the same source still counts as an update.

## Consent sheet

The app asks for consent with a sheet, built in core so macOS, Linux and a phone attached over the client protocol show the same one. The code is `core/src/plugins/consent.rs`.

- `Studio::install_plugin(request, source)` takes what `local-studio plugin install` takes. It fetches, checks, hashes and copies the plugin into a scratch folder inside the plugins folder, exactly as install does, and installs nothing yet. When the plugin asks for capabilities the person has not answered, Studio sends `Update::Consents` with one `ConsentRequest` per open sheet; otherwise it installs at once and answers `RequestDone`.
- `Studio::answer_consent(request, answer)` closes the sheet. `Cancel` (Esc or Cancel) installs nothing, grants nothing and deletes the scratch copy. `Install { sensitive }` first rescans and rehashes the scratch copy and refuses, installing and granting nothing, when its files, findings or catalog pin no longer match what the sheet showed or the rows it would ask for changed. It then installs the scratch copy through the same step as `plugin install` (same tree and source record, same update rules) and grants the needed capabilities the sheet listed plus the sensitive ones in `sensitive`, through the same code as `plugin grant`. A sensitive id the sheet did not list is refused and installs nothing.
- On the wire it is the `consents` update and the `answer_consent` op (docs/remote.md).

Which rows a sheet lists:

- A new install, a reinstall that carries no grant, or an install from another source: every capability the plugin asks for.
- An update whose grant carries over (same tree or same source, per the rules above): only the capabilities the new version asks for that the installed version did not. An update that asks for nothing new, including one that drops a capability, installs without a sheet.
- A plugin that is already installed but lacks a needed capability (copied in by hand, changed in place, or installed by `plugin install` and never granted): Studio offers a sheet with its missing capabilities when it scans the plugins folder, at most once per plugin version while Studio runs. Its Install grants; Cancel leaves the plugin as it was. Studio hashes the folder when it offers the sheet and again before it grants, and refuses the grant if the folder changed in between.

Needed capabilities (`mcp-server`, `native-host`, `ask-user`) have no switch: the plugin cannot work without them, so declining means Cancel. Sensitive capabilities (`screen-capture`, `synthetic-input`, `site-secrets`, `accessibility`) each have a switch that starts off. The grant rules above are unchanged: a plugin whose sensitive capability stays off counts as needing consent for it, so its MCP server stays off until that capability is granted with `local-studio plugin grant`. Studio does not offer the sheet again for a plugin that lacks only sensitive capabilities. The sheet says this plainly: when it lists a sensitive row, its footer says the plugin's tools stay off until every permission it asks for is on and names `local-studio plugin grant <id> <capability>`, and Install reads "Install with tools off" while a switch is off. The answer's `RequestDone` says the plugin is installed but off, with the grant command for what is missing.

Each row carries the capability id, `needed` or `sensitive`, a plain sentence of what it allows and one scope line, both fixed per capability in `Capability::sentence` and `Capability::scope` (`mcp-server`'s scope names the `[mcp]` command and its args). The sheet also carries the plugin's name, icon, description and version, the version it updates, the source (the Local Studio catalog with its commit, a git URL with its commit, or a local folder), the file count and the scan's findings: each executable the plugin ships besides its `[mcp]` command. Install stays off until the person opens the findings.

Three optional manifest fields feed the sheet. A build from before them refuses a manifest that uses them.

```toml
name = "Tychicus"
icon = "icon.png"

[gates]
screen-capture = "page screenshots for the agent"
synthetic-input = "clicking and typing in its own tabs"
```

`name` is the display name (the id when absent). `icon` is a path inside the plugin to the image the sheet shows. `gates` names, for each sensitive capability, the features it turns on; a key that is not a sensitive capability the plugin asks for is an error. The sheet does not show it yet, because under the current grant rule a switch left off keeps the whole plugin off; it is kept for a later change to that rule.

## MCP server entries

Local Studio writes each ready plugin's server into `<agent dir>/settings.json` `mcpServers` as `plugin-<id>` with `"localStudioPlugin": true` and `"PYTHONDONTWRITEBYTECODE": {"env": "PATH"}` in its `env` (Eukhe takes env values only as host variable names, and Python needs any non-empty value). Eukhe ignores the extra field. On each sync Local Studio removes or rewrites only entries it wrote:

- an entry tagged `"localStudioPlugin": true`, or
- an untagged entry named `plugin-<name>` whose `command` is an absolute path inside the plugins folder, with no `..` (entries from before the tag, including one whose plugin moved its command or no longer loads). Its `cwd` does not count: Local Studio never writes a command outside the plugins folder.

Every other entry stays, whatever its name, including `plugin-manager`, an untagged `plugin-<name>` that runs code from outside the plugins folder even with its `cwd` inside it, and a tagged entry copied to a name without the `plugin-` prefix. If a ready plugin's name is already taken by an entry Local Studio did not write, the user's entry stays, that plugin's server is off, and sync reports the name as an error (a notice in the app, an `error:` line in the CLI).

## Downgrades

Builds from before grants were bound to code reject a `grants.json` that has `tree` and `source` fields. Running one of them on the same data dir fails every `plugin` command, and its Studio treats every plugin as ungranted and removes their MCP entries. Go back to a build with this rule to recover; the file itself is fine.
