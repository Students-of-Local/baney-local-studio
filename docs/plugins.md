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

## Local-only plugins

A plugin whose `plugin.toml` sets `local_only = true` runs only on local models. `plugin validate`, `plugin list` and the composer's command list show "Runs only on local models" for it.

Local Studio counts a provider as local only when its `models.json` entry has a `baseUrl` whose host is `localhost` or an IP address on loopback, a private range (10/8, 172.16/12, 192.168/16, IPv6 fc00::/7) or 100.64/10 (Tailscale). That covers the local fleet, Ollama, LM Studio and Fonsviva. Every other provider counts as cloud: built-in provider ids, the Claude Code and Codex harnesses, any entry without a `baseUrl`, and any host name other than `localhost`, even one that resolves to a private address.

When a message would expand one of the plugin's commands or prompts (`/name`) or skills (`/skill:name` or `$name`) in a thread on a cloud provider, Local Studio does not send it. The app says why and offers to switch the thread to a local model and run it; the message goes back into the composer. Once a local-only command runs in a thread, the thread stays local: every later message is checked the same way, a switch to a cloud model is refused, and a fork keeps the rule. A thread that ran on a cloud model before the command still holds that history.

In that thread Local Studio also:

- refuses to run while the session lists any MCP server from `settings.json` (connected or not), or any other connected MCP server, other than local-only plugins' servers, and names them. Eukhe exposes web fetch and search only through MCP servers, and it fixes `mcpServers` from `settings.json` when a session opens, so remove them there and start a new thread.
- sets the session's side models (tiny, judge, translate, auxiliary, subagents, workers, RLM queries, images, the backup model) to the thread's local model, clears their fallback chains, and turns off failover, chat memory and automatic titles. Regenerating a title uses the text generation model only when it is local; otherwise it asks the thread's own model.
- starts the plugin's MCP server for that session only, with `LOCAL_STUDIO_LOCAL_ONLY=1` in its environment. A local-only plugin's server is never written to `settings.json`, so other sessions do not get it.

This keeps the plugin's prompts and the thread off cloud models. It does not stop the model's tools from reaching the network: bash, the kernel and the plugin's own server can still open connections. Blocking that is upstream Eukhe work (#33). Another Eukhe client attached to the same session can still change its model.

## Downgrades

Builds from before grants were bound to code reject a `grants.json` that has `tree` and `source` fields. Running one of them on the same data dir fails every `plugin` command, and its Studio treats every plugin as ungranted and removes their MCP entries. Go back to a build with this rule to recover; the file itself is fine.
