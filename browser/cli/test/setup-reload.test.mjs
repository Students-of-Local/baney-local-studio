// `local-studio-browser setup` with an old extension connected, against a fake native host: it reloads the
// extension when no agent is attached, and leaves it alone while one is.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

const cli = join(dirname(fileURLToPath(import.meta.url)), "..", "dist", "index.js");
const built = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "static", "manifest.chrome.json"), "utf8")).version;

const frame = (m) => {
	const body = Buffer.from(JSON.stringify(m));
	const head = Buffer.alloc(4);
	head.writeUInt32LE(body.length);
	return Buffer.concat([head, body]);
};

let root;
let server;
let version;
let sessions;
let reloads;

before(async () => {
	root = mkdtempSync(join(tmpdir(), "local-studio-setup-reload-"));
	for (const dir of ["home", "bin", "build", "apps"]) mkdirSync(join(root, dir));
	mkdirSync(join(root, "sock"), { mode: 0o700 });
	symlinkSync(process.execPath, join(root, "bin", "node"));
	const support = process.platform === "darwin" ? join(root, "home", "Library", "Application Support") : join(root, "home", ".config");
	mkdirSync(join(support, "net.imput.helium"), { recursive: true });
	mkdirSync(join(root, "apps", "Helium.app"));
	for (const f of ["background.js", "sidepanel.js", "sidepanel.html", "app.css"]) writeFileSync(join(root, "build", f), "");
	writeFileSync(join(root, "build", "manifest.json"), JSON.stringify({ name: "Local Studio", version: built }));
	server = createServer((socket) => {
		let buffered = Buffer.alloc(0);
		socket.on("error", () => undefined);
		socket.on("data", (chunk) => {
			buffered = Buffer.concat([buffered, chunk]);
			while (buffered.length >= 4 && buffered.length >= 4 + buffered.readUInt32LE(0)) {
				const msg = JSON.parse(buffered.subarray(4, 4 + buffered.readUInt32LE(0)).toString());
				buffered = buffered.subarray(4 + buffered.readUInt32LE(0));
				const answer = (result) => socket.write(frame({ type: "response", id: msg.id, result }));
				if (msg.type === "hello") socket.write(frame({ type: "welcome", clientId: "c1", hostVersion: "0", extensionConnected: true }));
				else if (msg.method === "bridge.debug") answer({ version });
				else if (msg.method === "sessions.list" && sessions === undefined) socket.write(frame({ type: "response", id: msg.id, error: { code: "unknown_method", message: "sessions.list" } }));
				else if (msg.method === "sessions.list") answer({ sessions });
				else if (msg.method === "bridge.reload") {
					reloads++;
					version = built;
					answer({});
				}
			}
		});
	});
	const path = join(root, "sock", `${process.pid}.sock`);
	await new Promise((resolve) => server.listen(path, resolve));
	chmodSync(path, 0o600);
});
after(() => {
	server.close();
	rmSync(root, { recursive: true, force: true });
});

async function setup(...args) {
	version = "0.0.1";
	reloads = 0;
	const child = spawn(process.execPath, [cli, "setup", "--yes", ...args], {
		env: {
			HOME: join(root, "home"),
			PATH: join(root, "bin"),
			LOCAL_STUDIO_BROWSER_SOCKET_DIR: join(root, "sock"),
			LOCAL_STUDIO_BROWSER_EXTENSION_DIR: join(root, "build"),
			LOCAL_STUDIO_BROWSER_APPLICATIONS_DIRS: join(root, "apps"),
		},
		stdio: ["ignore", "pipe", "pipe"],
	});
	let out = "";
	child.stdout.on("data", (d) => {
		out += d;
	});
	child.stderr.on("data", (d) => {
		out += d;
	});
	const code = await new Promise((resolve) => child.on("close", resolve));
	return { code, out };
}

test("an old extension nobody is using is reloaded onto this build", async () => {
	for (const listed of [[], [{ name: "done", connected: 0 }], undefined]) {
		sessions = listed;
		const { code, out } = await setup("--timeout", "20");
		assert.equal(reloads, 1, out);
		assert.match(out, /Connected/);
		assert.equal(code, 0, out);
	}
});

test("an old extension an agent is working in is left alone, with the next step", async () => {
	sessions = [{ name: "research", connected: 1 }];
	for (const args of [["--no-wait"], ["--timeout", "20"]]) {
		const { code, out } = await setup(...args);
		assert.equal(reloads, 0, out);
		assert.match(out, /Not reloaded: an agent is working in the browser \(research\)/);
		assert.match(out, /local-studio-browser reload/);
		assert.equal(code, 0, out);
	}
});
