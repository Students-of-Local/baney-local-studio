// The context guard leaves Local Studio models alone (second review of #291, first-F1): Eukhe runs its own instructions and
// tools, so the panel's 13k-token fixed part must not make a small window "too small", and old turns must not be cut,
// since the first user message is what keeps the chat in one Eukhe session.
import assert from "node:assert/strict";
import { test } from "node:test";
import { createAssistantMessageEventStream } from "@mariozechner/pi-ai";

const { guardContext, resetLearnedWindows } = await import("../src/models/context-guard.ts");
const { LOCAL_STUDIO_PROVIDER } = await import("../src/eukhe/models.ts");

const tools = Array.from({ length: 13 }, (_, i) => ({ name: `tool_${i}`, description: "x".repeat(2000), parameters: { type: "object", properties: {} } }));
const history = Array.from({ length: 60 }, (_, i) => ({ role: i % 2 ? "assistant" : "user", content: [{ type: "text", text: `turn ${i} ${"word ".repeat(400)}` }], timestamp: i }));
const context = { systemPrompt: "s".repeat(20_000), messages: history, tools };

function recording() {
	const sent = [];
	const inner = async (model, ctx) => {
		sent.push(ctx);
		const s = createAssistantMessageEventStream();
		const done = { role: "assistant", content: [{ type: "text", text: "ok" }], api: model.api, provider: model.provider, model: model.id, stopReason: "stop" };
		s.push({ type: "done", reason: "stop", message: done });
		s.end();
		return s;
	};
	return { sent, inner };
}

const collect = async (stream) => {
	const events = [];
	for await (const e of stream) events.push(e);
	return events;
};

test("a Local Studio model with an 8192 window gets the whole history, unchanged", async () => {
	resetLearnedWindows?.();
	const { sent, inner } = recording();
	const model = { api: "eukhe-session", provider: LOCAL_STUDIO_PROVIDER, id: "llama-small", contextWindow: 8192, maxTokens: 2048 };
	const events = await collect(await guardContext(inner)(model, context, {}));
	assert.equal(sent.length, 1);
	assert.equal(sent[0], context);
	assert.equal(events.at(-1).type, "done");
});

test("another provider's 8192 window is still guarded", async () => {
	resetLearnedWindows?.();
	const { sent, inner } = recording();
	const model = { api: "openai-completions", provider: "cerebras", id: "qwen", contextWindow: 8192, maxTokens: 2048 };
	const events = await collect(await guardContext(inner)(model, context, {}));
	assert.equal(sent.length, 0);
	assert.equal(events.at(-1).type, "error");
});
