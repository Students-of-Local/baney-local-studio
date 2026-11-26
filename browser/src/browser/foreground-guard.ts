/**
 * Foreground guard, runs in the service worker.
 *
 * Pages driven in the background open popups and target=_blank links, and Chrome
 * makes those new tabs active, stealing the user's focus. When a new tab's opener
 * is a tab some session owns, hand the tab to that session's group and put the
 * user's previous tab back in front.
 */

import { allOwnedTabIds, BrowserSession, sessionOwningTab } from "./session.js";

/** Tabs already handled, so the two events that report one popup adopt it once. */
const handled = new Set<number>();
let guardLog: (line: string) => void = () => undefined;

/** Route guard decisions into the bridge's debug log. */
export function setGuardLogger(log: (line: string) => void): void {
	guardLog = log;
}

async function onNewTab(tabId: number, sourceTabId: number, via: string): Promise<void> {
	if (handled.has(tabId)) return;
	const owned = await allOwnedTabIds();
	if (!owned.has(sourceTabId) || owned.has(tabId)) return;
	guardLog(`new tab=${tabId} from owned tab=${sourceTabId} (${via})`);
	const owner = await sessionOwningTab(sourceTabId);
	if (!owner) return;
	handled.add(tabId);
	setTimeout(() => handled.delete(tabId), 60000);
	const tab = await chrome.tabs.get(tabId).catch(() => undefined);
	if (!tab) return;

	// Restore whatever the user had active in that window before this tab appeared, but only
	// when the agent opened it from a hidden tab. If the user is looking at the opener (they
	// clicked a link on a shared tab themselves), the new tab is theirs to see. When the tab
	// before it is unknown, nothing is restored: any guess could take the user somewhere else.
	// A tab that opened in the background took nothing from the user: no need to read what they saw.
	const seen = tab.active ? await seenTabs() : undefined;
	const now = seen?.active[tab.windowId];
	const userWasOn = now === tabId ? seen?.previous[tab.windowId] : now;
	let restored = false;
	if (seen && userWasOn !== undefined && userWasOn !== sourceTabId) {
		const previous = await chrome.tabs.query({ windowId: tab.windowId });
		// The tab active right before the popup is what the user was looking at; lastActive is
		// the fallback (it can already point at the popup, which is not owned when activated).
		const target =
			previous.find((t) => t.id !== tabId && t.id === userWasOn) ??
			previous.find((t) => t.id !== tabId && t.id === seen.last[tab.windowId]);
		if (target?.id !== undefined) {
			await chrome.tabs.update(target.id, { active: true }).catch(() => undefined);
			restored = true;
		}
	}

	// Re-resolve the owner under the lock: the panel may have renamed it (first save) while
	// this ran, and opening the id read above would recreate the old record.
	const session = await BrowserSession.openOwnerOf(sourceTabId);
	if (!session) return;
	// The popup becomes the session's current tab, as the model would expect after a click. A
	// tab opened from one of the user's own tabs stays out of the group, like its opener.
	await session.adopt(tabId, true, { group: !session.isLoose(sourceTabId) });
	guardLog(`popup tab=${tabId} from=${sourceTabId} -> ${session.id}${restored ? " (focus restored)" : ""}`);
}

export function startForegroundGuard(): void {
	chrome.tabs.onCreated.addListener((tab) => {
		if (tab.id !== undefined && tab.openerTabId !== undefined)
			void onNewTab(tab.id, tab.openerTabId, "opener").catch((e) => guardLog(`guard error ${e}`));
	});
	// target=_blank implies noopener, and then Chrome may not set openerTabId; this event
	// still names the source tab.
	chrome.webNavigation.onCreatedNavigationTarget.addListener(
		(d) => void onNewTab(d.tabId, d.sourceTabId, "target").catch((e) => guardLog(`guard error ${e}`)),
	);

	// Remember the user's active tab per window so the guard can restore it.
	chrome.tabs.onActivated.addListener(async (info) => {
		await editSeen((s) => {
			const before = s.active[info.windowId];
			if (before !== undefined && before !== info.tabId) s.previous[info.windowId] = before;
			s.active[info.windowId] = info.tabId;
		});
		const owned = await allOwnedTabIds();
		if (!owned.has(info.tabId))
			await editSeen((s) => {
				s.last[info.windowId] = info.tabId;
			});
	});
}

/**
 * Per window: the active tab (any tab), the one before it, and the last one no session owns, to tell
 * who opened a new tab. Kept in chrome.storage.session: the worker idles out between events, and a
 * popup that wakes it must still find the tab the user was on.
 */
interface SeenTabs {
	active: Record<number, number>;
	previous: Record<number, number>;
	last: Record<number, number>;
}
const SEEN_KEY = "foreground_guard_tabs";
let seen: Promise<SeenTabs> | undefined;
/** Edits run one at a time, each on the state the one before left. */
let seenEdits: Promise<unknown> = Promise.resolve();

function loadSeen(): Promise<SeenTabs> {
	seen ??= chrome.storage.session
		.get(SEEN_KEY)
		.then((d) => d[SEEN_KEY] as SeenTabs | undefined)
		.catch(() => undefined)
		.then((v) => ({ active: { ...v?.active }, previous: { ...v?.previous }, last: { ...v?.last } }));
	return seen;
}

/** The state after every activation already reported. */
async function seenTabs(): Promise<SeenTabs> {
	await seenEdits;
	return loadSeen();
}

function editSeen(change: (s: SeenTabs) => void): Promise<void> {
	const next = seenEdits.then(async () => {
		const s = await loadSeen();
		change(s);
		await chrome.storage.session.set({ [SEEN_KEY]: s }).catch(() => undefined);
	});
	seenEdits = next.catch(() => undefined);
	return next;
}
