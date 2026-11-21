# Plugin consent

What a capability grant covers, when it carries over to an update, and which `mcpServers` entries Local Studio owns. The code is `core/src/plugins/grants.rs`, `core/src/plugins/install.rs` and `sync_mcp_servers` in `core/src/plugins.rs`.

## Grants

`<data dir>/plugins/grants.json` holds one record per plugin id: the version, the granted capabilities, the installed tree hash (`tree`, the same sha256 as `local-studio plugin hash`) and the source the code came from (`catalog <id>`, `git <url>` or `folder <path>`). A plugin that asks for no capabilities is always ready. A plugin that asks for any is ready only when:

- the record's `tree` equals the hash of the plugin folder as it is now, and
- every capability it asks for is in the record.

Anything else is "needs consent": its skills, prompts and commands still work, and it gets no MCP server. Local Studio hashes the folder each time it checks consent, so a folder changed in place, or replaced by hand, needs consent again. That hash covers the whole folder, `.git` included (`plugin hash` skips a top-level `.git` for catalog pins), and skips only `.DS_Store` files, which Finder writes and no runtime loads. A plugin whose `[mcp]` command or args name a `.DS_Store` file does not load. A plugin that keeps state should keep it outside its own folder; Local Studio's entry for its server sets `PYTHONDONTWRITEBYTECODE`, so Python writes no `__pycache__` there.

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
- an untagged entry named `plugin-<name>` whose `command` or `cwd` lies inside the plugins folder (entries from before the tag, including one whose plugin moved its command or no longer loads).

Every other entry stays, whatever its name, including `plugin-manager` or an untagged `plugin-<name>` that runs code from outside the plugins folder. If a ready plugin's name is already taken by an entry Local Studio did not write, the user's entry stays, that plugin's server is off, and sync reports the name as an error (a notice in the app, an `error:` line in the CLI).

## Local-only plugins

A plugin whose `plugin.toml` sets `local_only = true` runs only on local models. `plugin validate`, `plugin list` and the composer's command list show "Runs only on local models" for it.

Local Studio counts a provider as local only when its `models.json` entry has a `baseUrl` whose host is `localhost` or an IP address on loopback, a private range (10/8, 172.16/12, 192.168/16, IPv6 fc00::/7) or 100.64/10 (Tailscale). That covers the local fleet, Ollama, LM Studio and Fonsviva. Every other provider counts as cloud: built-in provider ids, the Claude Code and Codex harnesses, any entry without a `baseUrl`, and any host name other than `localhost`, even one that resolves to a private address.

When a message invokes one of the plugin's commands (`/name`), skills (`/skill:name` or `$name`) in a thread on a cloud provider, Local Studio does not send it. The app says why and offers to switch the thread to a local model and run it; the message goes back into the composer. Once a local-only command runs in a thread, the thread stays local: every later message is checked the same way, a switch to a cloud model is refused, and a fork keeps the rule. A thread that ran on a cloud model before the command still holds that history.

In that thread Local Studio also:

- refuses to run while the session has any connected MCP server other than local-only plugins' servers, and names them. Eukhe exposes web fetch and search only through MCP servers, and it fixes `mcpServers` from `settings.json` when a session opens, so remove them there and start a new thread.
- sets the session's side models (tiny, judge, translate, auxiliary, subagents, workers, RLM queries, images, the backup model) to the thread's local model, clears their fallback chains, and turns off failover, chat memory and automatic titles. Regenerating a title uses the text generation model only when it is local; otherwise it asks the thread's own model.
- starts the plugin's MCP server for that session only, with `LOCAL_STUDIO_LOCAL_ONLY=1` in its environment. A local-only plugin's server is never written to `settings.json`, so other sessions do not get it.

This keeps the plugin's prompts and the thread off cloud models. It does not stop the model's tools from reaching the network: bash, the kernel and the plugin's own server can still open connections. Blocking that is upstream Eukhe work (#33). Another Eukhe client attached to the same session can still change its model.
