# T3 Code import and sync

Someone moving from T3 Code should find their setup in Local Studio on first launch, and it should keep following T3 Code until they turn that off.

## What comes over

| T3 Code | Local Studio |
| --- | --- |
| Claude Code instances (`claudeAgent`, `claudeAgent_*`) | Claude Code accounts in `harnessAccounts.claude`, named by `displayName`, with the instance accent. An empty `homePath` means `~/.claude`. |
| The Codex instance | The Codex account in `harnessAccounts.codex` (`homePath`, else `~/.codex`). |
| Cursor, Grok, OpenCode, OMP, Antigravity | Reported as "Not supported yet": Local Studio has no harness for them. A secret field (`apiKey`, `serverPassword`, `token`) is reported only as "set in T3 Code". |
| Projects in `statev2.sqlite` (`projection_projects`) | Projects, by title, when the folder exists and is not archived or deleted. A project's default model becomes its project setting. |
| Claude Code and Codex threads (`orchestration_v2_projection_*` in `statev2.sqlite`) | Threads with their title, project, folder, account, model, thinking level, access and shelf (settled, archived, snoozed). Subagent threads, deleted threads and threads that never ran are left out. |
| `defaultModelSelection`, `textGenerationModelSelection`, `sourceControlModelSelection` | Default, text generation and source control model, with the thinking level. |
| `favorites` and `providerModelPreferences.*.hiddenModels` | Favorites and hidden models, where the model exists in Local Studio. Claude ids must be in Local Studio's Claude list; Codex ids pass through. |
| `keybindings.json` | Shortcut overrides for commands Local Studio has (`mod` becomes `cmd`). A binding equal to Local Studio's default is not written. |
| Theme (`defaultTheme`, `themes/*.json`), appearance and client settings | Theme, color scheme, fonts, sizes, chat width, contrast, diff colors and the preferences the two apps share. |

## Sign-ins are shared, never copied

An account is a config dir. Local Studio points at the same dirs T3 Code uses, so both apps share one login. It never copies a sign-in: a copy that refreshes would rotate the OAuth token and sign the real account out. The import never opens `secrets/`, `providers/`, `cloud-auth-token.json`, `.credentials.json`, `.claude.json` or `auth.json`; it checks only that a sign-in file exists, with `symlink_metadata`. Secret fields in `settings.json` are parsed only as present or absent.

## Threads continue in a copy

A thread that came over shows its session as T3 Code left it. The first message you send forks that session (`claude --resume <T3 session> --fork-session`, Codex `thread/fork`), so Local Studio continues in its own copy and never writes T3 Code's. T3 Code keeps working on the original; what happens there after the fork doesn't show here.

- A thread whose folder is gone (a removed worktree) can be read but not continued.
- A thread running in T3 Code is listed as "Running in T3 Code" and comes over when it stops.
- A thread whose session file is not on this machine (Claude Code removes old ones) is listed and left out.
- Each thread comes over once. One you delete in Local Studio is not brought back.
- The account is the one whose folder holds the session, starting with the one T3 Code used. The folder is the thread's worktree, else its project, else the folder its session ran in.

## How sync works

- Local Studio looks in `$T3CODE_HOME` (else `~/.t3`), in `userdata` and then `dev`.
- On launch it offers the first-run sheet "Bring over your T3 Code setup" when T3 Code has something new. Each group can be turned off. Not Now keeps sync off; Settings → General → T3 Code turns it on later.
- With sync on, the core checks the T3 Code files every 3 seconds (size and modified time of `settings.json`, `client-settings.json`, `keybindings.json`, `statev2.sqlite` and `themes/*.json`) and applies changes. Nothing is ever written to T3 Code's folder.
- Sync is additive and remembers what it brought over, per key, in `<data dir>/t3-sync.json`. A value is updated only while it still equals what the last sync wrote (or the Local Studio default, the first time). A value you change in Local Studio is kept and shows as "changed here, kept". A favorite, project or account you remove is not brought back.
- `statev2.sqlite` is opened read-only with `immutable=1`, so it never takes a lock T3 Code needs. A project or thread changed in the last few seconds may show its change only after T3 Code checkpoints its write-ahead log.

## CLI

`local-studio import t3 [--dry-run] [--json]` runs one sync against the data dir (`LOCAL_STUDIO_DATA_DIR`, else the default). `--dry-run` writes nothing. `--json` prints the report. A real run refuses while Local Studio holds the data dir; use Import Now in Settings then.

## Check

`core/examples/t3_import_smoke.rs` (run by `scripts/e2e.sh`) builds T3 Code data in a scratch `HOME` with two Claude Code instances, Codex, Cursor and OpenCode, a projects and threads database and keybindings. Every sign-in and secret file is a FIFO, so opening one hangs the run past its time limit. It imports twice through the CLI and requires the mapping, byte-identical files on the second run, kept user changes, accounts that name the real config dirs, no secret value in any file or output, each thread with its account, folder, model, access and shelf, a running thread held back until it stops, a deleted one kept away, and unchanged T3 Code and account folders. Then it runs the Studio in process: the first-run offer, the import, a T3 Code change synced by the file watch, a user change kept, and sync off.
