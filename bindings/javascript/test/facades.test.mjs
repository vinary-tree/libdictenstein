import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", root)));

test("all language facades select the shared umbrella runtime", () => {
  assert.equal(packageJson.name, "@vinary-tree/libdictenstein");
  assert.equal(packageJson.dependencies["@vinary-tree/vinary-tree"], "4.0.0-rc.1");
  for (const path of [".", "./typescript", "./clojurescript", "./wasm", "./wasi"]) {
    assert.ok(packageJson.exports[path]);
  }
});

test("ClojureScript mirrors Clojure CRUD naming", async () => {
  const source = await readFile(new URL("cljs/vinary_tree/libdictenstein.cljs", root), "utf8");
  for (const name of ["dynamic-dawg", "double-array-trie", "scdawg", "contains?", "get", "put!", "put-all!", "remove!", "snapshot", "entries", "keys", "values", "reduce-entries", "with-entry-stream", "clear!", "compact!", "contains-substring?", "frequency", "close!"]) {
    assert.ok(source.includes(`(defn ${name}`), `missing ${name}`);
  }
  assert.equal((source.match(/\.streamEntries dictionary/g) ?? []).length, 2);
  assert.equal(source.includes("(.close cursor)"), true);
});
