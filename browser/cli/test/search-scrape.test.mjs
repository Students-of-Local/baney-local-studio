import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, it } from "node:test";
import { assertSelfHostedFirecrawlUrl, firecrawlCrawl, firecrawlMap, pageMarkdown, requireFirecrawlApiUrl } from "../src/firecrawl.ts";
import { requireSearxngUrl, searxSearch, searxngUrl } from "../src/searx.ts";

const saved = { ...process.env };

function restoreEnv() {
	for (const k of ["SEARXNG_URL", "FIRECRAWL_API_URL", "FIRECRAWL_API_KEY", "LOCAL_STUDIO_BROWSER_DIR"]) {
		if (Object.hasOwn(saved, k)) process.env[k] = saved[k];
		else delete process.env[k];
	}
}

beforeEach(() => {
	delete process.env.SEARXNG_URL;
	delete process.env.FIRECRAWL_API_URL;
	delete process.env.FIRECRAWL_API_KEY;
	process.env.LOCAL_STUDIO_BROWSER_DIR = mkdtempSync(join(tmpdir(), "tk-search-"));
});
afterEach(restoreEnv);

describe("searx", () => {
	it("requires SEARXNG_URL or config", () => {
		assert.equal(searxngUrl(), "");
		assert.throws(() => requireSearxngUrl(), /SEARXNG_URL/);
	});

	it("reads searxngUrl from search.json", () => {
		writeFileSync(join(process.env.LOCAL_STUDIO_BROWSER_DIR, "search.json"), JSON.stringify({ searxngUrl: "http://127.0.0.1:8888/" }));
		assert.equal(requireSearxngUrl(), "http://127.0.0.1:8888");
	});

	it("queries the JSON API", async () => {
		let agent;
		const srv = createServer((req, res) => {
			agent = req.headers["user-agent"];
			assert.match(req.url, /\/search\?/);
			assert.match(req.url, /format=json/);
			res.writeHead(200, { "content-type": "application/json" });
			res.end(JSON.stringify({ results: [{ title: "One", url: "https://example.com/a", content: "hi", engine: "test" }] }));
		});
		await new Promise((r) => srv.listen(0, "127.0.0.1", r));
		const { port } = srv.address();
		process.env.SEARXNG_URL = `http://127.0.0.1:${port}`;
		const out = await searxSearch("local-studio-browser");
		srv.close();
		assert.match(agent, /^local-studio-browser\/\d/, "the helper names itself local-studio-browser");
		assert.equal(out.query, "local-studio-browser");
		assert.equal(out.results.length, 1);
		assert.equal(out.results[0].url, "https://example.com/a");
	});
});

describe("firecrawl self-host only", () => {
	it("refuses api.firecrawl.dev and cousins", () => {
		for (const bad of [
			"https://api.firecrawl.dev",
			"https://api.firecrawl.dev/v2",
			"http://api.firecrawl.dev",
			"https://API.Firecrawl.dev",
			"https://foo.api.firecrawl.dev",
			"https://api.firecrawl.dev.",
			"https://api.firecrawl.dev./v2",
		]) {
			assert.throws(() => assertSelfHostedFirecrawlUrl(bad), /refuses hosted Firecrawl/, bad);
		}
	});

	it("accepts loopback and private URLs", () => {
		assert.equal(assertSelfHostedFirecrawlUrl("http://127.0.0.1:3002/"), "http://127.0.0.1:3002");
		assert.equal(assertSelfHostedFirecrawlUrl("http://100.64.0.1:3002"), "http://100.64.0.1:3002");
	});

	it("requireFirecrawlApiUrl refuses when unset", () => {
		assert.throws(() => requireFirecrawlApiUrl(), /FIRECRAWL_API_URL/);
	});

	it("page_markdown posts /v2/scrape", async () => {
		const srv = createServer((req, res) => {
			assert.equal(req.method, "POST");
			assert.equal(req.url, "/v2/scrape");
			let body = "";
			req.on("data", (c) => (body += c));
			req.on("end", () => {
				const j = JSON.parse(body);
				assert.equal(j.url, "https://example.com/");
				assert.deepEqual(j.formats, ["markdown"]);
				res.writeHead(200, { "content-type": "application/json" });
				res.end(
					JSON.stringify({
						success: true,
						data: { markdown: "# Hi", metadata: { title: "Hi", sourceURL: "https://example.com/" } },
					}),
				);
			});
		});
		await new Promise((r) => srv.listen(0, "127.0.0.1", r));
		const { port } = srv.address();
		process.env.FIRECRAWL_API_URL = `http://127.0.0.1:${port}`;
		const out = await pageMarkdown("https://example.com/");
		srv.close();
		assert.equal(out.markdown, "# Hi");
		assert.equal(out.title, "Hi");
	});

	it("page_markdown does not follow redirects (hosted bypass)", async () => {
		let destHits = 0;
		const dest = createServer((_req, res) => {
			destHits += 1;
			res.writeHead(200, { "content-type": "application/json" });
			res.end(
				JSON.stringify({
					success: true,
					data: { markdown: "# leaked", metadata: { title: "leaked", sourceURL: "https://example.com/" } },
				}),
			);
		});
		await new Promise((r) => dest.listen(0, "127.0.0.1", r));
		const destPort = dest.address().port;
		const srv = createServer((_req, res) => {
			res.writeHead(302, { Location: `http://127.0.0.1:${destPort}/v2/scrape` });
			res.end();
		});
		await new Promise((r) => srv.listen(0, "127.0.0.1", r));
		const { port } = srv.address();
		process.env.FIRECRAWL_API_URL = `http://127.0.0.1:${port}`;
		try {
			await assert.rejects(
				() => pageMarkdown("https://example.com/"),
				(err) => {
					const msg = `${err}\n${err?.cause ?? ""}`;
					assert.match(msg, /unexpected redirect/i);
					return true;
				},
			);
			assert.equal(destHits, 0, "must not follow the redirect to the destination");
		} finally {
			delete process.env.FIRECRAWL_API_URL;
			await new Promise((r) => srv.close(r));
			await new Promise((r) => dest.close(r));
		}
	});

	it("map and crawl hit their paths, and crawl waits for its job's pages", async () => {
		const seen = [];
		let asked = 0;
		const page = (u) => ({ url: u, markdown: `# ${u}` });
		const srv = createServer((req, res) => {
			seen.push(`${req.method} ${req.url}`);
			res.writeHead(200, { "content-type": "application/json" });
			const { port } = srv.address();
			const job = (status, body = {}) => res.end(JSON.stringify({ status, ...body }));
			if (req.url === "/v2/map") return res.end(JSON.stringify({ ok: true }));
			if (req.url === "/v2/crawl") return res.end(JSON.stringify({ success: true, id: `job-${seen.filter((s) => s === "POST /v2/crawl").length}` }));
			// job-1 is scraping once, then done with a second part at next; job-2 fails; job-3 never ends; job-4's next is another server.
			if (req.url === "/v2/crawl/job-1") return asked++ ? job("completed", { data: [page("a")], next: `http://127.0.0.1:${port}/v2/crawl/job-1?skip=1` }) : job("scraping", { data: [] });
			if (req.url === "/v2/crawl/job-1?skip=1") return job("completed", { data: [page("b")], next: null });
			if (req.url === "/v2/crawl/job-2") return job("failed", { error: "robots.txt" });
			if (req.url === "/v2/crawl/job-3") return job("scraping", { data: [page("c")] });
			if (req.url === "/v2/crawl/job-4") return job("completed", { data: [page("d")], next: "http://127.0.0.2:1/v2/crawl/job-4?skip=1" });
			res.end("{}");
		});
		await new Promise((r) => srv.listen(0, "127.0.0.1", r));
		const { port } = srv.address();
		process.env.FIRECRAWL_API_URL = `http://127.0.0.1:${port}`;
		try {
			await firecrawlMap("https://example.com");
			const done = await firecrawlCrawl("https://example.com", { limit: 3, pollMs: 10 });
			assert.deepEqual(seen, ["POST /v2/map", "POST /v2/crawl", "GET /v2/crawl/job-1", "GET /v2/crawl/job-1", "GET /v2/crawl/job-1?skip=1"]);
			assert.equal(done.status, "completed");
			assert.deepEqual(done.data.map((p) => p.url), ["a", "b"]);
			assert.equal(done.next, undefined);
			assert.equal(done.unfinished, undefined);
			await assert.rejects(() => firecrawlCrawl("https://example.com", { pollMs: 10 }), /Firecrawl's crawl failed: robots\.txt/);
			const slow = await firecrawlCrawl("https://example.com", { pollMs: 10, waitMs: 50 });
			assert.deepEqual(slow.data.map((p) => p.url), ["c"]);
			assert.match(slow.unfinished, /still running after 0 seconds/);
			const away = await firecrawlCrawl("https://example.com", { pollMs: 10 });
			assert.deepEqual(away.data.map((p) => p.url), ["d"], "a next on another server is not followed");
		} finally {
			delete process.env.FIRECRAWL_API_URL;
			srv.close();
		}
	});
});
