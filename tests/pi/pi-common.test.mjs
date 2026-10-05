// Runs the shared PI plumbing (assets/propertyInspector/pi-common.js).
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { PI_DIR } from "./fake-dom.mjs";

const require = createRequire(import.meta.url);
const { selectKnown, createSettingsChannel, isHexColor } = require(`${PI_DIR}pi-common.js`);

function select(values) {
	const options = values.map((value) => ({ value }));
	return {
		options,
		_value: "",
		set value(v) {
			this._value = options.some((o) => o.value === v) ? v : "";
		},
		get value() {
			return this._value;
		},
	};
}

test("selectKnown shows a known value and falls back for an unknown one (KI-10)", () => {
	const window = select(["session", "weekly", "monthly"]);
	assert.equal(selectKnown(window, "weekly", "session"), "weekly");
	assert.equal(selectKnown(window, "hourly", "session"), "session");
	assert.equal(selectKnown(window, undefined, "session"), "session");
	assert.equal(selectKnown(window, "", "session"), "session");
});

test("isHexColor accepts #rrggbb only", () => {
	assert.ok(isHexColor("#a1B2c3"));
	for (const bad of ["red", "#abc", "#12345g", "", null, 7]) assert.ok(!isHexColor(bad), String(bad));
});

function channel(passThrough, form = { a: 1 }) {
	const sent = [];
	const timers = [];
	const ch = createSettingsChannel({
		send: (m) => sent.push(m),
		read: () => ({ ...form }),
		passThrough,
		setTimer: (fn) => (timers.push(fn), timers.length),
		clearTimer: () => {},
	});
	return { ch, sent, fire: () => timers.splice(0).forEach((fn) => fn()) };
}

test("without pass-through fields a change saves straight away", () => {
	const { ch, sent } = channel([]);
	ch.changed();
	assert.deepEqual(sent, [{ event: "setSettings", payload: { a: 1 } }]);
});

test("a save re-reads the stored pass-through value first (KI-14)", () => {
	const { ch, sent } = channel(["style"]);
	assert.equal(ch.received({ style: "bar" }), true); // shown on open
	ch.changed();
	ch.changed(); // a second edit while waiting adds no request
	assert.deepEqual(sent, [{ event: "getSettings" }]);
	// A press changed the style meanwhile; OpenDeck replies with it.
	assert.equal(ch.received({ style: "thinRing", a: 99 }), false, "the reply isn't shown over the edit");
	assert.deepEqual(sent[1], { event: "setSettings", payload: { a: 1, style: "thinRing" } });
});

test("with no reply, the save goes out with the value it has", () => {
	const { ch, sent, fire } = channel(["view"]);
	ch.received({ view: "fourWeeks" });
	ch.changed();
	fire();
	assert.deepEqual(sent[1], { event: "setSettings", payload: { a: 1, view: "fourWeeks" } });
});

test("a pass-through value never stored isn't invented", () => {
	const { ch, sent, fire } = channel(["series"]);
	ch.received({});
	ch.changed();
	fire();
	assert.deepEqual(sent[1], { event: "setSettings", payload: { a: 1 } });
});
