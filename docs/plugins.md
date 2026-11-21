# Plugin consent

What a capability grant covers, when it carries over to an update, and which `mcpServers` entries Local Studio owns. The code is `core/src/plugins/grants.rs`, `core/src/plugins/install.rs` and `sync_mcp_servers` in `core/src/plugins.rs`.

## Grants

`<data dir>/plugins/grants.json` holds one record per plugin id: the version, the granted capabilities, the installed tree hash (`tree`, the same sha256 as `local-studio plugin hash`) and the source the code came from (`catalog <id>`, `git <url>` or `folder <path>`). A plugin that asks for no capabilities is always ready. A plugin that asks for any is ready only when:

- the record's `tree` equals the hash of the plugin folder as it is now, and
- every capability it asks for is in the record.

Anything else is "needs consent": its skills, prompts and commands still work, and it gets no MCP server. Local Studio hashes the folder each time it checks consent (once per scan), so a folder changed in place, or replaced by hand, needs consent again. The change takes effect at the next sync (the next Studio scan or `plugin` command), not at the moment of the edit: until then, sessions can still start the old entry. A plugin that keeps state should keep it outside its own folder; Local Studio's entry for its server sets `PYTHONDONTWRITEBYTECODE`, so Python writes no `__pycache__` there.

The consent hash covers the whole folder, a top-level `.git` included, except:

- `.DS_Store` files, which Finder writes and no runtime loads, and
- the state git rewrites on its own: `.git/index`, `.git/FETCH_HEAD`, `.git/logs/`, `.git/objects/` and `.git/refs/remotes/`, so `git status` and `git fetch` in a checkout keep consent. A commit, checkout or pull changes the tree and needs consent again.

(`plugin hash` skips a top-level `.git` entirely, for catalog pins.) A plugin whose `[mcp]` command or args name a `.DS_Store` file or anything inside `.git` does not load. A plugin with capabilities is refused, and can't be granted, while its folder holds any of these: a symlink that leaves the folder, dangles, or points at something the hash skips (a `.DS_Store`, or the git state above); a file over 10 MB; or anything that is not a file, folder or symlink. `plugin list` shows `refused` with the reason. Each check reads no file over 10 MB, so a large file costs a stat, not a read.

Grants from before this rule have no `tree`, so each plugin with capabilities asks once more after the upgrade. Granting it again records the tree and leaves the source empty (unknown origin). The next catalog install of that id counts as an update of it; an install from git or a folder drops it.

## Installs and updates

Every `local-studio plugin install` records the new tree and source:

- **Same tree** (a reinstall of the same bytes, from any source): the grant stays as it was, source included.
- **Same source, new tree** (an update): the grant keeps only the capabilities that both the granted version and the new version ask for. An update that asks for nothing new stays ready without a prompt, as slice 2 promised. A capability the new version asks for that the grant does not hold needs consent, including one that an earlier version had and a later one dropped: 1.0.0 granted `mcp-server` and `screen-capture`, 2.0.0 dropped `screen-capture`, so 3.0.0 asks for `screen-capture` again.
- **Another source, new tree**: the grant is dropped and every capability needs consent. Installing `gantrel` from a folder or another git URL never inherits the grant given to the catalog's `gantrel`. Install prints a line naming both sources when it drops a grant.

The source is the catalog id, the git URL without its commit, or the folder's absolute path. `plugin grant` on a folder whose tree has no install record (one copied in by hand) records it as `folder <plugin dir>`, unless the record is a grant from before this rule (see above).

`plugin revoke <id>` with no capabilities clears every capability and keeps the tree and source, so a later update from the same source still counts as an update.

## MCP server entries

Local Studio writes each ready plugin's server into `<agent dir>/settings.json` `mcpServers` as `plugin-<id>` with `"localStudioPlugin": true` and `"PYTHONDONTWRITEBYTECODE": {"env": "PATH"}` in its `env` (Eukhe takes env values only as host variable names, and Python needs any non-empty value). Eukhe ignores the extra field. On each sync Local Studio removes or rewrites only entries it wrote:

- an entry tagged `"localStudioPlugin": true`, or
- an untagged entry named `plugin-<name>` whose `command` is an absolute path inside the plugins folder, with no `..` (entries from before the tag, including one whose plugin moved its command or no longer loads). Its `cwd` does not count: Local Studio never writes a command outside the plugins folder.

Every other entry stays, whatever its name, including `plugin-manager`, an untagged `plugin-<name>` that runs code from outside the plugins folder even with its `cwd` inside it, and a tagged entry copied to a name without the `plugin-` prefix. If a ready plugin's name is already taken by an entry Local Studio did not write, the user's entry stays, that plugin's server is off, and sync reports the name as an error (a notice in the app, an `error:` line in the CLI).

## Downgrades

Builds from before grants were bound to code reject a `grants.json` that has `tree` and `source` fields. Running one of them on the same data dir fails every `plugin` command, and its Studio treats every plugin as ungranted and removes their MCP entries. Go back to a build with this rule to recover; the file itself is fine.
