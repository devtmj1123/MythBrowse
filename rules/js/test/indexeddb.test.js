// Node test for rules/js/indexeddb.js (the in-memory IndexedDB shim). Run: node rules/js/test/indexeddb.test.js
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
"use strict";
const fs = require("fs");
const path = require("path");
const assert = require("assert");
const vm = require("vm");

const src = fs.readFileSync(path.join(__dirname, "..", "indexeddb.js"), "utf8");
const ctx = { setTimeout, clearTimeout, structuredClone, DOMException, Promise, console, ArrayBuffer, Uint8Array, Date, JSON, Math, Object, Array, String, TypeError, Error };
ctx.window = ctx;
vm.createContext(ctx);
vm.runInContext("(function(){" + src + "\n})();", ctx);
const { indexedDB, IDBKeyRange } = ctx;
// values come from another realm (vm context): compare by structure
const deq = (a, b, m) => assert.deepStrictEqual(JSON.parse(JSON.stringify(a)), JSON.parse(JSON.stringify(b)), m);

const p = (req) => new Promise((res, rej) => { req.onsuccess = () => res(req.result); req.onerror = (e) => { e.preventDefault(); rej(req.error); }; });
const done = (tx) => new Promise((res, rej) => { tx.oncomplete = () => res(); tx.onabort = () => rej(tx.error); });

async function main() {
  // open + upgrade
  let upgraded = 0;
  const open = indexedDB.open("app", 2);
  open.onupgradeneeded = (e) => {
    upgraded++;
    assert.strictEqual(e.oldVersion, 0);
    assert.strictEqual(e.newVersion, 2);
    const db = open.result;
    const s = db.createObjectStore("people", { keyPath: "id", autoIncrement: true });
    s.createIndex("by_name", "name");
    s.createIndex("by_email", "email", { unique: true });
    s.createIndex("by_tag", "tags", { multiEntry: true });
    db.createObjectStore("kv");
  };
  const db = await p(open);
  assert.strictEqual(upgraded, 1);
  assert.strictEqual(db.version, 2);
  assert.ok(db.objectStoreNames.contains("people") && db.objectStoreNames.contains("kv"));

  // writes
  let tx = db.transaction(["people", "kv"], "readwrite");
  const people = tx.objectStore("people");
  const k1 = await p(people.add({ name: "Ann", email: "a@x", tags: ["a", "b"] }));
  const k2 = await p(people.add({ name: "Bob", email: "b@x", tags: ["b"] }));
  await p(people.put({ id: 10, name: "Cid", email: "c@x", tags: [] }));
  const k4 = await p(people.add({ name: "Ann", email: "a2@x", tags: ["c"] }));
  deq([k1, k2, k4], [1, 2, 11]);
  await p(tx.objectStore("kv").put("v1", "key1"));
  await p(tx.objectStore("kv").put({ deep: [1, 2] }, ["arr", 1]));
  await done(tx);

  // reads
  tx = db.transaction("people");
  const s = tx.objectStore("people");
  assert.strictEqual((await p(s.get(2))).name, "Bob");
  assert.strictEqual(await p(s.count()), 4);
  deq(await p(s.getAllKeys()), [1, 2, 10, 11]);
  deq((await p(s.getAll(IDBKeyRange.bound(2, 10)))).map((x) => x.name), ["Bob", "Cid"]);
  deq((await p(s.index("by_name").getAll("Ann"))).map((x) => x.id), [1, 11]);
  assert.strictEqual((await p(s.index("by_email").get("c@x"))).id, 10);
  assert.strictEqual(await p(s.index("by_tag").count("b")), 2);
  assert.strictEqual(await p(s.index("by_name").getKey("Bob")), 2);
  await done(tx);

  // readonly write throws ReadOnlyError
  tx = db.transaction("kv", "readonly");
  assert.throws(() => tx.objectStore("kv").put(1, "x"), (e) => e.name === "ReadOnlyError");
  assert.strictEqual(await p(tx.objectStore("kv").get("key1")), "v1");
  deq(await p(tx.objectStore("kv").get(["arr", 1])), { deep: [1, 2] });
  await done(tx);

  // cursor iteration (next / prev / update / delete)
  tx = db.transaction("people", "readwrite");
  const names = [];
  await new Promise((res) => {
    const r = tx.objectStore("people").openCursor();
    r.onsuccess = () => {
      const c = r.result;
      if (!c) return res();
      names.push(c.value.name);
      if (c.value.name === "Cid") c.update(Object.assign({}, c.value, { name: "Cyd" }));
      if (c.value.name === "Bob") c["delete"]();
      c["continue"]();
    };
  });
  deq(names, ["Ann", "Bob", "Cid", "Ann"]);
  const rev = [];
  await new Promise((res) => {
    const r = tx.objectStore("people").index("by_name").openCursor(null, "prev");
    r.onsuccess = () => { const c = r.result; if (!c) return res(); rev.push(c.key + ":" + c.primaryKey); c["continue"](); };
  });
  deq(rev, ["Cyd:10", "Ann:11", "Ann:1"]);
  await done(tx);

  // unique index violation aborts the transaction and rolls back
  tx = db.transaction("people", "readwrite");
  const st = tx.objectStore("people");
  st.put({ id: 50, name: "Tmp", email: "t@x", tags: [] });
  const bad = st.add({ name: "Dup", email: "a@x", tags: [] });
  let reqErr = null;
  bad.onerror = () => { reqErr = bad.error; };
  await done(tx).then(() => assert.fail("should abort"), (e) => assert.strictEqual(e.name, "ConstraintError"));
  assert.strictEqual(reqErr.name, "ConstraintError");
  tx = db.transaction("people");
  assert.strictEqual(await p(tx.objectStore("people").get(50)), undefined, "rolled back");
  assert.strictEqual(await p(tx.objectStore("people").count()), 3);
  await done(tx);

  // explicit abort rolls back
  tx = db.transaction("kv", "readwrite");
  tx.objectStore("kv").clear();
  const ab = done(tx);
  setTimeout(() => {}, 0);
  await new Promise((r) => setTimeout(r, 0));
  try { tx.abort(); } catch (e) { /* may have completed */ }
  await ab.catch(() => {});
  tx = db.transaction("kv");
  assert.strictEqual(await p(tx.objectStore("kv").count()), 2, "clear rolled back by abort");

  // reopen same version: no upgrade; lower version: VersionError
  let up2 = 0;
  const o2 = indexedDB.open("app", 2);
  o2.onupgradeneeded = () => up2++;
  const db2 = await p(o2);
  assert.strictEqual(up2, 0);
  assert.strictEqual(db2.version, 2);
  await p(indexedDB.open("app", 1)).then(() => assert.fail("lower version"), (e) => assert.strictEqual(e.name, "VersionError"));
  // open without version on a new db -> version 1
  const db3 = await p(indexedDB.open("fresh"));
  assert.strictEqual(db3.version, 1);
  deq((await indexedDB.databases()).map((d) => d.name).sort(), ["app", "fresh"]);
  await p(indexedDB.deleteDatabase("fresh"));
  deq((await indexedDB.databases()).map((d) => d.name), ["app"]);

  // key comparison order: number < Date < string < binary < array
  assert.strictEqual(indexedDB.cmp(1, new Date(0)), -1);
  assert.strictEqual(indexedDB.cmp("a", new Date(0)), 1);
  assert.strictEqual(indexedDB.cmp(["a"], "zzz"), 1);
  assert.strictEqual(indexedDB.cmp([1, 2], [1, 2]), 0);
  assert.throws(() => indexedDB.cmp({}, 1), (e) => e.name === "DataError");
  assert.ok(IDBKeyRange.lowerBound(5, true).includes(6) && !IDBKeyRange.lowerBound(5, true).includes(5));

  // idb-keyval style: promise chain across microtasks within one transaction
  const kvOpen = indexedDB.open("keyval-store");
  kvOpen.onupgradeneeded = () => kvOpen.result.createObjectStore("keyval");
  const kvdb = await p(kvOpen);
  const ktx = kvdb.transaction("keyval", "readwrite");
  const ks = ktx.objectStore("keyval");
  await p(ks.put(1, "a"));
  await p(ks.put(2, "b"));
  deq(await p(ks.getAll()), [1, 2]);
  await done(ktx);
  console.log("indexeddb shim: all tests passed");
}
main().catch((e) => { console.error(e); process.exit(1); });
