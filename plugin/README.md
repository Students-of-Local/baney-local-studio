# localstudio plugin

The `localstudio` command wires Local Studio into T3 Code: `install` adds the Local Studio provider, `probe` reports this machine's GPUs, and `find` picks models that fit it.

## Extensions

T3 Code has no MCP setting of its own, so `extensions` writes our MCP servers into the config of each agent T3 runs: omp's `mcp.json`, and every enabled Claude instance through `claude mcp add-json` with its tools allowed in that instance's `settings.json`. Codex and other drivers are listed as skipped.

```
localstudio extensions list [--json]
localstudio extensions add <name> [--json] [--dry-run] [--t3-home P] [--omp-dir P]
localstudio extensions add <name> --command <path> [--arg <a>]... [--json] [--dry-run]
localstudio extensions remove <name> [--json] [--dry-run]
```

`browser` (needs `local-studio-browser`) and `skills` (needs `local-studio`) are built in. Any other name is a custom extension and needs `--command`. `install` adds `browser` itself when it finds the browser.
