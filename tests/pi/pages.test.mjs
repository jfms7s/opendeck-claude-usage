// Runs each Property Inspector page end to end against a stub DOM and a
// fake OpenDeck: what it shows for stored settings, and what it saves.
import { test } from "node:test";
import assert from "node:assert/strict";
import { loadPage } from "./fake-dom.mjs";

test("gauge: an unknown stored window shows (and saves) Session, not \"\" (KI-10)", async () => {
	const page = loadPage("index.html");
	await page.open({ window: "hourly", colorNormal: "#123456", style: "bar" });
	assert.equal(page.el("window").value, "session");
	page.change("colorNormal", "#654321");
	page.receive({ window: "hourly", style: "thinRing" }); // reply to getSettings
	const [saved] = page.saves();
	assert.equal(saved.window, "session");
	assert.equal(saved.colorNormal, "#654321");
	assert.equal(saved.style, "thinRing", "the press-picked style passes through (KI-14)");
});

test("gauge: saving keeps a press-picked style and drops a stored cycleStyles", async () => {
	const page = loadPage("index.html");
	await page.open({ window: "session", style: "bar", cycleStyles: ["bar"] });
	page.change("window", "weekly");
	page.receive({ window: "session", style: "softPill", cycleStyles: ["bar"] }); // reply to getSettings
	const [saved] = page.saves();
	assert.equal(saved.window, "weekly");
	assert.equal(saved.style, "softPill", "the press-picked style passes through (KI-14)");
	assert.ok(!("cycleStyles" in saved));
});

test("burn rate: unknown window and metric fall back (KI-10)", async () => {
	const page = loadPage("burnrate.html");
	await page.open({ window: "monthly", metric: "", critical: 95 });
	assert.equal(page.el("window").value, "session");
	assert.equal(page.el("metric").value, "pace");
	page.change("metric", "runway");
	assert.deepEqual(page.saves(), [{ window: "session", metric: "runway", critical: 95 }]);
});

test("combo: saving keeps the press-picked layout (KI-14)", async () => {
	const page = loadPage("combo.html");
	await page.open({ layout: "vertical" });
	page.change("risk", "70");
	page.runTimers();
	assert.deepEqual(page.saves(), [{ risk: 70, layout: "vertical" }]);
});

test("sparkline: a stored Monthly (cut) shows Session and the series passes through", async () => {
	const page = loadPage("sparkline.html");
	await page.open({ window: "monthly", series: "today" });
	assert.equal(page.el("window").value, "session");
	page.change("watch", "40");
	page.runTimers();
	assert.deepEqual(page.saves(), [{ window: "session", watch: 40, alerts: false, series: "today" }]);
});

test("gauge: a stored Monthly (cut) shows Session", async () => {
	const page = loadPage("index.html");
	await page.open({ window: "monthly" });
	assert.equal(page.el("window").value, "session");
});

test("heatmap: a bad stored color shows the default, the view passes through", async () => {
	const page = loadPage("heatmap.html");
	await page.open({ metric: "bogus", color: "red", view: "fourWeeks" });
	assert.equal(page.el("metric").value, "tokens");
	assert.equal(page.el("color").value, "#d97757");
	page.change("metric", "cost");
	page.runTimers();
	assert.deepEqual(page.saves(), [{ metric: "cost", scope: "all", color: "#d97757", view: "fourWeeks" }]);
});

test("metric tile: unknown metric and range fall back; no refresh field, a stored one isn't saved", async () => {
	const page = loadPage("metrictile.html");
	await page.open({ metric: "bogus", range: "yearly", refresh_seconds: 30 });
	assert.equal(page.el("metric").value, "tokens");
	assert.equal(page.el("range").value, "today");
	assert.throws(() => page.el("refresh_seconds"), /no element #refresh_seconds/);
	page.change("range", "sevenday");
	assert.deepEqual(page.saves(), [{ metric: "tokens", range: "sevenday", scope: "all" }]);
});

test("metric tile: no Source picker; a saved Console tile opens on its range and saves no source", async () => {
	const page = loadPage("metrictile.html");
	await page.open({ source: "console", metric: "cost", range: "session" });
	assert.throws(() => page.el("source"), /no element #source/);
	assert.equal(page.el("metric").value, "cost");
	assert.equal(page.el("range").value, "session");
	page.change("metric", "tokens");
	assert.deepEqual(page.saves(), [{ metric: "tokens", range: "session", scope: "all" }]);
});

test("scope: a saved family is shown and kept; an unknown one falls back to All", async () => {
	for (const file of ["metrictile.html", "heatmap.html"]) {
		const kept = loadPage(file);
		await kept.open({ metric: "tokens", scope: "opus" });
		assert.equal(kept.el("scope").value, "opus", file);
		kept.change("metric", "cost");
		kept.runTimers();
		assert.equal(kept.saves()[0].scope, "opus", file);

		const unknown = loadPage(file);
		await unknown.open({ scope: "gpt" });
		assert.equal(unknown.el("scope").value, "all", file);
		unknown.change("scope", "haiku");
		unknown.runTimers();
		assert.equal(unknown.saves()[0].scope, "haiku", file);
	}
});

test("peak clock: an empty day list stays empty; bad times fall back", async () => {
	const page = loadPage("peakclock.html");
	await page.open({ peak_days: [], peak_start: 7 });
	assert.equal(page.el("peak_start").value, "13:00");
	page.change("peak_end", "19:30");
	assert.deepEqual(page.saves(), [{ peak_start: "13:00", peak_end: "19:30", peak_days: [] }]);
});

for (const file of ["metrictile.html", "heatmap.html"]) {
	test(`${file}: asks for the project list and fills the scope from it`, async () => {
		const page = loadPage(file);
		await page.open({ scope: "opus" });
		assert.deepEqual(
			page.sent.filter((m) => m.event === "sendToPlugin").map((m) => m.payload),
			[{ request: "projects" }],
		);
		page.pluginMessage({
			projects: [
				{ id: "-home-u-git-repo", name: "repo" },
				{ id: "-home-u-git-other", name: "<b>other</b>" },
			],
		});
		const scope = page.el("scope");
		assert.deepEqual(
			scope.options.map((o) => o.value),
			["all", "opus", "sonnet", "haiku", "project:-home-u-git-repo", "project:-home-u-git-other"],
		);
		assert.equal(scope.options[5].textContent, "Project: <b>other</b>", "names are text");
		assert.equal(scope.value, "opus", "the shown scope is kept");
		page.change("scope", "project:-home-u-git-repo");
		page.runTimers();
		assert.equal(page.saves().at(-1).scope, "project:-home-u-git-repo");
		// A second list replaces the first.
		page.pluginMessage({ projects: [{ id: "-home-u-git-other", name: "other" }] });
		assert.deepEqual(
			scope.options.map((o) => o.value).filter((v) => v.startsWith("project:")),
			["project:-home-u-git-other", "project:-home-u-git-repo"],
			"the selected project stays even when the list drops it",
		);
		assert.equal(scope.value, "project:-home-u-git-repo");
	});

	test(`${file}: a saved project is kept before (and without) the list`, async () => {
		const page = loadPage(file);
		await page.open({ scope: "project:-home-u-git-gone" });
		const scope = page.el("scope");
		assert.equal(scope.value, "project:-home-u-git-gone");
		assert.equal(scope.options.at(-1).textContent, "Project: -home-u-git-gone");
		page.pluginMessage({ projects: [{ id: "-home-u-git-repo", name: "repo" }] });
		assert.equal(scope.value, "project:-home-u-git-gone");
		page.pluginMessage({ nonsense: true });
		page.change("metric", "cost");
		page.runTimers();
		assert.equal(page.saves().at(-1).scope, "project:-home-u-git-gone");
	});
}

for (const file of ["index.html", "sparkline.html"]) {
	test(`${file}: the alerts box shows and saves the stored choice`, async () => {
		const page = loadPage(file);
		await page.open({ window: "weekly", alerts: true });
		assert.equal(page.el("alerts").checked, true);
		page.el("alerts").checked = false;
		page.el("alerts").dispatch("change");
		page.receive({ window: "weekly", alerts: true }); // reply to getSettings
		assert.equal(page.saves().at(-1).alerts, false);

		const garbage = loadPage(file);
		await garbage.open({ alerts: "yes" });
		assert.equal(garbage.el("alerts").checked, false, "only a stored true ticks it");
	});
}
