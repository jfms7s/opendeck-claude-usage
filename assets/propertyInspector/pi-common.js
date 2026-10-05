// Shared Property Inspector plumbing for every action's PI: the OpenAction
// socket handshake, reading and saving settings, and the press-picked
// fields a save must pass through untouched. Each page keeps only its own
// form: `connectPI({ apply, read, passThrough })` does the rest.
//
// Plain functions over the DOM (no framework), so tests/pi/ can run the
// same code under `node --test`.
(function (root) {
	"use strict";

	function isHexColor(value) {
		return typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value);
	}

	// Shows `value` in `select` if it's one of its options, otherwise
	// `fallback`. A stored value the select doesn't know (a downgrade, a
	// future option) would leave it blank, and the next save would send ""
	// (KI-10).
	function selectKnown(select, value, fallback) {
		const known = Array.from(select.options).some((option) => option.value === value);
		select.value = known ? value : fallback;
		return select.value;
	}

	// How a PI saves. `read()` returns the form's settings; `passThrough`
	// names fields the PI never edits because only a key press changes
	// them (the gauge style, the heatmap view, ...).
	//
	// OpenDeck may not forward the plugin's own setSettings (from a press)
	// to an open PI, so a pass-through value held here can be stale and
	// saving it would undo the press (KI-14). With pass-through fields, a
	// save therefore first asks for the stored settings and saves once
	// they arrive - or after `replyTimeoutMs` with what it has.
	function createSettingsChannel({
		send,
		read,
		passThrough = [],
		replyTimeoutMs = 500,
		setTimer = setTimeout,
		clearTimer = clearTimeout,
	}) {
		const stored = {};
		let pending = null;

		function remember(settings) {
			for (const key of passThrough) {
				stored[key] = settings[key];
			}
		}

		function save() {
			clearTimer(pending);
			pending = null;
			const payload = read();
			for (const key of passThrough) {
				if (stored[key] !== undefined) {
					payload[key] = stored[key];
				}
			}
			send({ event: "setSettings", payload });
		}

		return {
			// Settings from OpenDeck. Returns true when the page should show
			// them; false when they were the reply to our own getSettings,
			// whose only use is refreshing the pass-through values - the
			// edit being saved is kept.
			received(settings) {
				remember(settings || {});
				if (pending) {
					save();
					return false;
				}
				return true;
			},
			// The user changed something.
			changed() {
				if (passThrough.length === 0) {
					save();
					return;
				}
				if (pending) return; // the pending save reads the form when it sends
				pending = setTimer(save, replyTimeoutMs);
				send({ event: "getSettings" });
			},
		};
	}

	// Connects a PI page to OpenDeck: registers, shows the stored settings
	// with `apply`, and returns `changed` for the page's inputs to call.
	function connectPI({ apply, read, passThrough = [] }) {
		let websocket;
		let uuid;
		const channel = createSettingsChannel({
			read,
			passThrough,
			send: (message) => websocket.send(JSON.stringify({ ...message, context: uuid })),
		});
		const show = (settings) => {
			if (channel.received(settings)) apply(settings || {});
		};
		const connected = new Promise((resolve) => {
			root.connectOpenActionSocket = (...args) => resolve(args);
			root.connectElgatoStreamDeckSocket = root.connectOpenActionSocket;
		});
		connected.then(([inPort, inUUID, inRegisterEvent, , inActionInfo]) => {
			uuid = inUUID;
			const actionInfo = JSON.parse(inActionInfo);
			websocket = new WebSocket(`ws://127.0.0.1:${inPort}`);
			websocket.onopen = () => {
				websocket.send(JSON.stringify({ event: inRegisterEvent, uuid: inUUID }));
				show(actionInfo.payload.settings || {});
			};
			websocket.onmessage = (event) => {
				const message = JSON.parse(event.data);
				if (message.event === "didReceiveSettings") {
					show(message.payload.settings || {});
				}
			};
		});
		return { changed: () => channel.changed() };
	}

	const api = { isHexColor, selectKnown, createSettingsChannel, connectPI };
	if (typeof module !== "undefined" && module.exports) {
		module.exports = api;
	} else {
		Object.assign(root, api);
	}
})(typeof window !== "undefined" ? window : globalThis);
