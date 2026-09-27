// Shared "Colors & thresholds" section for the Usage Gauge and Burn Rate
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
	container.innerHTML = `
		<details>
			<summary>Colors &amp; thresholds</summary>
			<label for="watch">Watch at (% used)</label>
			<input type="number" id="watch" min="0" max="100" step="1" />
			<label for="risk">Risk at (% used)</label>
			<input type="number" id="risk" min="0" max="100" step="1" />
			<label for="critical">Critical at (% used)</label>
			<input type="number" id="critical" min="0" max="100" step="1" />
			<label for="colorNormal">Normal color</label>
			<input type="color" id="colorNormal" />
			<label for="colorWatch">Watch color</label>
			<input type="color" id="colorWatch" />
			<label for="colorRisk">Risk color</label>
			<input type="color" id="colorRisk" />
			<label for="colorCritical">Critical color</label>
			<input type="color" id="colorCritical" />
			<div id="colorModeRow">
				<label for="colorMode">Color by</label>
				<select id="colorMode">
					<option value="fixed">Current usage</option>
					<option value="pace">Pace (also warn when burning too fast)</option>
				</select>
			</div>
			<p class="hint">Marks must increase (Watch &lt; Risk &lt; Critical), otherwise defaults are used.</p>
			<button type="button" id="colorReset">Reset to defaults</button>
		</details>`;
	container.querySelector("#colorModeRow").hidden = !showMode;
	for (const key of Object.keys(COLOR_DEFAULTS)) {
		container.querySelector(`#${key}`).addEventListener("change", onChange);
	}
	container.querySelector("#colorReset").addEventListener("click", () => {
		applyColorSettings(COLOR_DEFAULTS);
		onChange();
	});
}

function applyColorSettings(settings) {
	for (const [key, fallback] of Object.entries(COLOR_DEFAULTS)) {
		document.getElementById(key).value = settings[key] ?? fallback;
	}
}

// Numbers go out as JSON numbers; a cleared number input sends its default
// rather than "" (the plugin would also fall back, but this keeps the
// stored settings readable).
function readColorSettings() {
	const out = {};
	for (const [key, fallback] of Object.entries(COLOR_DEFAULTS)) {
		const raw = document.getElementById(key).value;
		if (typeof fallback === "number") {
			const n = Number(raw);
			out[key] = raw.trim() !== "" && Number.isFinite(n) ? n : fallback;
		} else {
			out[key] = raw;
		}
	}
	return out;
}
