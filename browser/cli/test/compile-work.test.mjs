// scripts/compile.mjs --work: build.mjs empties its out dir first, so compile refuses a folder that already holds files.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const cliRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const work = mkdtempSync(join(tmpdir(), "local-studio-compile-work-"));
after(() => rmSync(work, { recursive: true, force: true }));

test("compile refuses a --work folder that already holds files, and leaves them", () => {
	const kept = join(work, "kept");
	mkdirSync(kept);
	writeFileSync(join(kept, "mine.txt"), "keep");
	const r = spawnSync(process.execPath, ["scripts/compile.mjs", "--arch", "x64", "--out", join(work, "never"), "--work", kept], { cwd: cliRoot, encoding: "utf8" });
	assert.equal(r.status, 2);
	assert.match(r.stderr, /--work .* is not empty/);
	assert.equal(readFileSync(join(kept, "mine.txt"), "utf8"), "keep");
});
