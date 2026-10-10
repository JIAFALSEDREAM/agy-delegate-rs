import fs from "node:fs";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";

const [archive, version] = process.argv.slice(2);
if (!archive || !/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version ?? "")) {
  throw new Error("usage: node scripts/publish-npm.mjs ARCHIVE VERSION");
}
const integrity = `sha512-${crypto.createHash("sha512").update(fs.readFileSync(archive)).digest("base64")}`;
const spec = `agy-delegate-rs@${version}`;
const query = spawnSync("npm", ["view", spec, "dist.integrity", "--json"], { encoding: "utf8" });
if (query.error) throw query.error;
let metadata;
try { metadata = JSON.parse(query.stdout); }
catch { throw new Error(`Cannot query registry for ${spec}: ${query.stderr}`); }
if (query.status === 0) {
  if (metadata !== integrity) throw new Error(`Published ${spec} differs from this artifact`);
  console.log(`Verified existing publication: ${spec}`);
} else {
  if (metadata?.error?.code !== "E404") throw new Error(`Registry query failed: ${query.stderr || query.stdout}`);
  const publish = spawnSync("npm", ["publish", archive, "--access", "public", "--provenance"], { stdio: "inherit" });
  if (publish.error) throw publish.error;
  if (publish.status !== 0) {
    let confirmed = false;
    for (let attempt = 0; attempt < 3; attempt++) {
      await new Promise(resolve => setTimeout(resolve, 1000 * 2 ** attempt));
      const retry = spawnSync("npm", ["view", spec, "dist.integrity", "--json"], { encoding: "utf8" });
      if (retry.status !== 0) continue;
      const published = JSON.parse(retry.stdout);
      if (published !== integrity) throw new Error(`Published ${spec} differs from this artifact`);
      confirmed = true;
      break;
    }
    if (!confirmed) throw new Error(`Publishing ${spec} failed; retry the same job`);
  }
}
