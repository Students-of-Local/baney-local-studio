// Second review of #296, first-F1: the weak-model eval names its MCP server once. claude names the tools after the
// config key, so the key, --allowedTools and what transcript.mjs reads back must all be the same name.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (f) => readFileSync(join(root, f), "utf8");

test("the eval's server key, allowed tools and transcript all name local-studio", () => {
	const run = read("evals/weak-model/run.mjs");
	const transcript = read("evals/weak-model/transcript.mjs");
	const key = run.match(/mcpServers: \{\s*"?([\w-]+)"?: \{/)?.[1];
	assert.equal(key, "local-studio");
	assert.match(run, new RegExp(`"--allowedTools", "mcp__${key}"`));
	assert.match(transcript, new RegExp(`s\\.name === "${key}"`));
	assert.match(transcript, new RegExp(`startsWith\\("mcp__${key}__"\\)`));
	assert.match(transcript, new RegExp(`replace\\(/\\^mcp__${key}__/`));
});
