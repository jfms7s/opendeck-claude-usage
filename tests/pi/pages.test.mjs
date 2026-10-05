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

test("gauge: no stored cycle list shows every style ticked", async () => {
	const page = loadPage("index.html");
	await page.open({});
	page.change("window", "weekly");
	page.runTimers(); // no reply to getSettings
	const [saved] = page.saves();
	assert.equal(saved.window, "weekly");
	assert.equal(saved.cycleStyles.length, 6);
	assert.ok(!("style" in saved));
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

test("sparkline: a stored Monthly is kept and the series passes through", async () => {
	const page = loadPage("sparkline.html");
	await page.open({ window: "monthly", series: "today" });
	assert.equal(page.el("window").value, "monthly");
	page.change("watch", "40");
	page.runTimers();
	assert.deepEqual(page.saves(), [{ window: "monthly", watch: 40, series: "today" }]);
});

test("heatmap: a bad stored color shows the default, the view passes through", async () => {
	const page = loadPage("heatmap.html");
	await page.open({ metric: "bogus", color: "red", view: "fourWeeks" });
	assert.equal(page.el("metric").value, "tokens");
	assert.equal(page.el("color").value, "#d97757");
	page.change("metric", "cost");
	page.runTimers();
	assert.deepEqual(page.saves(), [{ metric: "cost", color: "#d97757", view: "fourWeeks" }]);
});

test("metric tile: unknown metric and range fall back, refresh is clamped", async () => {
	const page = loadPage("metrictile.html");
	await page.open({ metric: "bogus", range: "yearly", refresh_seconds: "x" });
	assert.equal(page.el("metric").value, "tokens");
	assert.equal(page.el("range").value, "today");
	assert.equal(page.el("refresh_seconds").value, "60");
	page.change("refresh_seconds", "1");
	assert.deepEqual(page.saves(), [{ metric: "tokens", range: "today", refresh_seconds: 5, source: "logs" }]);
});

test("peak clock: an empty day list stays empty; bad times fall back", async () => {
	const page = loadPage("peakclock.html");
	await page.open({ peak_days: [], peak_start: 7 });
	assert.equal(page.el("peak_start").value, "13:00");
	page.change("peak_end", "19:30");
	assert.deepEqual(page.saves(), [{ peak_start: "13:00", peak_end: "19:30", peak_days: [] }]);
});

test("metric tile: a saved Console + Session tile still shows Session (it draws 5H N/A)", async () => {
	const page = loadPage("metrictile.html");
	await page.open({ source: "console", range: "session" });
	assert.equal(page.el("source").value, "console");
	assert.equal(page.el("range").value, "session");
	assert.equal(page.el("range").options.find((o) => o.value === "session").disabled, true);
	assert.equal(page.el("consoleHint").classList.contains("hidden"), false);
	assert.deepEqual(page.saves(), [], "opening the PI saves nothing");
});

test("metric tile: switching to Console moves Session to Today and saves it", async () => {
	const page = loadPage("metrictile.html");
	await page.open({ range: "session", source: "bogus" });
	assert.equal(page.el("source").value, "logs");
	assert.equal(page.el("consoleHint").classList.contains("hidden"), true);
	page.change("source", "console");
	assert.deepEqual(page.saves(), [{ metric: "tokens", range: "today", refresh_seconds: 60, source: "console" }]);
});

test("api spend: the press-picked range passes through; a junk budget shows blank (KI-14, KI-10)", async () => {
	const page = loadPage("apispend.html");
	await page.open({ range: "today", budgetDollars: "abc", critical: 95 });
	assert.equal(page.el("budgetDollars").value, "");
	page.change("budgetDollars", "50");
	page.receive({ range: "sevenday" }); // reply to getSettings
	assert.deepEqual(page.saves(), [{ budgetDollars: 50, critical: 95, range: "sevenday" }]);
});

test("api spend: clearing the budget saves no budget", async () => {
	const page = loadPage("apispend.html");
	await page.open({ budgetDollars: 40 });
	assert.equal(page.el("budgetDollars").value, "40");
	page.change("budgetDollars", "");
	page.runTimers(); // no reply to getSettings
	assert.deepEqual(page.saves(), [{}]);
});
