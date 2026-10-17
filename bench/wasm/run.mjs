// Runs the WebAssembly gets (bench/wasm) on one key dump, bbtree then std's
// BTreeMap, and prints a row each as bbtree-scaling's scaling.csv has them:
//
//   node run.mjs <module.wasm> <u64|uuid|wiki-titles|str-json> <dump> <seconds>
import { readFileSync } from "node:fs";

const [wasm, keys, dumpPath, seconds] = process.argv.slice(2);
const kind = { u64: 0, uuid: 1, "wiki-titles": 2, "str-json": 2 }[keys];
if (kind === undefined) throw new Error(`no key kind ${keys}`);

const { instance } = await WebAssembly.instantiate(readFileSync(wasm), {
  env: { now_ms: () => performance.now() },
});
const { memory, reserve, measure } = instance.exports;
const dump = readFileSync(dumpPath);
const at = reserve(dump.length);
new Uint8Array(memory.buffer, at, dump.length).set(dump);
const out = reserve(24);

for (const [code, map] of [[0, "bbtree"], [1, "std-btree"]]) {
  measure(kind, code, at, dump.length, Number(seconds), out);
  // The memory may have grown: read it afresh, copied, as `out` needn't be
  // aligned.
  const [operations, elapsedNs, hits] = new Float64Array(memory.buffer.slice(out, out + 24));
  const share = hits / operations;
  if (share < 0.45 || share > 0.55) console.error(`${map}: ${(share * 100).toFixed(1)}% of gets hit`);
  console.log(
    `${map},${keys},get-half,1,${operations},${Math.round(elapsedNs)},${((operations / elapsedNs) * 1e3).toFixed(3)}`,
  );
}
