# localstudio plugin

The `localstudio` command wires Local Studio into T3 Code: `install` adds the Local Studio provider, `probe` reports this machine's GPUs, and `find` picks models that fit it.

## Install

```
localstudio install [--harness omp] [--t3-home P] [--omp-dir P] [--controller URL] [--dry-run] [--json]
```

`install` adds your local models to the harness you already use and registers no harness of its own, so T3 Code shows one omp entry. It writes a `localstudio` provider with the controller's models into omp's `models.yml`, which every omp instance reads, and keeps your other providers (Synthetic included) as they are. In T3's `settings.json` it uses your omp entry, or adds one named `omp` when there is none.

An older install added a separate "Local Studio" omp entry. `install` removes it, and its threads continue under omp. If that entry ran with an `--approval-mode=` other than `yolo` and your omp entry has none, your omp entry takes it. Install never makes an existing omp entry more permissive: an old `--approval-mode=yolo` is not carried, and install prints how to add it yourself. A new omp entry gets the old entry's approval mode, or `--approval-mode=yolo` when there was none.

`--harness` takes a comma list. Only `omp` is supported now, and it is the default; Claude Code is not supported yet.

## Extensions

T3 Code has no MCP setting of its own, so `extensions` writes our MCP servers into the config of each agent T3 runs: omp's `mcp.json`, and every enabled Claude instance through `claude mcp add-json` with its tools allowed in that instance's `settings.json`. Codex and other drivers are listed as skipped.

```
localstudio extensions list [--json]
localstudio extensions add <name> [--json] [--dry-run] [--t3-home P] [--omp-dir P]
localstudio extensions add <name> --command <path> [--arg <a>]... [--json] [--dry-run]
localstudio extensions remove <name> [--json] [--dry-run]
```

`browser` (needs `local-studio-browser`) and `skills` (needs `local-studio`) are built in. Any other name is a custom extension and needs `--command`. `install` adds `browser` itself when it finds the browser.
