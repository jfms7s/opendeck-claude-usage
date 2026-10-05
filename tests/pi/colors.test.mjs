// Runs colors.js (the shared Colors & thresholds cards) against a stub DOM.
import { test, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { PI_DIR, loadPage } from "./fake-dom.mjs";

const require = createRequire(import.meta.url);
globalThis.isHexColor = require(`${PI_DIR}pi-common.js`).isHexColor;
const colors = require(`${PI_DIR}colors.js`);
const cases = JSON.parse(readFileSync(new URL("marks-cases.json", import.meta.url), "utf8")).cases;

// colors.js reads the page through the global `document`: give it a combo
// page (just the colors section) and point the global at it.
let page;
beforeEach(() => {
	page = loadPage("combo.html");
	globalThis.document = { getElementById: (id) => page.el(id) };
	colors.touchedColorFields.clear();
});

test("a stored value a field can't show falls back to the default (KI-11)", () => {
	assert.equal(colors.validColorValue("colorNormal", "red"), "#d97757");
	assert.equal(colors.validColorValue("colorNormal", "#AABBCC"), "#aabbcc");
	assert.equal(colors.validColorValue("watch", "40"), 40);
	assert.equal(colors.validColorValue("watch", ""), 50);
	assert.equal(colors.validColorValue("watch", null), 50);
	assert.equal(colors.validColorValue("colorMode", "bogus"), "fixed");
	colors.applyColorSettings({ colorWatch: "yellow" });
	assert.equal(page.el("colorWatch").value, "#eab308", "not the #000000 a color input falls back to");
});

test("saving leaves untouched defaults out (KI-12)", () => {
	colors.applyColorSettings({ risk: 70 });
	assert.deepEqual(colors.readColorSettings(), { risk: 70 });
	// A field the user touched is saved even when set back to its default.
	page.el("watch").value = "50";
	colors.touchedColorFields.add("watch");
	assert.deepEqual(colors.readColorSettings(), { watch: 50, risk: 70 });
});

test("a cleared number field saves its default, not \"\"", () => {
	colors.applyColorSettings({});
	page.el("critical").value = "";
	colors.touchedColorFields.add("critical");
	assert.deepEqual(colors.readColorSettings(), { critical: 90 });
});

test("the marks warning matches what the plugin does with the marks (KI-13)", () => {
	for (const { marks, used } of cases) {
		const [watch, risk, critical] = marks;
		colors.applyColorSettings({ watch, risk, critical });
		const shown = !page.el("marksWarning").classList.contains("hidden");
		assert.equal(shown, !used, `marks ${marks}`);
	}
});
