// Compiles the CLI into one executable with no Node needed on the machine: the esbuild bundle (build.mjs, in
// compiled mode) goes through `bun build --compile --bytecode`. Bun beat Node's single-executable applications
// on cold start, size and cross-building.
//   node scripts/compile.mjs --arch arm64|x64 --out <file> [--os linux|darwin|windows] [--work <dir>]
// --os cross-builds for another system (the release builds all three on one Linux runner); the default is this one.
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const arg = (name, fallback) => {
	const i = process.argv.indexOf(name);
	return i >= 0 ? process.argv[i + 1] : fallback;
};
const arch = arg("--arch", process.arch === "arm64" ? "arm64" : "x64");
const out = arg("--out");
if (!["arm64", "x64"].includes(arch) || !out) {
	console.error("usage: node scripts/compile.mjs --arch arm64|x64 --out <file>");
	process.exit(2);
}
const given = arg("--work");
if (given === "") {
	console.error("compile: --work needs a folder");
	process.exit(2);
}
// Absolute, since build.mjs runs from this package's folder and would read a relative path against it.
const work = resolve(given ?? mkdtempSync(join(tmpdir(), "local-studio-compile-")));
// build.mjs empties its out dir first, so a --work holding anything but an earlier compile's output would be deleted
// with it. STAMP marks a folder compile built.
const STAMP = ".local-studio-compile";
if (given !== undefined && existsSync(work)) {
	if (!statSync(work).isDirectory()) {
		console.error(`compile: --work ${work} is not a folder`);
		process.exit(2);
	}
	const entries = readdirSync(work);
	if (entries.length > 0 && !entries.includes(STAMP)) {
		console.error(`compile: --work ${work} is not empty; give a new or empty folder, or one compile built`);
		process.exit(2);
	}
}
const run = (cmd, args, env = {}) => {
	const r = spawnSync(cmd, args, { cwd: root, stdio: "inherit", env: { ...process.env, ...env } });
	if (r.status !== 0) {
		console.error(`compile: ${cmd} ${args.join(" ")} failed (${r.status ?? r.signal})`);
		process.exit(1);
	}
};
run(process.execPath, ["scripts/build.mjs"], { LOCAL_STUDIO_BROWSER_CLI_COMPILED: "1", LOCAL_STUDIO_BROWSER_CLI_OUT_DIR: work });
writeFileSync(join(work, STAMP), "");
// This machine's system unless --os names another: the Mac app's build, and a real executable to run wherever the tests run.
const os = arg("--os", process.platform === "win32" ? "windows" : process.platform);
if (!["linux", "darwin", "windows"].includes(os)) {
	console.error("compile: --os takes linux, darwin or windows");
	process.exit(2);
}
run("bun", ["build", "--compile", "--bytecode", `--target=bun-${os}-${arch === "x64" ? "x64-baseline" : arch}`, join(work, "index.js"), "--outfile", out]);
// The x86_64 build is the baseline one: it runs on every Intel Mac and under Rosetta (which has no AVX).
chmodSync(out, 0o755);
if (given === undefined) rmSync(work, { recursive: true, force: true });
