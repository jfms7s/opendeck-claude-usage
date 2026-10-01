// Shared "Colors & thresholds" cards for the Usage Gauge and Burn Rate
// property inspectors. Field names and defaults mirror src/level.rs's wire
// format (a Rust test checks the defaults stay in sync).
const COLOR_DEFAULTS = {
	watch: 50,
	risk: 75,
	critical: 90,
	colorNormal: "#d97757",
	colorWatch: "#eab308",
	colorRisk: "#f97316",
	colorCritical: "#ef4444",
	colorMode: "fixed",
};

function mountColorSection(container, { showMode, onChange }) {
	// Two pi.css cards; the page's container uses display: contents so they
	// sit in the page grid next to the page's own cards.
	container.innerHTML = `
		<section class="card">
			<h2>Thresholds</h2>
			<div class="row">
				<div class="field">
					<label for="watch">Watch</label>
					<input type="number" id="watch" min="0" max="100" step="1" />
				</div>
				<div class="field">
					<label for="risk">Risk</label>
					<input type="number" id="risk" min="0" max="100" step="1" />
				</div>
				<div class="field">
					<label for="critical">Critical</label>
					<input type="number" id="critical" min="0" max="100" step="1" />
				</div>
			</div>
			<p class="hint">% used. Marks must increase (Watch &lt; Risk &lt; Critical), otherwise defaults are used.</p>
			<p class="status bad hidden" id="marksWarning">These marks aren't increasing, so the key is using the defaults (50 / 75 / 90).</p>
		</section>
		<section class="card">
			<h2>Colors</h2>
			<div class="row">
				<div class="field">
					<label for="colorNormal">Normal</label>
					<input type="color" id="colorNormal" />
				</div>
				<div class="field">
					<label for="colorWatch">Watch</label>
					<input type="color" id="colorWatch" />
				</div>
				<div class="field">
					<label for="colorRisk">Risk</label>
					<input type="color" id="colorRisk" />
				</div>
				<div class="field">
					<label for="colorCritical">Critical</label>
					<input type="color" id="colorCritical" />
				</div>
			</div>
			<div class="field" id="colorModeRow">
				<label for="colorMode">Color by</label>
				<select id="colorMode">
					<option value="fixed">Current usage</option>
					<option value="pace">Pace (also warn when burning too fast)</option>
				</select>
			</div>
			<div class="row">
				<button type="button" id="colorReset">Reset to defaults</button>
			</div>
		</section>`;
	// .field sets display, which would override the hidden attribute.
	container.querySelector("#colorModeRow").classList.toggle("hidden", !showMode);
	for (const key of Object.keys(COLOR_DEFAULTS)) {
		container.querySelector(`#${key}`).addEventListener("change", () => {
			touchedColorFields.add(key);
			onChange();
		});
	}
	for (const key of ["watch", "risk", "critical"]) {
		container.querySelector(`#${key}`).addEventListener("input", updateMarksWarning);
	}
	container.querySelector("#colorReset").addEventListener("click", () => {
		touchedColorFields.clear();
		applyColorSettings(COLOR_DEFAULTS);
		onChange();
	});
}

// Fields the user edited in this PI. They are saved even when set back to
// the default; every other field is saved only when it differs from the
// default, so a key that never set a field follows future defaults.
const touchedColorFields = new Set();

function isHexColor(value) {
	return typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value);
}

// A stored value the field can't show falls back to the default: an
// <input type=color> would show (and later save) #000000, and a <select>
// would go blank and save "".
function validColorValue(key, value) {
	const fallback = COLOR_DEFAULTS[key];
	if (typeof fallback === "number") {
		const n = typeof value === "string" && value.trim() !== "" ? Number(value) : value;
		return typeof n === "number" && Number.isFinite(n) ? n : fallback;
	}
	if (key === "colorMode") {
		return value === "fixed" || value === "pace" ? value : fallback;
	}
	return isHexColor(value) ? value.toLowerCase() : fallback;
}

function applyColorSettings(settings) {
	for (const key of Object.keys(COLOR_DEFAULTS)) {
		document.getElementById(key).value = validColorValue(key, settings[key]);
	}
	updateMarksWarning();
}

// Numbers go out as JSON numbers; a cleared number input counts as its
// default rather than "".
function readColorValue(key) {
	const fallback = COLOR_DEFAULTS[key];
	const raw = document.getElementById(key).value;
	if (typeof fallback === "number") {
		const n = Number(raw);
		return raw.trim() !== "" && Number.isFinite(n) ? n : fallback;
	}
	return raw;
}

function isDefaultColorValue(key, value) {
	const fallback = COLOR_DEFAULTS[key];
	return typeof value === "string" && typeof fallback === "string"
		? value.toLowerCase() === fallback.toLowerCase()
		: value === fallback;
}

function readColorSettings() {
	const out = {};
	for (const key of Object.keys(COLOR_DEFAULTS)) {
		const value = readColorValue(key);
		if (touchedColorFields.has(key) || !isDefaultColorValue(key, value)) {
			out[key] = value;
		}
	}
	return out;
}

// Mirrors Marks::sanitized in src/level.rs: clamp to 0..=100, then the
// marks must be strictly increasing or the plugin uses the defaults.
function updateMarksWarning() {
	const clamp = (n) => Math.min(100, Math.max(0, n));
	const [watch, risk, critical] = ["watch", "risk", "critical"].map((key) => clamp(readColorValue(key)));
	document.getElementById("marksWarning").classList.toggle("hidden", watch < risk && risk < critical);
}
