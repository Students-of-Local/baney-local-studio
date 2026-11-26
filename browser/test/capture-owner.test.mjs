// A capture runs only while its session owns the tab: a start that finishes after the tab changed owner is refused,
// and a running capture stops collecting the moment the owner changes, not at the next sweep.
import assert from "node:assert/strict";
import test from "node:test";
import { CaptureError, CaptureManager } from "../src/browser/capture.ts";

/** A browser whose starts wait on `hold`, with ownership the test changes and no close or detach event. */
function setup(hold) {
	const log = { halted: [], discarded: [] };
	const ownership = new Map();
	const run = (id) => ({
		async stats() {
			return { bytes: 10 };
		},
		async halt() {
			log.halted.push(id);
		},
		async finish() {
			return { file: { kind: "har", encoding: "json", data: { log: {} }, bytes: 20 }, notes: [] };
		},
		async discard() {
			log.discarded.push(id);
		},
	});
	const backend = {
		async startNetwork(_tabId, id) {
			await hold;
			return run(id);
		},
		async startVideo(_tabId, id) {
			await hold;
			return run(id);
		},
		async owns(sessionId, tabId) {
			return ownership.get(`${sessionId}:${tabId}`) ?? true;
		},
	};
	let n = 0;
	const manager = new CaptureManager(backend, { newId: () => `c${++n}` });
	return { log, ownership, manager };
}
const alice = { id: "alice", tabIds: [10, 11] };

test("a start that finishes after its tab changed owner is refused and stopped", async () => {
	let open;
	const { log, ownership, manager } = setup(new Promise((r) => (open = r)));
	const started = manager.run(alice, { action: "network_start", tabId: 10 });
	await new Promise((r) => setImmediate(r));
	// The tab goes to another session while the browser is still starting the capture.
	ownership.set("alice:10", false);
	open();
	await assert.rejects(started, (err) => {
		assert.ok(err instanceof CaptureError, `a CaptureError, got ${err}`);
		assert.equal(err.code, "not_found");
		assert.match(err.message, /no longer one of the session's/);
		return true;
	});
	assert.deepEqual(log.discarded, ["c1"]);
	assert.equal(manager.size, 0);
	assert.deepEqual((await manager.run(alice, { action: "status" })).captures, []);
});

test("a running capture stops collecting as soon as its tab changes owner, and keeps what it had", async () => {
	const { log, ownership, manager } = setup(Promise.resolve());
	const a = await manager.run(alice, { action: "network_start", tabId: 10 });
	const b = await manager.run(alice, { action: "record_start", tabId: 11 });
	ownership.set("alice:10", false);
	await manager.ownersChanged();
	assert.deepEqual(log.halted, [a.id], "only the capture whose tab moved stops");
	const status = Object.fromEntries((await manager.run(alice, { action: "status" })).captures.map((c) => [c.id, c]));
	assert.equal(status[a.id].state, "ended");
	assert.match(status[a.id].endedBy, /no longer one of the session's/);
	assert.equal(status[b.id].state, "running");
	// What it held before the move can still be saved.
	assert.equal((await manager.run(alice, { action: "network_stop", id: a.id })).file.kind, "har");
});
