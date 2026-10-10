const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const crypto = require("node:crypto");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");

const payload = Buffer.from("package fixture");
const integrity = `sha512-${crypto.createHash("sha512").update(payload).digest("base64")}`;
const missing = { status: 1, value: { error: { code: "E404" } } };
const present = { status: 0, value: integrity };
function scenario(queries, publishStatus = 0) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "agy-npm-publish-"));
  try {
    fs.writeFileSync(path.join(directory, "package.tgz"), payload);
    fs.writeFileSync(path.join(directory, "scenario.json"), JSON.stringify({ queries, publishStatus }));
    fs.writeFileSync(path.join(directory, "npm"), `#!${process.execPath}
const fs=require("node:fs"),path=require("node:path"),root=process.env.FIXTURE;
const file=path.join(root,"calls.json");const calls=fs.existsSync(file)?JSON.parse(fs.readFileSync(file)):[];
const args=process.argv.slice(2);const index=calls.filter(c=>c[0]==="view").length;calls.push(args);fs.writeFileSync(file,JSON.stringify(calls));
const scenario=JSON.parse(fs.readFileSync(path.join(root,"scenario.json")));
if(args[0]==="view"){const result=scenario.queries[Math.min(index,scenario.queries.length-1)];console.log(JSON.stringify(result.value));process.exit(result.status)}
if(args[0]==="publish")process.exit(scenario.publishStatus);process.exit(99);
`, { mode: 0o755 });
    const result = spawnSync(process.execPath, [path.join(__dirname, "publish-npm.mjs"), path.join(directory, "package.tgz"), "0.1.0"], {
      env: { ...process.env, FIXTURE: directory, PATH: `${directory}${path.delimiter}${process.env.PATH}` }, encoding: "utf8", timeout: 15000,
    });
    if (result.error) throw result.error;
    return { status: result.status, stderr: result.stderr, calls: JSON.parse(fs.readFileSync(path.join(directory, "calls.json"))) };
  } finally { fs.rmSync(directory, { recursive: true, force: true }); }
}

test("identical immutable npm versions are skipped", () => {
  const result = scenario([present]);
  assert.equal(result.status, 0);
  assert.equal(result.calls.length, 1);
});
test("different artifacts and registry outages prevent publication", () => {
  for (const query of [{ status: 0, value: "different" }, { status: 1, value: { error: { code: "E500" } } }]) {
    const result = scenario([query]);
    assert.equal(result.status, 1);
    assert.ok(result.calls.every(call => call[0] === "view"));
  }
});
test("only a missing version is published with public access and provenance", () => {
  const result = scenario([missing]);
  assert.equal(result.status, 0);
  assert.deepEqual(result.calls[1].slice(2), ["--access", "public", "--provenance"]);
});
test("an accepted upload followed by a client failure is reconciled", () => {
  const result = scenario([missing, present], 1);
  assert.equal(result.status, 0);
  assert.deepEqual(result.calls.map(call => call[0]), ["view", "publish", "view"]);
});
