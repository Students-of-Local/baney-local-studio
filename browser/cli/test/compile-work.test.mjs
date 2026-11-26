// scripts/compile.mjs --work: build.mjs empties its out dir first, so compile refuses a folder that holds anything but
// an earlier compile's output, and reads a relative --work against the caller's folder, as build.mjs then gets it.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const cliRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const compile = join(cliRoot, "scripts", "compile.mjs");
const work = mkdtempSync(join(tmpdir(), "local-studio-compile-work-"));
after(() => rmSync(work, { recursive: true, force: true }));

const run = (workArg, cwd = cliRoot) =>
	spawnSync(process.execPath, [compile, "--arch", "x64", "--out", join(work, "never"), "--work", workArg], { cwd, encoding: "utf8" });

test("compile refuses a --work folder that already holds files, and leaves them", () => {
	const kept = join(work, "kept");
	mkdirSync(kept);
	writeFileSync(join(kept, "mine.txt"), "keep");
	const r = run(kept);
	assert.equal(r.status, 2);
	assert.match(r.stderr, /--work .* is not empty/);
	assert.equal(readFileSync(join(kept, "mine.txt"), "utf8"), "keep");
});

test("a relative --work is the caller's folder, not this package's", () => {
	const caller = join(work, "caller");
	mkdirSync(join(caller, "here"), { recursive: true });
	writeFileSync(join(caller, "here", "mine.txt"), "keep");
	const r = run("here", caller);
	assert.equal(r.status, 2);
	assert.match(r.stderr, new RegExp(`--work ${join(caller, "here")} is not empty`));
	assert.equal(readFileSync(join(caller, "here", "mine.txt"), "utf8"), "keep");
});

test("a --work that is a file, or empty, is refused with exit 2", () => {
	const file = join(work, "a-file.txt");
	writeFileSync(file, "keep");
	const r = run(file);
	assert.equal(r.status, 2);
	assert.match(r.stderr, /is not a folder/);
	assert.equal(readFileSync(file, "utf8"), "keep");
	const e = run("");
	assert.equal(e.status, 2);
	assert.match(e.stderr, /--work needs a folder/);
});

test("a --work folder an earlier compile built is taken again", () => {
	const again = join(work, "again");
	mkdirSync(again);
	writeFileSync(join(again, ".local-studio-compile"), "");
	writeFileSync(join(again, "index.js"), "");
	const r = run(again);
	assert.doesNotMatch(r.stderr, /is not empty|is not a folder/);
});
