# Plugin consent

What a capability grant covers, when it carries over to an update, how the app finds, installs and updates plugins, and which `mcpServers` entries Local Studio owns. The code is `core/src/plugins/grants.rs`, `core/src/plugins/install.rs`, `core/src/plugins/hub.rs` and `sync_mcp_servers` in `core/src/plugins.rs`.

## Grants

`<data dir>/plugins/grants.json` holds one record per plugin id: the version, the granted capabilities, the installed tree hash (`tree`, the same sha256 as `local-studio plugin hash`) and the source the code came from (`catalog <id>`, `git <url>` or `folder <path>`). A plugin that asks for no capabilities is always ready. A plugin that asks for any is ready only when:

- the record's `tree` equals the hash of the plugin folder as it is now, and
- every capability it asks for is in the record.

Anything else is "needs consent": its skills, prompts and commands still work, and it gets no MCP server. Local Studio hashes the folder each time it checks consent (once per scan) for a plugin whose record holds a capability it asks for; with no such record there is nothing to compare, so it skips the hash. A folder changed in place, or replaced by hand, needs consent again. The change takes effect at the next sync, not at the moment of the edit: until then, sessions can still start the old entry. These sync: a Studio scan, `plugin list`, `plugin install`, `plugin grant` and `plugin revoke`. `plugin validate`, `plugin hash` and `plugin capabilities` do not. A plugin that keeps state should keep it outside its own folder; Local Studio's entry for its server sets `PYTHONDONTWRITEBYTECODE`, so Python writes no `__pycache__` there.

The consent hash covers the whole folder, `.git` included, and skips only `.DS_Store` files (in any letter case), which Finder writes and no runtime loads. Git rewrites its own state (`.git/index`, `FETCH_HEAD`, `logs/`, `objects/`, refs) during `git status`, `git fetch`, `git commit` and the rest, so any git command in a checkout plugin can end consent. A file that disappears while consent reads the folder (git's `index.lock`, say) counts as absent: consent ends if the folder changed, with no error.

To grant a plugin you are developing, keep the checkout outside the plugins folder and run `local-studio plugin install <dir>` after each change. Install copies the tree without `.git`, so git commands in the checkout never touch the grant, and a reinstall from the same folder is an update: it keeps every granted capability the new version still asks for.

(`plugin hash` and install skip a top-level `.git` entirely, in any letter case, for catalog pins.) A plugin whose `[mcp]` command or args name a `.DS_Store` file does not load, whether the name differs in letter case or is any other spelling the disk resolves to a `.DS_Store`. A plugin with capabilities is refused, and can't be granted, while its folder holds any of these: a symlink that leaves the folder, dangles, or points at a `.DS_Store` (in any letter case); or a file over 10 MB, which also refuses a checkout whose `.git` holds a pack that big. `plugin list` shows `refused` with the reason. Consent, install and `plugin hash` run these checks before they hash, so a file over 10 MB costs a stat, not a read. Sockets and FIFOs are left out of the hash and do not refuse consent: they hold no code, and a command that names one does not load. Each file is opened without waiting and without following a symlink, and only a regular file is read: a path that turned into a FIFO, a folder or a symlink after the walk saw a regular file refuses that check (consent, grant, install or hash) instead of blocking, and a file that vanished counts as absent.

A plugin folder in the plugins folder that is itself a symlink loads only when its manifest declares no capabilities and the folder it points at resolves inside your home folder (not the home folder itself), so a skills-only plugin you are developing can be linked in with `ln -s ~/code/notes <data dir>/plugins/notes`. Its id must match the name of the folder it points at. Its skills, prompts and commands are read through the link, so an edit there shows at the next scan, and `local-studio plugin hash` on the link hashes the folder it points at, so an edit there changes the hash. A linked plugin that declares any capability does not load, nor does a link that resolves outside your home folder or points nowhere, and consent refuses a linked folder: install the folder it points at with `local-studio plugin install <dir>` instead. `local-studio plugin validate` resolves a symlinked argument and checks the folder it points at, capabilities included, since it never touches the plugins folder or `mcpServers`.

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

Needed capabilities (`mcp-server`, `native-host`, `ask-user`) have no switch: the plugin cannot work without them, so declining means Cancel. Sensitive capabilities (`screen-capture`, `synthetic-input`, `site-secrets`, `accessibility`) each have a switch that starts off, unless the sheet was opened from Settings, Plugins for that one capability (`ConsentRequest.preset`). For a plugin that is not told (see [Partial grants](#partial-grants)), a sensitive capability left off counts as needing consent, so its MCP server stays off until that capability is granted in Settings, Plugins or with `local-studio plugin grant`. Studio does not offer the sheet again for a plugin that lacks only sensitive capabilities. The sheet says this plainly: when it lists a sensitive row, its footer says the plugin's tools stay off until every permission it asks for is on and points to Settings, Plugins, and Install reads "Install with tools off" while a switch is off. For a told plugin the footer says a switch left off is turned off for the plugin, and Install counts only what is on. The answer's `RequestDone` names the version and how many permissions it got, and for a plugin that is installed but off says to turn the rest on in Settings, Plugins.

Each row carries the capability id, `needed` or `sensitive`, a plain sentence of what it allows and one scope line, both fixed per capability in `Capability::sentence` and `Capability::scope` (`mcp-server`'s scope names the `[mcp]` command and its args). The sheet also carries the plugin's name, icon, description and version, the version it updates, the source (the Local Studio catalog with its commit, a git URL with its commit, or a local folder), the file count and the scan's findings: each executable the plugin ships besides its `[mcp]` command. Install stays off until the person opens the findings.

Three optional manifest fields feed the sheet. A build from before them refuses a manifest that uses them.

```toml
name = "Tychicus"
icon = "icon.png"

[gates]
screen-capture = "page screenshots for the agent"
synthetic-input = "clicking and typing in its own tabs"
```

`name` is the display name (the id when absent). `icon` is a path inside the plugin to the image the sheet shows. `gates` names, for each sensitive capability, the features it turns on; a key that is not a sensitive capability the plugin asks for is an error. Settings, Plugins shows it under a told plugin's switch: "Off means no page screenshots for the agent."

A sheet for a git URL that is not a catalog pin carries `unpinned`: its subtitle reads "at commit" instead of "pinned to", and it warns that the code is not pinned by the Local Studio catalog.

## Partial grants

A plugin opts into partial grants by putting `{granted}` in its `[mcp] args`:

```toml
[mcp]
command = "bin/server"
args = ["serve", "--granted={granted}"]
```

Such a plugin is told. On each sync Local Studio replaces `{granted}` with the sensitive capabilities the person granted, comma separated and sorted (`--granted=screen-capture`, or `--granted=` when none is on). A told plugin is ready once its needed capabilities are granted and its tree matches; each sensitive capability it asks for is on or off by itself, and turning one on or off rewrites the server entry at the next sync. The sheet shows `<allowed>` where the args hold `{granted}`.

This is a promise the plugin keeps, not one Local Studio enforces: the server runs as the same user and could use a capability it was told it lacks. The sheet and Settings say so ("The plugin is told; Local Studio cannot block one that ignores it"). A plugin without `{granted}` keeps the rule above: every capability it asks for, or its server stays off.

## Settings, Plugins

Settings, Plugins is one page in both apps with three groups. Every action goes through Studio, so the CLI, macOS, Linux and a phone see the same state.

- **Installed.** One row per plugin in the plugins folder, from `Update::Plugins`: name, version, state and a line of detail. The states are Running, Waiting for you (it needs consent; Allow opens the sheet), Off (its server name is held by a `settings.json` entry Local Studio did not write) and Failed (the manifest did not load or consent refused the folder, with the reason). Opening a row shows the description; each capability with a switch for a sensitive one (turning one on goes through the consent sheet with that row preset; turning one off revokes it at once); the commands and skills; the source, commit and channel; Open Folder; and Remove. Check for Updates looks up every git plugin's channel; a row with an update shows "Update to X" and, when the update asks for something new, names it.
- **Catalog.** The entries in `examples/plugins/catalog.toml`, each pinned to a full commit and a `sha256` tree hash that install checks before anything runs. Install is one click, then the consent sheet when the plugin asks for anything, then it runs. An installed entry shows its version.
- **Install from a Git URL or Folder.** A git URL with no commit installs the app's channel (below); `<url>@<sha>` installs that commit. Either way, Check for Updates then follows the channel and offers what it finds; nothing updates until you choose Update. Choose Folder copies a local folder. Both say the code is not pinned by the catalog.

`Studio` methods: `load_plugins`, `check_plugin_updates`, `install_plugin`, `update_plugin`, `remove_plugin`, `grant_plugin(id, capabilities)` (empty capabilities means all missing ones) and `revoke_plugin(id, capability)`. Remove deletes the folder (or the symlink, never its target) under the grants lock, forgets the plugin's grant and origin, and the next sync drops its server entry.

## Channels and updates

`<data dir>/plugins/origins.json` records where each installed plugin came from: the git URL, the commit, the subdirectory and excluded paths for a catalog entry, or the folder, and whether a catalog pinned it. A plugin with no record (copied in by hand) has no update.

A git plugin follows the app's channel, the one in Settings, General, Version (`local-studio-version`'s `CHANNEL`):

- **stable**: the newest `vX.Y.Z` tag.
- **nightly**: the `nightly` tag.
- **dev**: `main`.

A repo without the channel's ref falls back to `HEAD`, for installs and updates alike. Check for Updates resolves the channel commit and reads only the new `plugin.toml` (a blob-less fetch), then offers an update when the commit differs and the new version is greater, or the version is the same and the plugin folder's git tree changed. A new commit that leaves the plugin folder alone is no update. Update installs the new commit as an update from the same source, so the grant rules above apply: an update that asks for nothing new installs with no sheet, and one that asks for more shows a sheet that lists only the new capabilities. After an update the origin records the new commit.

A plugin installed from the catalog never follows a channel: what runs stays exactly what the catalog pinned. When a new Local Studio ships a catalog that pins it to another commit, Check for Updates offers that commit, and Update installs it through the catalog, so the tree hash is checked again before anything runs and the same grant rules apply.

## MCP server entries

Local Studio writes each ready plugin's server into `<agent dir>/settings.json` `mcpServers` as `plugin-<id>` with `"localStudioPlugin": true` and `"PYTHONDONTWRITEBYTECODE": {"env": "PATH"}` in its `env` (Eukhe takes env values only as host variable names, and Python needs any non-empty value). Eukhe ignores the extra field. On each sync Local Studio removes or rewrites only entries it wrote:

- an entry tagged `"localStudioPlugin": true`, or
- an untagged entry named `plugin-<name>` whose `command` is an absolute path inside the plugins folder, with no `..` (entries from before the tag, including one whose plugin moved its command or no longer loads). Its `cwd` does not count: Local Studio never writes a command outside the plugins folder.

Every other entry stays, whatever its name, including `plugin-manager`, an untagged `plugin-<name>` that runs code from outside the plugins folder even with its `cwd` inside it, and a tagged entry copied to a name without the `plugin-` prefix. If a ready plugin's name is already taken by an entry Local Studio did not write, the user's entry stays, that plugin's server is off, and sync reports the name as an error (a notice in the app, an `error:` line in the CLI).

## Questions from plugins

A ready plugin that asks for both `mcp-server` and `ask-user` can ask the person a question and get the person's answer, never the model's. The code is `core/src/plugins/ask.rs` (socket, tokens, server entries) and `core/src/studio/ask.rs` (cards, answers, Stop).

- **Delivery.** The `settings.json` entry above never carries the ask variables. When a thread starts or reopens, and before Local Studio sends a prompt to an idle session, it registers each asking plugin's server with that session as an ACP MCP server (`ReplaceAcpMcpServers`, owner `local-studio-ask`) under the same name `plugin-<id>`, with the same command, args and cwd and these variables: `LOCAL_STUDIO_ASK` (the socket path), `LOCAL_STUDIO_ASK_TOKEN` (32 random bytes as hex, one per plugin and thread), `LOCAL_STUDIO_THREAD` and `LOCAL_STUDIO_ASK_PID` (the process id of the Local Studio that owns the socket). Eukhe also gives the server the kernel's own `HOME`, `PATH` and `TMPDIR`, so a server that starts with `#!/usr/bin/env node` or `python3` finds the interpreter on the daemon's `PATH`. Eukhe looks up ACP servers before `settings.json`, so this entry wins in Local Studio threads. Other Eukhe frontends get the plain entry, so there the plugin has no `LOCAL_STUDIO_ASK` and must treat that as not confirmed. A plugin without the `ask-user` grant never gets these variables, and neither does the model's bash.
- **Taken away.** A session the daemon keeps across a Local Studio restart still holds the list an earlier Local Studio gave it. So until this Local Studio has sent a session its list, it always replaces the session's list, with an empty one when no plugin may ask any more (its grant was revoked or the plugin removed). Eukhe refuses a non-empty list while the session runs but takes an empty one, so a prompt sent during a run clears the list when it holds a server that may no longer ask, and the session gets its list again with its next idle prompt.
- **Not registered.** Eukhe takes ACP server env values only as literal strings and resolves host variable references only in `settings.json` entries. A plugin whose `[mcp] env` names host variables therefore keeps its `settings.json` entry, where Eukhe resolves them, and cannot ask; sync reports `<id> cannot ask you questions`. Local Studio also registers nothing under a `plugin-<id>` name that a `settings.json` entry it did not write holds.
- **Check the socket first (required).** The model's bash can unlink `ask.sock` and listen there itself. Before a plugin sends its token, it must check that the process at the other end is `LOCAL_STUDIO_ASK_PID`: `LOCAL_PEERPID` (`getsockopt` level 0, option 2) on macOS, `SO_PEERCRED` on Linux. On a mismatch it closes the connection without sending anything and treats the question as not confirmed, or retries. A plugin that skips this hands its token to the impostor and takes the impostor's answer. A client:

  ```python
  import json, os, socket, struct, sys

  def ask(text, choices):
      path, token, pid = (os.environ.get(name) for name in ("LOCAL_STUDIO_ASK", "LOCAL_STUDIO_ASK_TOKEN", "LOCAL_STUDIO_ASK_PID"))
      if not (path and token and pid):
          return {"status": "refused", "error": "not running under Local Studio"}
      conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
      try:
          conn.connect(path)
          if sys.platform == "darwin":
              peer = struct.unpack("i", conn.getsockopt(0, 2, 4))[0]
          else:
              peer = struct.unpack("3i", conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))[0]
          if peer != int(pid):
              return {"status": "refused", "error": "ask.sock is not served by Local Studio"}
          request = {"token": token, "thread": os.environ.get("LOCAL_STUDIO_THREAD"), "text": text, "choices": choices}
          conn.sendall((json.dumps(request) + "\n").encode())
          reply = b""
          while not reply.endswith(b"\n"):
              chunk = conn.recv(4096)
              if not chunk:
                  return {"status": "cancelled"}
              reply += chunk
          return json.loads(reply)
      except OSError as error:
          return {"status": "refused", "error": str(error)}
      finally:
          conn.close()
  ```

- **Socket.** `<data dir>/ask.sock`, or a short path in the temp folder when that one is too long for a Unix socket. Mode 0600; connections from another user are dropped. Send one JSON line within 5 s, under 16 KiB: `{"token": "...", "thread": "...", "text": "...", "choices": ["Yes", "No"]}`. `thread` is optional and must match the token's thread. `text` is 1 to 2000 characters, `choices` at most 8 unique labels of 1 to 120 characters, and any other field is refused. A question on an archived thread is refused. Keep the connection open, both ways, until the reply: closing it, or shutting down only the sending side, withdraws the question, and a half-closed connection gets `{"status": "refused", "error": "the plugin closed its sending side, ..."}`. The reply is one JSON line: `{"status": "answered", "answer": "..."}`, `{"status": "cancelled"}` or `{"status": "refused", "error": "..."}`.
- **Replaced socket.** Every second, and on each new connection, Local Studio checks that `ask.sock` is still its own listener (device, inode and change time). When it is not, Local Studio makes it again and posts a warning in the app and in each thread whose session has asking plugins: "Another process replaced the plugin question socket ..., so Local Studio made it again. A plugin answer from the replaced socket did not come from you." It warns once per replacement, not once per second while something keeps taking the path.
- **Tokens.** Local Studio keeps the tokens in memory only and never writes them to disk, where the model's bash could read them. At start it removes the `<data dir>/ask-tokens.json` an older build left. A token lasts as long as the plugin holds its `ask-user` grant, the thread exists and that Local Studio runs. A new Local Studio issues new tokens and has a new pid: a plugin server that is still running keeps the old token and `LOCAL_STUDIO_ASK_PID`, so it refuses the new socket, or the socket refuses its token, and its questions come back not confirmed until the session's next prompt registers it again and Eukhe restarts it with the new values. A session that was busy during the restart keeps the old values until its next idle prompt.
- **Who answers.** The socket has no answer message. Only `Studio::answer_questions`, which the desktop apps call when the person clicks a choice, presses Return on one or sends typed text in the card, and which a phone reaches through the Remote op `answer_questions`, settles a question. The answer is a choice label or the typed text.
- **Card.** The thread shows a question card labeled with the plugin's id and a plugin icon (manifests carry no icon of their own), counts as needing the person, and raises a `needs_answer` notification. A phone that starts watching the thread gets the open questions again. An open question blocks only the plugin's tool call.
- **Stop and close.** Stop, archiving the thread or deleting it answers every open question in it `cancelled`. A plugin must treat `cancelled` as not confirmed.
- **Transcript.** Once the run is idle, Local Studio appends a displayed note to the session (custom type `local-studio-ask`): `<plugin> asked: <text>. You answered: <answer>` (no extra period when the question ends in one), or, after Stop or archive, `<plugin> asked: <text>. You stopped the question (not confirmed).` The model sees it in its next request.
- **What this guarantees.** The protocol gives the model no way to answer: the socket takes only questions, and only the person's click or typed text, in the app or on a paired phone, settles one. A server the model puts in place of `ask.sock` gets no token and gives no answer to a plugin that checks the peer pid, and Local Studio takes the path back and says so. Until per-session sandboxing ([#33](https://github.com/baney75/local-studio/issues/33)) lands, the model's bash runs as the same user as Local Studio and the plugin. A model that attacks the system itself is outside this guarantee: one that drives the desktop app, pairs a device, kills or traces processes, or reads a plugin's environment (`ps eww`, the kernel's view of the session's MCP config) to take its token and raise questions as that plugin.
- **Timeouts.** Eukhe gives ACP stdio servers no `callTimeoutMs`, so the model's MCP call gives up after the kernel's default 60 s; the card stays until the person answers or the thread stops, and the plugin gets that answer on its open connection.

## Caller identity

A plugin's MCP server learns who started the thread it serves. The core sets it, never the model. The code is `Caller` in `core/src/plugins.rs`, `acp_servers` in `core/src/plugins/ask.rs` and `sync_plugin_servers` in `core/src/studio/ask.rs`.

- **Variables.** `LOCAL_STUDIO_CALLER` holds a stable member id and `LOCAL_STUDIO_CALLER_NAME` a display name:
  - `local:<uid>` for a thread started on the host (the desktop apps or any other `Studio::start_thread`), named by `USER` (else `LOGNAME`, `USERNAME`, then the uid).
  - `device:<id>` for a thread a paired phone or a linked desktop started through Remote (`start_thread` in docs/remote.md): the device's id in `<data dir>/remote/devices.json`, which serve binds to that device's key at pairing, named by that device's name when the thread started. The prefix keeps a device from pairing under an id that reads as a host id.
- **Fixed at start.** The core sets the caller when the thread starts and stores it with the thread (`caller` in `state.json`). A fork keeps it. Later messages change nothing: a paired device that sends into a host thread runs as `local:<uid>`, and the host sending into a device's thread runs as that device. A thread from before this rule, or one Local Studio did not start, has no caller.
- **Delivery.** For a thread with a caller, the registration under Questions from plugins covers every ready plugin with an MCP server, not only asking ones: each gets its own ACP MCP server entry `plugin-<id>` for that session with both variables (asking plugins keep the ask variables too). A local-only plugin's session entry carries them as well. Each session starts its own server process, so one server sees one caller for its whole life. The variables come only from the thread's stored caller; a tool's arguments (a `caller` field, say) are the model's words, never identity. A manifest whose `[mcp] env` names either variable does not load.
- **No caller.** The `settings.json` entry carries neither variable, so a server started by another Eukhe frontend, a thread without a stored caller, and a plugin whose `[mcp] env` names host variables (it keeps its `settings.json` entry, as under Questions from plugins) see no `LOCAL_STUDIO_CALLER`. Treat that as an unknown caller and refuse anything scoped to a member.
- **Not per call.** Eukhe takes an ACP server's env only when it starts the server, and Local Studio does not write the server's stdio, so the core cannot add a `_meta.caller` to each `tools/call` or strip one the model supplies. Per-call identity needs an Eukhe change. The per-session variables stand in for it, and they are exact because a session serves one thread.
- **What this does not guarantee.** As with questions, until per-session sandboxing ([#33](https://github.com/baney75/local-studio/issues/33)) lands the model's bash runs as the same user as Local Studio and the plugin: it can read the plugin's own data, and it can reach the daemon socket and register a server with other variables. A plugin that serves members by role also needs member sessions limited to its own tools ([#3](https://github.com/baney75/local-studio/issues/3)).
- **Check.** `caller_smoke` (AGENTS.md) runs a host thread and a thread started from a paired scratch desktop through the real `local-studio-serve`, each asking the real local model to call a logging tool whose arguments claim the other caller, and requires two different true ids from two server processes.

## Local-only plugins

A plugin whose `plugin.toml` sets `local_only = true` runs only on local models. `plugin validate`, `plugin list` and the composer's command list show "Runs only on local models" for it.

Local Studio counts a provider as local only when its `models.json` entry has a `baseUrl` whose host is `localhost` or an IP address on loopback, a private range (10/8, 172.16/12, 192.168/16, IPv6 fc00::/7) or 100.64/10 (Tailscale). That covers the local fleet, Ollama, LM Studio and Fonsviva. Every other provider counts as cloud: built-in provider ids, the Claude Code and Codex harnesses, any entry without a `baseUrl`, and any host name other than `localhost`, even one that resolves to a private address.

When a message would expand one of the plugin's commands or prompts (`/name`) or skills (`/skill:name` or `$name`) in a thread on a cloud provider, Local Studio does not send it. The app says why and offers to switch the thread to a local model and run it; the message goes back into the composer. Once a local-only command runs in a thread, the thread stays local: every later message is checked the same way, a switch to a cloud model is refused, and a fork keeps the rule. A thread that ran on a cloud model before the command still holds that history.

In that thread Local Studio also:

- refuses to run while the session lists any MCP server from `settings.json` (connected or not), or any other connected MCP server, other than local-only plugins' servers, and names them. Eukhe exposes web fetch and search only through MCP servers, and it fixes `mcpServers` from `settings.json` when a session opens, so remove them there and start a new thread.
- sets the session's side models (tiny, judge, translate, auxiliary, subagents, workers, RLM queries, images, the backup model) to the thread's local model, clears their fallback chains, and turns off failover, chat memory and automatic titles. Regenerating a title uses the text generation model only when it is local; otherwise it asks the thread's own model.
- starts the plugin's MCP server for that session only, with `LOCAL_STUDIO_LOCAL_ONLY=1` and the thread's caller (see Caller identity) in its environment. A local-only plugin's server is never written to `settings.json`, so other sessions do not get it.

This keeps the plugin's prompts and the thread off cloud models. It does not stop the model's tools from reaching the network: bash, the kernel and the plugin's own server can still open connections. Blocking that is upstream Eukhe work (#33). Another Eukhe client attached to the same session can still change its model.

## Downgrades

Builds from before grants were bound to code reject a `grants.json` that has `tree` and `source` fields. Running one of them on the same data dir fails every `plugin` command, and its Studio treats every plugin as ungranted and removes their MCP entries. Go back to a build with this rule to recover; the file itself is fine.
