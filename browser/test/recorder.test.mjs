// The interaction recorder (src/recording/recorder.ts): what it records from a page, and what the saved skill's replay
// does on one. The page is happy-dom, which forwards a label's click to its control and submits a form from its submit
// button as Chrome does; the injected script and the replay library run against it through the page globals.
import assert from "node:assert/strict";
import test from "node:test";
import { Window } from "happy-dom";
import { buildSkillFromRecording, Recorder, toJsIdentifier } from "../src/recording/recorder.ts";

const GLOBALS = ["CSS", "Element", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "Event"];

/** A page with `html` in its body; `use` points the page globals at it. */
function page(html) {
	const win = new Window({ url: "https://shop.example/checkout" });
	win.document.body.innerHTML = html;
	return win;
}
function use(win) {
	globalThis.window = win;
	globalThis.document = win.document;
	for (const k of GLOBALS) globalThis[k] = win[k];
}

/** chrome.* for the recorder, on one tab of `win`. `active()` says whether the tab is in front when asked. */
function fakeChrome(win, { active = () => true } = {}) {
	const listeners = new Set();
	const shots = [];
	globalThis.chrome = {
		tabs: {
			async query() {
				return [{ id: 7, windowId: 3, url: win.location.href, active: true }];
			},
			async get(id) {
				return { id, windowId: 3, url: win.location.href, active: active() };
			},
			async captureVisibleTab(windowId) {
				shots.push(windowId);
				return "data:image/jpeg;base64,AAAA";
			},
		},
		runtime: {
			onMessage: { addListener: (fn) => listeners.add(fn), removeListener: (fn) => listeners.delete(fn) },
			sendMessage(msg) {
				for (const fn of listeners) fn(structuredClone(msg), { tab: { id: 7 } });
			},
		},
		webNavigation: { onCompleted: { addListener() {}, removeListener() {} } },
		scripting: {
			async executeScript({ func }) {
				use(win);
				return [{ result: func() }];
			},
		},
	};
	return { shots };
}

/** Record what `act` does on `win`. */
async function record(win, act, options) {
	const fake = fakeChrome(win, options);
	const recorder = new Recorder();
	await recorder.start();
	await act(win.document);
	return { rec: await recorder.stop(), shots: fake.shots };
}

/** The user types `value` into a field: its value changes, then it fires change. */
function type(win, el, value) {
	el.value = value;
	el.dispatchEvent(new win.Event("change", { bubbles: true }));
}

/** The saved skill's replay object, loaded into `win` as browserjs() would. */
function replayer(win, rec, name = "flow") {
	const skill = buildSkillFromRecording(rec, { name });
	use(win);
	new Function(skill.library)();
	return win[toJsIdentifier(name)];
}

/** Count the submits the page sees; its script handles them (preventDefault plus fetch). */
function countSubmits(win) {
	const seen = { submits: 0 };
	win.document.addEventListener("submit", (e) => {
		e.preventDefault();
		seen.submits++;
	});
	return seen;
}

const step = (type, selector, extra = {}) => ({ type, selector, timestamp: 1, ...extra });

test("replay picks an option in a select, and stops at a step that fails instead of going on to submit", async () => {
	const html = `<form id="f"><select id="size"><option>S</option><option>M</option></select><input type="file" id="doc"></form>`;
	const win = page(html);
	const seen = countSubmits(win);
	const pick = replayer(win, { hostname: "shop.example", startUrl: "", steps: [step("input", "#size", { tag: "select", value: "M" }), step("submit", "#f", { tag: "form" })], screenshots: [] });
	const picked = await pick.replay();
	assert.deepEqual(picked.map((r) => r.status), ["typed", "submitted"], JSON.stringify(picked));
	assert.equal(win.document.getElementById("size").value, "M");
	assert.equal(seen.submits, 1);
	// A file input refuses a typed value: the replay ends there, and the form is not sent.
	const bad = replayer(win, { hostname: "shop.example", startUrl: "", steps: [step("input", "#doc", { tag: "input", value: "C:\\x" }), step("submit", "#f", { tag: "form" })], screenshots: [] }, "bad");
	const stopped = await bad.replay();
	assert.deepEqual(stopped.map((r) => r.status), ["error"]);
	assert.equal(seen.submits, 1);
});

test("one step per user action: a label's forwarded click and a submit button's submit add none, and replay sends once", async () => {
	const html = `<form id="f"><label for="agree">I agree</label><input type="checkbox" id="agree"><button id="pay">Pay</button></form>`;
	const win = page(html);
	countSubmits(win);
	// Clicking the label also clicks the checkbox; clicking the button (or Enter in a field) also submits the form.
	const { rec } = await record(win, (d) => {
		d.querySelector("label").click();
		d.getElementById("pay").click();
	});
	const acts = rec.steps.filter((s) => s.type !== "input");
	assert.deepEqual(
		acts.map((s) => [s.type, s.tag]),
		[
			["click", "label"],
			["click", "button"],
		],
	);
	const fresh = page(html);
	const seen = countSubmits(fresh);
	await replayer(fresh, rec).replay();
	assert.equal(fresh.document.getElementById("agree").checked, true, "the box was toggled twice");
	assert.equal(seen.submits, 1);
	// A recording made before this holds the submit as well: replay still sends the form once.
	const again = page(html);
	const seenAgain = countSubmits(again);
	const steps = [step("click", "#pay", { tag: "button", text: "Pay" }), step("submit", "#f", { tag: "form" })];
	await replayer(again, { hostname: "shop.example", startUrl: "", steps, screenshots: [] }).replay();
	assert.equal(seenAgain.submits, 1);
});
