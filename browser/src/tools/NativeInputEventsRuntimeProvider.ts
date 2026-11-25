import type { SandboxRuntimeProvider } from "@mariozechner/pi-web-ui";
import { getCurrentBrowserSession } from "../browser/current.js";
import { NATIVE_INPUT_EVENTS_DESCRIPTION } from "../prompts/prompts.js";

interface KeyInfo {
	key: string;
	code: string;
	keyCode: number;
	shift?: boolean;
	text?: string;
}

// Character -> [code, Windows virtual key code, needs Shift]
const PUNCTUATION_KEYS: Record<string, [string, number, boolean]> = {
	".": ["Period", 190, false],
	",": ["Comma", 188, false],
	"-": ["Minus", 189, false],
	"=": ["Equal", 187, false],
	";": ["Semicolon", 186, false],
	"'": ["Quote", 222, false],
	"/": ["Slash", 191, false],
	"\\": ["Backslash", 220, false],
	"`": ["Backquote", 192, false],
	"[": ["BracketLeft", 219, false],
	"]": ["BracketRight", 221, false],
	")": ["Digit0", 48, true],
	"!": ["Digit1", 49, true],
	"@": ["Digit2", 50, true],
	"#": ["Digit3", 51, true],
	$: ["Digit4", 52, true],
	"%": ["Digit5", 53, true],
	"^": ["Digit6", 54, true],
	"&": ["Digit7", 55, true],
	"*": ["Digit8", 56, true],
	"(": ["Digit9", 57, true],
	_: ["Minus", 189, true],
	"+": ["Equal", 187, true],
	":": ["Semicolon", 186, true],
	'"': ["Quote", 222, true],
	"?": ["Slash", 191, true],
	"|": ["Backslash", 220, true],
	"~": ["Backquote", 192, true],
	"{": ["BracketLeft", 219, true],
	"}": ["BracketRight", 221, true],
	"<": ["Comma", 188, true],
	">": ["Period", 190, true],
};

/**
 * Provides native input event functions to JavaScript REPL using Chrome Debugger API.
 * Dispatches REAL browser events (isTrusted: true) for automation of anti-bot sites.
 * Operates on the session's current tab, which need not be visible.
 */
export class NativeInputEventsRuntimeProvider implements SandboxRuntimeProvider {
	private modifiers = 0; // Track currently pressed modifiers
	// Modifier bit flags for CDP
	private readonly MODIFIER_ALT = 1;
	private readonly MODIFIER_CTRL = 2;
	private readonly MODIFIER_META = 4;
	private readonly MODIFIER_SHIFT = 8;

	getData(): Record<string, any> {
		return {};
	}

	/**
	 * The session's current tab, with the debugger attached.
	 */
	private async getActiveTabId(): Promise<number> {
		const tab = await getCurrentBrowserSession().attachedCurrentTab();
		if (!tab.id) {
			throw new Error("No tab available");
		}
		return tab.id;
	}

	private getKeyInfo(key: string): KeyInfo {
		// Key mapping: simple names -> CDP parameters
		// Supports standard key names that LLMs typically know
		const keyMap: Record<string, { key: string; code: string; keyCode: number }> = {
			// Arrow keys
			ArrowRight: { key: "ArrowRight", code: "ArrowRight", keyCode: 39 },
			ArrowLeft: { key: "ArrowLeft", code: "ArrowLeft", keyCode: 37 },
			ArrowUp: { key: "ArrowUp", code: "ArrowUp", keyCode: 38 },
			ArrowDown: { key: "ArrowDown", code: "ArrowDown", keyCode: 40 },

			// Navigation keys
			Enter: { key: "Enter", code: "Enter", keyCode: 13 },
			Tab: { key: "Tab", code: "Tab", keyCode: 9 },
			Escape: { key: "Escape", code: "Escape", keyCode: 27 },
			Backspace: { key: "Backspace", code: "Backspace", keyCode: 8 },
			Delete: { key: "Delete", code: "Delete", keyCode: 46 },
			Home: { key: "Home", code: "Home", keyCode: 36 },
			End: { key: "End", code: "End", keyCode: 35 },
			PageUp: { key: "PageUp", code: "PageUp", keyCode: 33 },
			PageDown: { key: "PageDown", code: "PageDown", keyCode: 34 },

			// Modifier keys
			Shift: { key: "Shift", code: "ShiftLeft", keyCode: 16 },
			Control: { key: "Control", code: "ControlLeft", keyCode: 17 },
			Alt: { key: "Alt", code: "AltLeft", keyCode: 18 },
			Meta: { key: "Meta", code: "MetaLeft", keyCode: 91 },

			// Function keys
			F1: { key: "F1", code: "F1", keyCode: 112 },
			F2: { key: "F2", code: "F2", keyCode: 113 },
			F3: { key: "F3", code: "F3", keyCode: 114 },
			F4: { key: "F4", code: "F4", keyCode: 115 },
			F5: { key: "F5", code: "F5", keyCode: 116 },
			F6: { key: "F6", code: "F6", keyCode: 117 },
			F7: { key: "F7", code: "F7", keyCode: 118 },
			F8: { key: "F8", code: "F8", keyCode: 119 },
			F9: { key: "F9", code: "F9", keyCode: 120 },
			F10: { key: "F10", code: "F10", keyCode: 121 },
			F11: { key: "F11", code: "F11", keyCode: 122 },
			F12: { key: "F12", code: "F12", keyCode: 123 },

			// Special keys
			Space: { key: " ", code: "Space", keyCode: 32 },
			Insert: { key: "Insert", code: "Insert", keyCode: 45 },
		};

		// Check if it's in the keyMap first
		const keyInfo = keyMap[key];
		if (keyInfo) {
			return keyInfo;
		}

		// For single character keys (a-z, A-Z, 0-9, etc.), generate the info
		if (key.length === 1) {
			const char = key;
			const upperChar = char.toUpperCase();
			const keyCode = upperChar.charCodeAt(0);

			// Letter keys (a-z, A-Z)
			if (/[a-zA-Z]/.test(char)) {
				return {
					key: char,
					code: `Key${upperChar}`,
					keyCode: keyCode,
				};
			}

			// Number keys (0-9)
			if (/[0-9]/.test(char)) {
				return {
					key: char,
					code: `Digit${char}`,
					keyCode: keyCode,
				};
			}

			// Punctuation on a US layout: its own code and virtual key, with Shift for the shifted ones
			const punct = PUNCTUATION_KEYS[char];
			if (punct) {
				return { key: char, code: punct[0], keyCode: punct[1], shift: punct[2] };
			}

			// Anything else goes in as text, with no virtual key code that would name another key
			return { key: char, code: "", keyCode: 0, text: char };
		}

		throw new Error(
			`Unknown key name: ${key}. Supported keys: ${Object.keys(keyMap).join(", ")}, or any single character (a-z, 0-9, etc.)`,
		);
	}

	getRuntime(): (sandboxId: string) => void {
		// This function will be stringified and injected into the user script
		return (_sandboxId: string) => {
			const sendRuntimeMessage = (window as any).sendRuntimeMessage;
			if (typeof sendRuntimeMessage !== "function") {
				throw new Error("sendRuntimeMessage is not available in this context");
			}

			(window as any).nativeClick = async (selector: string): Promise<void> => {
				const response = await sendRuntimeMessage({
					type: "native-input",
					action: "click",
					selector,
				});
				// sendRuntimeMessage throws on error, so if we get here, it succeeded
			};

			(window as any).nativeType = async (selector: string, text: string): Promise<void> => {
				const response = await sendRuntimeMessage({
					type: "native-input",
					action: "type",
					selector,
					text,
				});
			};

			(window as any).nativePress = async (key: string): Promise<void> => {
				const response = await sendRuntimeMessage({
					type: "native-input",
					action: "press",
					key,
				});
			};

			(window as any).nativeKeyDown = async (key: string): Promise<void> => {
				const response = await sendRuntimeMessage({
					type: "native-input",
					action: "keyDown",
					key,
				});
			};

			(window as any).nativeKeyUp = async (key: string): Promise<void> => {
				const response = await sendRuntimeMessage({
					type: "native-input",
					action: "keyUp",
					key,
				});
			};
		};
	}

	async handleMessage(message: any, respond: (response: any) => void): Promise<void> {
		if (message.type !== "native-input") {
			return;
		}

		console.log("[NativeInput] Received event:", message.action, message);

		// Session tab with the debugger attached (attachment is kept for the tab's lifetime)
		let tabId: number;
		try {
			tabId = await this.getActiveTabId();
		} catch (error: any) {
			respond({ success: false, error: error.message || String(error) });
			return;
		}

		try {
			if (message.action === "click") {
				console.log("[NativeInput] Finding element:", message.selector);

				// Find element and get its center coordinates
				const result = (await chrome.debugger.sendCommand({ tabId: tabId }, "Runtime.evaluate", {
					expression: `(() => {
							const sel = ${JSON.stringify(message.selector)};
							const el = document.querySelector(sel);
							if (!el) throw new Error('Selector not found: ' + sel);
							const rect = el.getBoundingClientRect();
							return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
						})()`,
					returnByValue: true,
				})) as any;

				console.log("[NativeInput] Element eval result:", result);

				if (result?.exceptionDetails) {
					console.error("[NativeInput] Element not found:", result.exceptionDetails);
					throw new Error(result.exceptionDetails.exception.description || "Element not found");
				}

				const { x, y } = result.result.value;
				console.log("[NativeInput] Clicking at coordinates:", { x, y });

				// Dispatch trusted mouse events
				const pressResult = await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchMouseEvent", {
					type: "mousePressed",
					x,
					y,
					button: "left",
					clickCount: 1,
				});
				console.log("[NativeInput] Mouse pressed result:", pressResult);

				const releaseResult = await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchMouseEvent", {
					type: "mouseReleased",
					x,
					y,
					button: "left",
					clickCount: 1,
				});
				console.log("[NativeInput] Mouse released result:", releaseResult);

				console.log("[NativeInput] Click completed successfully");
				respond({ success: true });
			} else if (message.action === "type") {
				console.log("[NativeInput] Typing text:", message.text, "into:", message.selector);

				// Focus element first
				const focusResult = (await chrome.debugger.sendCommand({ tabId: tabId }, "Runtime.evaluate", {
					expression: `(() => {
							const sel = ${JSON.stringify(message.selector)};
							const el = document.querySelector(sel);
							if (!el) throw new Error('Selector not found: ' + sel);
							el.focus();
							return true;
						})()`,
					returnByValue: true,
				})) as any;

				console.log("[NativeInput] Focus result:", focusResult);

				if (focusResult?.exceptionDetails) {
					console.error("[NativeInput] Element not found for typing:", focusResult.exceptionDetails);
					throw new Error(focusResult.exceptionDetails.exception.description || "Element not found");
				}

				// Type each character
				for (const char of message.text) {
					await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchKeyEvent", {
						type: "keyDown",
						text: char,
					});

					await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchKeyEvent", {
						type: "keyUp",
						text: char,
					});
				}

				console.log("[NativeInput] Typing completed successfully");
				respond({ success: true });
			} else if (message.action === "press") {
				console.log("[NativeInput] Pressing key:", message.key);

				const keyInfo = this.getKeyInfo(message.key);
				const keyFields = keyInfo.keyCode
					? {
							key: keyInfo.key,
							code: keyInfo.code,
							windowsVirtualKeyCode: keyInfo.keyCode,
							nativeVirtualKeyCode: keyInfo.keyCode,
							...(keyInfo.shift ? { modifiers: this.MODIFIER_SHIFT } : {}),
						}
					: { key: keyInfo.key };

				// Press single key with proper CDP parameters
				const keyDownResult = await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchKeyEvent", {
					type: "keyDown",
					...keyFields,
					...(keyInfo.text ? { text: keyInfo.text } : {}),
				});
				console.log("[NativeInput] Key down result:", keyDownResult);

				const keyUpResult = await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchKeyEvent", {
					type: "keyUp",
					...keyFields,
				});
				console.log("[NativeInput] Key up result:", keyUpResult);

				console.log("[NativeInput] Key press completed successfully");
				respond({ success: true });
			} else if (message.action === "keyDown") {
				console.log("[NativeInput] Key down:", message.key);

				const keyInfo = this.getKeyInfo(message.key);

				// Update modifier state
				if (message.key === "Alt") this.modifiers |= this.MODIFIER_ALT;
				if (message.key === "Control") this.modifiers |= this.MODIFIER_CTRL;
				if (message.key === "Meta") this.modifiers |= this.MODIFIER_META;
				if (message.key === "Shift") this.modifiers |= this.MODIFIER_SHIFT;

				const keyDownResult = await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchKeyEvent", {
					type: "keyDown",
					key: keyInfo.key,
					code: keyInfo.code,
					windowsVirtualKeyCode: keyInfo.keyCode,
					nativeVirtualKeyCode: keyInfo.keyCode,
					modifiers: this.modifiers,
				});
				console.log("[NativeInput] Key down result:", keyDownResult, "modifiers:", this.modifiers);

				console.log("[NativeInput] Key down completed successfully");
				respond({ success: true });
			} else if (message.action === "keyUp") {
				console.log("[NativeInput] Key up:", message.key);

				const keyInfo = this.getKeyInfo(message.key);

				const keyUpResult = await chrome.debugger.sendCommand({ tabId: tabId }, "Input.dispatchKeyEvent", {
					type: "keyUp",
					key: keyInfo.key,
					code: keyInfo.code,
					windowsVirtualKeyCode: keyInfo.keyCode,
					nativeVirtualKeyCode: keyInfo.keyCode,
					modifiers: this.modifiers,
				});
				console.log("[NativeInput] Key up result:", keyUpResult, "modifiers:", this.modifiers);

				// Update modifier state after keyUp
				if (message.key === "Alt") this.modifiers &= ~this.MODIFIER_ALT;
				if (message.key === "Control") this.modifiers &= ~this.MODIFIER_CTRL;
				if (message.key === "Meta") this.modifiers &= ~this.MODIFIER_META;
				if (message.key === "Shift") this.modifiers &= ~this.MODIFIER_SHIFT;

				console.log("[NativeInput] Key up completed successfully");
				respond({ success: true });
			} else {
				console.error("[NativeInput] Unknown action:", message.action);
				respond({ success: false, error: `Unknown action: ${message.action}` });
			}
		} catch (error: any) {
			console.error("[NativeInput] Error during operation:", error);
			respond({ success: false, error: error.message || String(error) });
		}
	}

	getDescription(): string {
		return NATIVE_INPUT_EVENTS_DESCRIPTION;
	}
}
