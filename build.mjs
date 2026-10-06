#!/usr/bin/env node
// Assembles dist/<uuid>.sdPlugin/ from assets/ + the release binary of every
// requested target, named the way manifest.json's CodePaths expects.
//
// Usage:
//   node build.mjs                  every CodePaths target that has been built
//                                   (target/<triple>/release, or target/release
//                                   for the host triple)
//   node build.mjs <triple>...      exactly these targets; each must be built
//   node build.mjs --all            every CodePaths target; each must be built
//                                   (what the Release workflow runs)
// Requires: cargo build --release [--target <triple>] already run.
import { cpSync, copyFileSync, mkdirSync, rmSync, existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { join } from "node:path";

const UUID = "com.jfms7s.claudeusage";
const BIN_NAME = "opendeck-claude-usage";

function fail(message) {
	console.error(message);
	process.exit(1);
}

// Cargo.toml's [package] version and manifest.json's "Version" have nothing
// keeping them in sync - catch drift here rather than shipping a plugin
// whose crate version and Elgato-facing manifest version disagree.
const cargoToml = readFileSync("Cargo.toml", "utf8");
const cargoVersionMatch = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
if (!cargoVersionMatch) {
	fail("could not find `version = \"...\"` in Cargo.toml");
}
const cargoVersion = cargoVersionMatch[1];

const manifest = JSON.parse(readFileSync("assets/manifest.json", "utf8"));
const manifestVersion = manifest.Version;

if (cargoVersion !== manifestVersion) {
	fail(`version mismatch: Cargo.toml is ${cargoVersion} but assets/manifest.json is ${manifestVersion} - bump them together`);
}

const codePaths = manifest.CodePaths || {};
for (const [target, file] of Object.entries(codePaths)) {
	if (file !== `${BIN_NAME}-${target}`) {
		fail(`manifest.json CodePaths["${target}"] is "${file}", expected "${BIN_NAME}-${target}"`);
	}
}
for (const key of ["CodePathLin", "CodePathMac"]) {
	if (manifest[key] && !Object.values(codePaths).includes(manifest[key])) {
		fail(`manifest.json ${key} "${manifest[key]}" is not one of its CodePaths`);
	}
}

function hostTriple() {
	try {
		const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
		return out.match(/^host:\s*(\S+)/m)?.[1];
	} catch {
		return undefined;
	}
}

// Where `cargo build --release` left this target's binary, if anywhere.
function binaryFor(target, host) {
	const cross = join("target", target, "release", BIN_NAME);
	if (existsSync(cross)) return cross;
	const native = join("target", "release", BIN_NAME);
	if (target === host && existsSync(native)) return native;
	return undefined;
}

const args = process.argv.slice(2);
const host = hostTriple();
const explicit = args.filter((a) => a !== "--all");
const requireEvery = args.includes("--all");
if (requireEvery && explicit.length > 0) {
	fail("usage: node build.mjs [--all | <target-triple>...]");
}
let targets;
if (requireEvery) {
	targets = Object.keys(codePaths);
} else if (explicit.length > 0) {
	targets = explicit;
} else {
	targets = Object.keys(codePaths).filter((t) => binaryFor(t, host));
}

if (targets.length === 0) {
	fail(`no release binary found for any CodePaths target (${Object.keys(codePaths).join(", ")}); run: cargo build --release --locked`);
}

const binaries = [];
for (const target of targets) {
	if (!(target in codePaths)) {
		fail(`${target} is not in manifest.json's CodePaths - OpenDeck would never launch it`);
	}
	const binPath = binaryFor(target, host);
	if (!binPath) {
		fail(`missing release binary for ${target} (run: cargo build --release --locked --target ${target})`);
	}
	binaries.push([target, binPath]);
}

// Cleared once, then every target is added - running this per target
// would drop the previous target's binary.
const outDir = join("dist", `${UUID}.sdPlugin`);
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

cpSync("assets/manifest.json", join(outDir, "manifest.json"));
// Not every plugin has layouts or a property inspector - copy what exists.
// Icon sources (assets/icon-src/) are deliberately not shipped.
for (const dir of ["icons", "layouts", "propertyInspector"]) {
	if (existsSync(join("assets", dir))) {
		cpSync(join("assets", dir), join(outDir, dir), { recursive: true });
	}
}
for (const [target, binPath] of binaries) {
	copyFileSync(binPath, join(outDir, codePaths[target]));
}

const missing = Object.entries(codePaths).filter(([, file]) => !existsSync(join(outDir, file)));
if (missing.length > 0) {
	const list = missing.map(([target]) => target).join(", ");
	if (requireEvery) {
		fail(`bundle is missing CodePaths targets: ${list}`);
	}
	console.warn(`note: not bundled (not built): ${list}`);
}
if (manifest.CodePathLin && !existsSync(join(outDir, manifest.CodePathLin))) {
	console.warn(`note: CodePathLin (${manifest.CodePathLin}) is not in this bundle`);
}
console.log(`built ${outDir} for ${targets.join(", ")}`);
