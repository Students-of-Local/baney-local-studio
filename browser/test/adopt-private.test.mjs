// Second review of #296, first-F2: a thread an earlier build saved in private mode stays private after the move to
// this build's storage, so the bridge and the local mirror, which read localStudioPrivate, never treat it as public.
import assert from "node:assert/strict";
import { test } from "node:test";
import { moveRecords } from "../src/storage/adopt.ts";

const stores = (data) => ({
	keys: async (store) => Object.keys(data[store] ?? {}),
	get: async (store, key) => data[store][key],
	has: async (store, key) => Object.hasOwn(data[store] ?? {}, key),
	set: async (store, key, value) => {
		data[store] ??= {};
		data[store][key] = value;
	},
});

test("a thread an earlier build marked private is private under this build's mark too", async () => {
	const from = {
		sessions: { a: { id: "a", tabkyraPrivate: true } },
		"sessions-metadata": { a: { id: "a", title: "t", tabkyraPrivate: true } },
	};
	const to = {};
	await moveRecords(stores(from), stores(to), ["sessions", "sessions-metadata"]);
	assert.equal(to.sessions.a.localStudioPrivate, true);
	assert.equal(to["sessions-metadata"].a.localStudioPrivate, true);
	assert.equal(to["sessions-metadata"].a.title, "t");
});

test("a thread that was never private stays unmarked", async () => {
	const to = {};
	await moveRecords(stores({ sessions: { b: { id: "b", tabkyraPrivate: false } } }), stores(to), ["sessions"]);
	assert.equal(to.sessions.b.localStudioPrivate, undefined);
});
