#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");

const platform = `${process.platform}-${process.arch}`;
const supported = ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64"];
if (!supported.includes(platform)) {
  console.error(`agy-delegate-rs: unsupported platform ${platform}; install from source with Cargo.`);
  process.exit(1);
}
const binary = path.join(__dirname, platform, `agy-delegate-rs${process.platform === "win32" ? ".exe" : ""}`);
if (!fs.existsSync(binary)) {
  console.error("agy-delegate-rs: native binary missing; reinstall the published npm package.");
  process.exit(1);
}
const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
const handlers = new Map(["SIGINT", "SIGTERM"].map(signal => [signal, () => child.kill(signal)]));
for (const [signal, handler] of handlers) process.on(signal, handler);
let failed = false;
child.once("error", error => {
  console.error(`agy-delegate-rs: ${error.message}`);
  failed = true;
});
child.once("close", (code, signal) => {
  for (const [name, handler] of handlers) process.off(name, handler);
  if (signal) process.kill(process.pid, signal);
  process.exitCode = failed ? 1 : (code ?? 1);
});
