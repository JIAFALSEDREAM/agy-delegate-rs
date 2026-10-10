const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { EventEmitter } = require("node:events");
const { test } = require("node:test");

const source = fs.readFileSync(path.join(__dirname, "../npm/bin/agy-delegate-rs.cjs"), "utf8");
function run(platform, arch, { exists = true, result = { status: 0 } } = {}) {
  const observed = {};
  const stop = Symbol("exit");
  const process = {
    platform, arch, argv: ["node", "launcher", "run", "--task", "spaces; $literal"], pid: 123,
    exit(code) { observed.exit = code; throw stop; },
    kill(pid, signal) { observed.signal = [pid, signal]; throw stop; },
    on() {}, off() {},
  };
  const context = {
    process, __dirname: "/package/bin", console: { error: message => { observed.error = message; } },
    require(name) {
      if (name === "node:fs") return { existsSync: () => exists };
      if (name === "node:path") return path;
      if (name === "node:child_process") return { spawn(binary, args, options) {
        observed.spawn = { binary, args: Array.from(args), options };
        const child = new EventEmitter();
        child.kill = () => {};
        observed.child = child;
        return child;
      } };
      throw new Error(name);
    },
  };
  try { vm.runInNewContext(source, context); } catch (error) { if (error !== stop) throw error; }
  if (observed.child) {
    try {
      if (result.error) observed.child.emit("error", result.error);
      observed.child.emit("close", result.status, result.signal);
    } catch (error) { if (error !== stop) throw error; }
    observed.exit = process.exitCode;
  }
  return observed;
}

test("selects each supported binary and preserves arguments and exit status", () => {
  for (const [platform, arch] of [["linux", "x64"], ["linux", "arm64"], ["darwin", "x64"], ["darwin", "arm64"], ["win32", "x64"], ["win32", "arm64"]]) {
    const value = run(platform, arch, { result: { status: 17 } });
    assert.equal(value.exit, 17);
    assert.equal(value.spawn.binary, path.join("/package/bin", `${platform}-${arch}`, `agy-delegate-rs${platform === "win32" ? ".exe" : ""}`));
    assert.deepEqual(value.spawn.args, ["run", "--task", "spaces; $literal"]);
    assert.equal(value.spawn.options.stdio, "inherit");
  }
});
test("unsupported and incomplete installations fail with an actionable message", () => {
  const unsupported = run("win32", "ia32");
  assert.equal(unsupported.exit, 1);
  assert.match(unsupported.error, /unsupported platform/);
  assert.equal(unsupported.spawn, undefined);
  const missing = run("linux", "x64", { exists: false });
  assert.equal(missing.exit, 1);
  assert.match(missing.error, /reinstall/);
});
test("execution errors and child signals propagate", () => {
  const error = run("linux", "x64", { result: { error: new Error("EACCES") } });
  assert.equal(error.exit, 1);
  assert.match(error.error, /EACCES/);
  assert.deepEqual(run("linux", "x64", { result: { signal: "SIGINT" } }).signal, [123, "SIGINT"]);
});

test("SIGTERM sent to the launcher reaches its native child", { timeout: 10000, skip: process.platform === "win32" }, async t => {
  const os = require("node:os");
  const { spawn } = require("node:child_process");
  const { setTimeout: delay } = require("node:timers/promises");
  const { once } = require("node:events");
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "agy-launcher-"));
  const native = path.join(directory, `${process.platform}-${process.arch}`);
  fs.mkdirSync(native);
  fs.copyFileSync(path.join(__dirname, "../npm/bin/agy-delegate-rs.cjs"), path.join(directory, "launcher.cjs"));
  fs.writeFileSync(path.join(native, "agy-delegate-rs"), '#!/usr/bin/env node\nconst fs=require("node:fs");process.on("SIGTERM",()=>{fs.writeFileSync(process.env.STOPPED,"yes");process.exit(0)});fs.writeFileSync(process.env.READY,"yes");setInterval(()=>{},1000);', { mode: 0o755 });
  const ready = path.join(directory, "ready"), stopped = path.join(directory, "stopped");
  const child = spawn(process.execPath, [path.join(directory, "launcher.cjs")], { detached: true, stdio: "ignore", env: { ...process.env, READY: ready, STOPPED: stopped } });
  const exited = once(child, "exit");
  t.after(async () => {
    try {
      try { process.kill(-child.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") throw error; }
      await exited;
    } finally { fs.rmSync(directory, { recursive: true, force: true }); }
  });
  for (let attempts = 0; attempts < 100 && !fs.existsSync(ready); attempts++) await delay(20);
  assert.ok(fs.existsSync(ready), "native child started");
  child.kill("SIGTERM");
  const [code] = await exited;
  assert.equal(code, 0);
  assert.equal(fs.readFileSync(stopped, "utf8"), "yes");
});
