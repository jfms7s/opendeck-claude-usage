// Just enough DOM to run a Property Inspector page under node: its
// inputs/selects (with the browser's value-sanitizing rules that the PI
// bugs were about), its scripts, and a fake OpenDeck socket.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import vm from "node:vm";

export const PI_DIR = new URL("../../assets/propertyInspector/", import.meta.url).pathname;

function attrs(tag) {
	const out = {};
	for (const [, name, value] of tag.matchAll(/([a-zA-Z-]+)="([^"]*)"/g)) {
		out[name] = value;
	}
	return out;
}

class Element {
	constructor(tagName, attributes, options = []) {
		this.tagName = tagName;
		this.id = attributes.id;
		this.type = attributes.type;
		this.options = options.map((value) => ({ value }));
		this.checked = false;
		this.listeners = {};
		this.classes = new Set((attributes.class || "").split(/\s+/).filter(Boolean));
		this._value = attributes.value ?? (tagName === "select" ? (options[0] ?? "") : "");
		const element = this;
		this.classList = {
			toggle(name, force) {
				const on = force ?? !element.classes.has(name);
				if (on) element.classes.add(name);
				else element.classes.delete(name);
				return on;
			},
			contains: (name) => element.classes.has(name),
		};
	}

	// What browsers do with a value an input can't hold - the source of
	// KI-10 (blank select) and KI-11 (black color).
	set value(v) {
		const text = String(v ?? "");
		if (this.tagName === "select") {
			this._value = this.options.some((o) => o.value === text) ? text : "";
		} else if (this.type === "color") {
			this._value = /^#[0-9a-f]{6}$/i.test(text) ? text.toLowerCase() : "#000000";
		} else if (this.type === "time") {
			this._value = /^\d{2}:\d{2}$/.test(text) ? text : "";
		} else {
			this._value = text;
		}
	}

	get value() {
		return this._value;
	}

	addEventListener(event, listener) {
		(this.listeners[event] ||= []).push(listener);
	}

	dispatch(event) {
		for (const listener of this.listeners[event] || []) listener({ target: this });
	}
}

class Page {
	constructor() {
		this.byId = new Map();
		this.groups = new Map();
		this.sent = [];
	}

	// Registers every element with an id in `html`, and the unlabelled
	// checkboxes inside each container with an id.
	parse(html) {
		for (const match of html.matchAll(/<select\b([^>]*)>([\s\S]*?)<\/select>/g)) {
			const a = attrs(match[1]);
			const options = [...match[2].matchAll(/<option\b[^>]*value="([^"]*)"/g)].map((m) => m[1]);
			this.byId.set(a.id, new Element("select", a, options));
		}
		for (const match of html.matchAll(/<(input|p|button|div|section)\b([^>]*)>/g)) {
			const a = attrs(match[2]);
			if (a.id && !this.byId.has(a.id)) this.byId.set(a.id, new Element(match[1], a));
		}
		for (const match of html.matchAll(/id="([^"]+)"[^>]*>([\s\S]*?)<\/div>/g)) {
			const boxes = [...match[2].matchAll(/<input type="checkbox" value="([^"]*)"/g)].map(
				(m) => new Element("input", { type: "checkbox", value: m[1] }),
			);
			if (boxes.length) this.groups.set(match[1], boxes);
		}
	}

	el(id) {
		const element = this.byId.get(id);
		if (!element) throw new Error(`no element #${id}`);
		return element;
	}

	// Sets an input as the user would, and fires its change event.
	change(id, value) {
		const element = this.el(id);
		element.value = value;
		element.dispatch("input");
		element.dispatch("change");
	}

	saves() {
		return this.sent.filter((m) => m.event === "setSettings").map((m) => m.payload);
	}
}

// Loads `file` from assets/propertyInspector and runs its scripts.
export function loadPage(file) {
	const page = new Page();
	const html = readFileSync(join(PI_DIR, file), "utf8");
	page.parse(html);

	const document = {
		getElementById: (id) => page.byId.get(id) ?? null,
		querySelectorAll(selector) {
			const group = selector.match(/^#(\S+) input$/);
			return group ? (page.groups.get(group[1]) ?? []) : [];
		},
	};
	const timers = [];
	class FakeWebSocket {
		constructor(url) {
			this.url = url;
			page.socket = this;
		}
		send(text) {
			page.sent.push(JSON.parse(text));
		}
	}
	const context = vm.createContext({
		document,
		WebSocket: FakeWebSocket,
		setTimeout: (fn) => timers.push(fn),
		clearTimeout: () => {},
		console,
	});
	context.window = context;
	page.runTimers = () => timers.splice(0).forEach((fn) => fn());

	// A container's innerHTML (colors.js's cards) adds its inputs to the page.
	for (const element of page.byId.values()) {
		Object.defineProperty(element, "innerHTML", {
			set(markup) {
				page.parse(markup);
			},
		});
		element.querySelector = (selector) => page.el(selector.replace(/^#/, ""));
	}

	for (const match of html.matchAll(/<script(?: src="([^"]+)")?>([\s\S]*?)<\/script>/g)) {
		const code = match[1] ? readFileSync(join(PI_DIR, match[1]), "utf8") : match[2];
		vm.runInContext(code, context, { filename: match[1] || `${file} (inline)` });
	}

	// OpenDeck opening the PI with these stored settings.
	page.open = async (settings) => {
		context.connectOpenActionSocket(
			"1234",
			"ctx1",
			"registerPropertyInspector",
			"{}",
			JSON.stringify({ payload: { settings } }),
		);
		await new Promise((resolve) => setImmediate(resolve));
		page.socket.onopen();
	};
	// OpenDeck sending didReceiveSettings.
	page.receive = (settings) => {
		page.socket.onmessage({ data: JSON.stringify({ event: "didReceiveSettings", payload: { settings } }) });
	};
	return page;
}
