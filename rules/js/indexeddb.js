// MythBrowse compat: in-memory IndexedDB shim (rule polyfill.indexeddb in rules/core.toml).
// Servo has no IndexedDB yet. Sites (Firebase, idb-keyval, localforage, analytics SDKs, React
// Query persisters...) throw "indexedDB is not defined" or reject at start-up. This gives them a
// working, spec-shaped API for the life of the page: databases live in memory (per document),
// nothing is written to disk, so data is gone after a reload, the same as a private window
// that clears on close. Values are stored as structured clones.
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
if (!("indexedDB" in window)) {
  var later = function (fn) { setTimeout(fn, 0); };
  var clone = function (v) {
    if (typeof structuredClone === "function") return structuredClone(v);
    return v === undefined ? undefined : JSON.parse(JSON.stringify(v));
  };
  var err = function (name, msg) {
    try { return new DOMException(msg || name, name); } catch (e) { var x = new Error(msg || name); x.name = name; return x; }
  };
  // ---------------------------------------------------------------- keys
  var rank = function (k) {
    if (typeof k === "number") return isNaN(k) ? 0 : 1;
    if (k instanceof Date) return isNaN(k.getTime()) ? 0 : 2;
    if (typeof k === "string") return 3;
    if (k instanceof ArrayBuffer || ArrayBuffer.isView(k)) return 4;
    if (Array.isArray(k)) return 5;
    return 0;
  };
  var validKey = function (k) {
    var r = rank(k);
    if (r === 5) { for (var i = 0; i < k.length; i++) if (!validKey(k[i])) return false; }
    return r > 0;
  };
  var bytes = function (k) { return k instanceof ArrayBuffer ? new Uint8Array(k) : new Uint8Array(k.buffer, k.byteOffset, k.byteLength); };
  var cmp = function (a, b) {
    var ra = rank(a), rb = rank(b);
    if (!ra || !rb) throw err("DataError", "The parameter is not a valid key.");
    if (ra !== rb) return ra < rb ? -1 : 1;
    if (ra === 1) return a < b ? -1 : a > b ? 1 : 0;
    if (ra === 2) { a = a.getTime(); b = b.getTime(); return a < b ? -1 : a > b ? 1 : 0; }
    if (ra === 3) return a < b ? -1 : a > b ? 1 : 0;
    if (ra === 4) {
      var x = bytes(a), y = bytes(b), n = Math.min(x.length, y.length);
      for (var i = 0; i < n; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
      return x.length === y.length ? 0 : x.length < y.length ? -1 : 1;
    }
    for (var j = 0; j < Math.min(a.length, b.length); j++) { var c = cmp(a[j], b[j]); if (c) return c; }
    return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
  };
  var keyAt = function (value, path) {
    if (Array.isArray(path)) {
      var out = [];
      for (var i = 0; i < path.length; i++) { var k = keyAt(value, path[i]); if (k === undefined) return undefined; out.push(k); }
      return out;
    }
    if (path === "") return value;
    var parts = String(path).split("."), v = value;
    for (var p = 0; p < parts.length; p++) {
      if (v === null || v === undefined) return undefined;
      if (typeof v === "string" && parts[p] === "length") v = v.length;
      else v = v[parts[p]];
    }
    return v;
  };
  var setKeyAt = function (value, path, key) {
    var parts = String(path).split("."), v = value;
    for (var p = 0; p < parts.length - 1; p++) { if (v[parts[p]] === undefined) v[parts[p]] = {}; v = v[parts[p]]; }
    v[parts[parts.length - 1]] = key;
  };
  // ---------------------------------------------------------------- key ranges
  var IDBKeyRange = function IDBKeyRange(lower, upper, lowerOpen, upperOpen) {
    this.lower = lower; this.upper = upper; this.lowerOpen = !!lowerOpen; this.upperOpen = !!upperOpen;
  };
  IDBKeyRange.prototype.includes = function (k) {
    if (this.lower !== undefined) { var a = cmp(k, this.lower); if (a < 0 || (a === 0 && this.lowerOpen)) return false; }
    if (this.upper !== undefined) { var b = cmp(k, this.upper); if (b > 0 || (b === 0 && this.upperOpen)) return false; }
    return true;
  };
  var needKey = function (k) { if (!validKey(k)) throw err("DataError", "The parameter is not a valid key."); return k; };
  IDBKeyRange.only = function (k) { needKey(k); return new IDBKeyRange(k, k, false, false); };
  IDBKeyRange.lowerBound = function (k, open) { needKey(k); return new IDBKeyRange(k, undefined, open, true); };
  IDBKeyRange.upperBound = function (k, open) { needKey(k); return new IDBKeyRange(undefined, k, true, open); };
  IDBKeyRange.bound = function (l, u, lo, uo) {
    needKey(l); needKey(u);
    if (cmp(l, u) > 0) throw err("DataError", "lower is greater than upper");
    return new IDBKeyRange(l, u, lo, uo);
  };
  var toRange = function (q) {
    if (q === undefined || q === null) return null;
    if (q instanceof IDBKeyRange) return q;
    return IDBKeyRange.only(q);
  };
  // ---------------------------------------------------------------- events
  var Emitter = function () {};
  Emitter.prototype.addEventListener = function (type, fn) {
    if (typeof fn !== "function" && !(fn && typeof fn.handleEvent === "function")) return;
    var l = this.__l || (this.__l = {});
    (l[type] || (l[type] = [])).push(fn);
  };
  Emitter.prototype.removeEventListener = function (type, fn) {
    var l = this.__l && this.__l[type];
    if (l) { var i = l.indexOf(fn); if (i >= 0) l.splice(i, 1); }
  };
  Emitter.prototype.dispatchEvent = function (ev) {
    try { Object.defineProperty(ev, "target", { value: ev.target || this, configurable: true }); } catch (e) { /* plain */ }
    try { Object.defineProperty(ev, "currentTarget", { value: this, configurable: true }); } catch (e) { /* plain */ }
    var h = this["on" + ev.type];
    if (typeof h === "function") h.call(this, ev);
    var l = this.__l && this.__l[ev.type];
    if (l) {
      l = l.slice();
      for (var i = 0; i < l.length; i++) {
        if (typeof l[i] === "function") l[i].call(this, ev); else l[i].handleEvent(ev);
      }
    }
    return !ev.defaultPrevented;
  };
  var event = function (type, extra) {
    var ev = { type: type, bubbles: false, cancelable: true, defaultPrevented: false, timeStamp: Date.now() };
    ev.preventDefault = function () { ev.defaultPrevented = true; };
    ev.stopPropagation = function () {};
    ev.stopImmediatePropagation = function () {};
    if (extra) for (var k in extra) ev[k] = extra[k];
    return ev;
  };
  var nameList = function (names) {
    var a = names.slice().sort();
    a.contains = function (n) { return a.indexOf(n) >= 0; };
    a.item = function (i) { return a[i] === undefined ? null : a[i]; };
    return a;
  };
  var extend = function (Ctor, Base) {
    Ctor.prototype = Object.create(Base.prototype);
    Ctor.prototype.constructor = Ctor;
  };
  // ---------------------------------------------------------------- requests
  var IDBRequest = function IDBRequest() {
    this.result = undefined; this.error = null; this.source = null; this.transaction = null;
    this.readyState = "pending"; this.onsuccess = null; this.onerror = null;
  };
  extend(IDBRequest, Emitter);
  var IDBOpenDBRequest = function IDBOpenDBRequest() { IDBRequest.call(this); this.onupgradeneeded = null; this.onblocked = null; };
  extend(IDBOpenDBRequest, IDBRequest);
  // ---------------------------------------------------------------- storage
  var databases = {}; // name -> { version, stores: { name: store data } }
  var newStoreData = function (keyPath, autoIncrement) {
    return { keyPath: keyPath === undefined ? null : keyPath, autoIncrement: !!autoIncrement, next: 1, recs: [], indexes: {} };
  };
  var find = function (recs, key) { // binary search: index of key, or -(insert point)-1
    var lo = 0, hi = recs.length - 1;
    while (lo <= hi) {
      var mid = (lo + hi) >> 1, c = cmp(recs[mid].k, key);
      if (c === 0) return mid;
      if (c < 0) lo = mid + 1; else hi = mid - 1;
    }
    return -lo - 1;
  };
  var indexKeys = function (ix, value) {
    var k = keyAt(value, ix.keyPath);
    if (k === undefined) return [];
    if (ix.multiEntry && Array.isArray(k)) {
      var out = [];
      for (var i = 0; i < k.length; i++) if (validKey(k[i]) && !out.some(function (o) { return cmp(o, k[i]) === 0; })) out.push(k[i]);
      return out;
    }
    return validKey(k) ? [k] : [];
  };
  // ---------------------------------------------------------------- transactions
  var IDBTransaction = function IDBTransaction(db, names, mode) {
    this.db = db; this.mode = mode; this.objectStoreNames = nameList(names); this.error = null;
    this.oncomplete = null; this.onerror = null; this.onabort = null; this.durability = "default";
    this._queue = []; this._done = false; this._snap = {}; this._running = false;
    var tx = this;
    later(function () { tx._run(); });
  };
  extend(IDBTransaction, Emitter);
  IDBTransaction.prototype.objectStore = function (name) {
    if (this._done) throw err("InvalidStateError", "The transaction has finished.");
    if (!this.objectStoreNames.contains(name)) throw err("NotFoundError", "No objectStore named " + name + " in this transaction");
    return new IDBObjectStore(this, name);
  };
  IDBTransaction.prototype._data = function (name) { return databases[this.db.name].stores[name]; };
  IDBTransaction.prototype._write = function (name) {
    if (this.mode === "readonly") throw err("ReadOnlyError", "The transaction is read-only.");
    if (!(name in this._snap)) {
      var d = this._data(name);
      this._snap[name] = { recs: d.recs.slice(), next: d.next };
    }
    return this._data(name);
  };
  IDBTransaction.prototype._request = function (source, op) {
    if (this._done) throw err("TransactionInactiveError", "The transaction has finished.");
    var req = new IDBRequest();
    req.source = source; req.transaction = this;
    this._queue.push({ req: req, op: op });
    if (!this._running) { var tx = this; later(function () { tx._run(); }); }
    return req;
  };
  IDBTransaction.prototype._run = function () {
    if (this._done || this._running) return;
    this._running = true;
    while (this._queue.length && !this._done) {
      var item = this._queue.shift(), req = item.req;
      try {
        req.result = item.op(req);
        req.readyState = "done";
        if (item.cursorStep) continue;
        req.dispatchEvent(event("success"));
      } catch (e) {
        req.readyState = "done"; req.error = e; req.result = undefined;
        var ev = event("error", { target: req });
        req.dispatchEvent(ev);
        if (!ev.defaultPrevented && !this._done) {
          this.error = e;
          this.dispatchEvent(event("error", { target: req }));
          this._abort(e);
        }
      }
    }
    this._running = false;
    if (!this._done && !this._queue.length) {
      var tx = this;
      later(function () { if (!tx._done && !tx._queue.length && !tx._running) tx._complete(); else tx._run(); });
    }
  };
  IDBTransaction.prototype._complete = function () {
    this._done = true;
    this.dispatchEvent(event("complete"));
    if (this._after) this._after(null);
  };
  IDBTransaction.prototype._abort = function (e) {
    if (this._done) return;
    this._done = true;
    for (var name in this._snap) {
      var d = databases[this.db.name] && databases[this.db.name].stores[name];
      if (d) { d.recs = this._snap[name].recs; d.next = this._snap[name].next; }
    }
    this.error = this.error || e || err("AbortError", "The transaction was aborted.");
    var pending = this._queue; this._queue = [];
    for (var i = 0; i < pending.length; i++) {
      pending[i].req.readyState = "done"; pending[i].req.error = err("AbortError", "The transaction was aborted.");
      pending[i].req.dispatchEvent(event("error"));
    }
    this.dispatchEvent(event("abort"));
    if (this._after) this._after(this.error);
  };
  IDBTransaction.prototype.abort = function () {
    if (this._done) throw err("InvalidStateError", "The transaction has finished.");
    this._abort(err("AbortError", "The transaction was aborted."));
  };
  IDBTransaction.prototype.commit = function () {};
  // ---------------------------------------------------------------- cursors
  var IDBCursor = function IDBCursor(source, list, direction, req, tx, keysOnly) {
    this.source = source; this.direction = direction || "next"; this.request = req;
    this._list = list; this._i = -1; this._tx = tx; this._keysOnly = keysOnly;
    this.key = undefined; this.primaryKey = undefined;
  };
  IDBCursor.prototype._step = function (n) {
    var c = this; this._i += n;
    var req = this.request;
    this._tx._queue.push({
      req: req, cursorStep: false,
      op: function () {
        var e = c._list[c._i];
        if (!e) { c.key = c.primaryKey = undefined; if (!c._keysOnly) c.value = undefined; return null; }
        c.key = e.key; c.primaryKey = e.pk;
        if (!c._keysOnly) {
          var d = c._tx._data(c._store), at = find(d.recs, e.pk);
          c.value = at >= 0 ? clone(d.recs[at].v) : undefined;
        }
        return c;
      }
    });
    if (!this._tx._running) { var tx = this._tx; later(function () { tx._run(); }); }
  };
  IDBCursor.prototype["continue"] = function (key) {
    if (key !== undefined) {
      needKey(key);
      var j = this._i + 1;
      var fwd = this.direction.indexOf("prev") < 0;
      while (j < this._list.length && (fwd ? cmp(this._list[j].key, key) < 0 : cmp(this._list[j].key, key) > 0)) j++;
      this._step(j - this._i);
    } else this._step(1);
  };
  IDBCursor.prototype.advance = function (n) {
    if (!(n > 0)) throw new TypeError("advance: count must be positive");
    this._step(n);
  };
  IDBCursor.prototype.continuePrimaryKey = function (key, pk) {
    var j = this._i + 1;
    while (j < this._list.length && (cmp(this._list[j].key, key) < 0 || (cmp(this._list[j].key, key) === 0 && cmp(this._list[j].pk, pk) < 0))) j++;
    this._step(j - this._i);
  };
  IDBCursor.prototype.update = function (value) {
    var store = new IDBObjectStore(this._tx, this._store);
    var d = this._tx._data(this._store);
    return store.put(value, d.keyPath === null ? this.primaryKey : undefined);
  };
  IDBCursor.prototype["delete"] = function () {
    return new IDBObjectStore(this._tx, this._store)["delete"](this.primaryKey);
  };
  var IDBCursorWithValue = function IDBCursorWithValue() { IDBCursor.apply(this, arguments); this.value = undefined; };
  extend(IDBCursorWithValue, IDBCursor);
  var entries = function (list, range, direction) {
    var out = [];
    for (var i = 0; i < list.length; i++) if (!range || range.includes(list[i].key)) out.push(list[i]);
    if (direction && direction.indexOf("prev") === 0) out.reverse();
    if (direction === "nextunique" || direction === "prevunique") {
      var u = [];
      for (var j = 0; j < out.length; j++) {
        if (u.length && cmp(u[u.length - 1].key, out[j].key) === 0) { if (direction === "prevunique") u[u.length - 1] = out[j]; continue; }
        u.push(out[j]);
      }
      out = u;
    }
    return out;
  };
  var openCursor = function (self, storeName, list, query, direction, keysOnly) {
    var tx = self.transaction, range = toRange(query);
    var req = tx._request(self, function () { return null; });
    var Ctor = keysOnly ? IDBCursor : IDBCursorWithValue;
    var cur = new Ctor(self, null, direction, req, tx, keysOnly);
    cur._store = storeName;
    // replace the queued op: build the list when it runs, then step to the first entry
    var item = tx._queue[tx._queue.length - 1];
    item.op = function () {
      cur._list = entries(list(), range, direction);
      cur._i = 0;
      var e = cur._list[0];
      if (!e) return null;
      cur.key = e.key; cur.primaryKey = e.pk;
      if (!keysOnly) { var d = tx._data(storeName), at = find(d.recs, e.pk); cur.value = at >= 0 ? clone(d.recs[at].v) : undefined; }
      return cur;
    };
    return req;
  };
  // ---------------------------------------------------------------- object stores
  var IDBObjectStore = function IDBObjectStore(tx, name) {
    this.transaction = tx; this.name = name;
    var d = tx._data(name);
    this.keyPath = d.keyPath; this.autoIncrement = d.autoIncrement;
    this.indexNames = nameList(Object.keys(d.indexes));
  };
  var storeEntries = function (tx, name) {
    return function () { return tx._data(name).recs.map(function (r) { return { key: r.k, pk: r.k }; }); };
  };
  IDBObjectStore.prototype._put = function (value, key, noOverwrite) {
    var tx = this.transaction, name = this.name, d = tx._data(name);
    if (tx.mode === "readonly") throw err("ReadOnlyError", "The transaction is read-only.");
    if (d.keyPath !== null && key !== undefined) throw err("DataError", "An explicit key was given to a store with a keyPath.");
    var v = clone(value);
    if (d.keyPath !== null) {
      key = keyAt(v, d.keyPath);
      if (key === undefined && d.autoIncrement) { key = d.next; setKeyAt(v, d.keyPath, key); }
    } else if (key === undefined && d.autoIncrement) key = d.next;
    needKey(key);
    return tx._request(this, function () {
      var w = tx._write(name), at = find(w.recs, key);
      if (at >= 0 && noOverwrite) throw err("ConstraintError", "Key already exists in the object store.");
      for (var ixn in w.indexes) {
        var ix = w.indexes[ixn];
        if (!ix.unique) continue;
        var ks = indexKeys(ix, v);
        for (var r = 0; r < w.recs.length; r++) {
          if (cmp(w.recs[r].k, key) === 0) continue;
          var other = indexKeys(ix, w.recs[r].v);
          for (var a = 0; a < ks.length; a++) for (var b = 0; b < other.length; b++)
            if (cmp(ks[a], other[b]) === 0) throw err("ConstraintError", "Unique index " + ixn + " violated.");
        }
      }
      if (typeof key === "number" && w.autoIncrement && key >= w.next) w.next = Math.floor(key) + 1;
      if (at >= 0) w.recs[at] = { k: key, v: v }; else w.recs.splice(-at - 1, 0, { k: key, v: v });
      return key;
    });
  };
  IDBObjectStore.prototype.put = function (value, key) { return this._put(value, key, false); };
  IDBObjectStore.prototype.add = function (value, key) { return this._put(value, key, true); };
  IDBObjectStore.prototype.get = function (query) {
    var tx = this.transaction, name = this.name, range = toRange(query);
    if (!range) throw err("DataError", "No key or key range specified.");
    return tx._request(this, function () {
      var recs = tx._data(name).recs;
      for (var i = 0; i < recs.length; i++) if (range.includes(recs[i].k)) return clone(recs[i].v);
      return undefined;
    });
  };
  IDBObjectStore.prototype.getKey = function (query) {
    var tx = this.transaction, name = this.name, range = toRange(query);
    return tx._request(this, function () {
      var recs = tx._data(name).recs;
      for (var i = 0; i < recs.length; i++) if (range.includes(recs[i].k)) return recs[i].k;
      return undefined;
    });
  };
  var collect = function (tx, name, query, count, keys) {
    var range = toRange(query), recs = tx._data(name).recs, out = [];
    for (var i = 0; i < recs.length && (!count || out.length < count); i++)
      if (!range || range.includes(recs[i].k)) out.push(keys ? recs[i].k : clone(recs[i].v));
    return out;
  };
  IDBObjectStore.prototype.getAll = function (query, count) {
    var tx = this.transaction, name = this.name;
    return tx._request(this, function () { return collect(tx, name, query, count, false); });
  };
  IDBObjectStore.prototype.getAllKeys = function (query, count) {
    var tx = this.transaction, name = this.name;
    return tx._request(this, function () { return collect(tx, name, query, count, true); });
  };
  IDBObjectStore.prototype.count = function (query) {
    var tx = this.transaction, name = this.name;
    return tx._request(this, function () { return collect(tx, name, query, 0, true).length; });
  };
  IDBObjectStore.prototype["delete"] = function (query) {
    var tx = this.transaction, name = this.name, range = toRange(query);
    if (tx.mode === "readonly") throw err("ReadOnlyError", "The transaction is read-only.");
    return tx._request(this, function () {
      var w = tx._write(name);
      w.recs = w.recs.filter(function (r) { return !range.includes(r.k); });
      return undefined;
    });
  };
  IDBObjectStore.prototype.clear = function () {
    var tx = this.transaction, name = this.name;
    if (tx.mode === "readonly") throw err("ReadOnlyError", "The transaction is read-only.");
    return tx._request(this, function () { tx._write(name).recs = []; return undefined; });
  };
  IDBObjectStore.prototype.openCursor = function (query, direction) {
    return openCursor(this, this.name, storeEntries(this.transaction, this.name), query, direction, false);
  };
  IDBObjectStore.prototype.openKeyCursor = function (query, direction) {
    return openCursor(this, this.name, storeEntries(this.transaction, this.name), query, direction, true);
  };
  IDBObjectStore.prototype.createIndex = function (name, keyPath, options) {
    var tx = this.transaction;
    if (tx.mode !== "versionchange") throw err("InvalidStateError", "createIndex outside an upgrade.");
    var d = tx._data(this.name);
    if (d.indexes[name]) throw err("ConstraintError", "Index " + name + " already exists.");
    d.indexes[name] = { keyPath: keyPath, unique: !!(options && options.unique), multiEntry: !!(options && options.multiEntry) };
    this.indexNames = nameList(Object.keys(d.indexes));
    return new IDBIndex(this, name);
  };
  IDBObjectStore.prototype.deleteIndex = function (name) {
    var tx = this.transaction;
    if (tx.mode !== "versionchange") throw err("InvalidStateError", "deleteIndex outside an upgrade.");
    var d = tx._data(this.name);
    if (!d.indexes[name]) throw err("NotFoundError", "No index " + name);
    delete d.indexes[name];
    this.indexNames = nameList(Object.keys(d.indexes));
  };
  IDBObjectStore.prototype.index = function (name) {
    if (!this.transaction._data(this.name).indexes[name]) throw err("NotFoundError", "No index named " + name);
    return new IDBIndex(this, name);
  };
  // ---------------------------------------------------------------- indexes
  var IDBIndex = function IDBIndex(store, name) {
    var ix = store.transaction._data(store.name).indexes[name];
    this.objectStore = store; this.name = name; this.keyPath = ix.keyPath;
    this.unique = ix.unique; this.multiEntry = ix.multiEntry;
  };
  IDBIndex.prototype._entries = function () {
    var store = this.objectStore, tx = store.transaction, sname = store.name, iname = this.name;
    return function () {
      var d = tx._data(sname), ix = d.indexes[iname], out = [];
      for (var i = 0; i < d.recs.length; i++) {
        var ks = indexKeys(ix, d.recs[i].v);
        for (var j = 0; j < ks.length; j++) out.push({ key: ks[j], pk: d.recs[i].k });
      }
      out.sort(function (a, b) { return cmp(a.key, b.key) || cmp(a.pk, b.pk); });
      return out;
    };
  };
  IDBIndex.prototype._query = function (query, count, what) {
    var list = this._entries(), tx = this.objectStore.transaction, sname = this.objectStore.name, range = toRange(query);
    return tx._request(this, function () {
      var es = entries(list(), range), d = tx._data(sname), out = [];
      for (var i = 0; i < es.length && (!count || out.length < count); i++) {
        if (what === "key") out.push(es[i].pk);
        else { var at = find(d.recs, es[i].pk); out.push(clone(d.recs[at].v)); }
      }
      return out;
    });
  };
  IDBIndex.prototype.get = function (query) {
    var r = this._query(query, 1, "value"), item = this.objectStore.transaction._queue[this.objectStore.transaction._queue.length - 1];
    var op = item.op; item.op = function (req) { return op(req)[0]; };
    return r;
  };
  IDBIndex.prototype.getKey = function (query) {
    var r = this._query(query, 1, "key"), item = this.objectStore.transaction._queue[this.objectStore.transaction._queue.length - 1];
    var op = item.op; item.op = function (req) { return op(req)[0]; };
    return r;
  };
  IDBIndex.prototype.getAll = function (query, count) { return this._query(query, count, "value"); };
  IDBIndex.prototype.getAllKeys = function (query, count) { return this._query(query, count, "key"); };
  IDBIndex.prototype.count = function (query) {
    var r = this._query(query, 0, "key"), item = this.objectStore.transaction._queue[this.objectStore.transaction._queue.length - 1];
    var op = item.op; item.op = function (req) { return op(req).length; };
    return r;
  };
  IDBIndex.prototype.openCursor = function (query, direction) {
    var c = { transaction: this.objectStore.transaction };
    var req = openCursor(c, this.objectStore.name, this._entries(), query, direction, false);
    req.source = this;
    return req;
  };
  IDBIndex.prototype.openKeyCursor = function (query, direction) {
    var c = { transaction: this.objectStore.transaction };
    var req = openCursor(c, this.objectStore.name, this._entries(), query, direction, true);
    req.source = this;
    return req;
  };
  // ---------------------------------------------------------------- databases
  var IDBDatabase = function IDBDatabase(name) {
    this.name = name; this.version = databases[name].version; this._closed = false;
    this.onversionchange = null; this.onclose = null; this.onabort = null; this.onerror = null;
  };
  extend(IDBDatabase, Emitter);
  Object.defineProperty(IDBDatabase.prototype, "objectStoreNames", {
    get: function () { return nameList(databases[this.name] ? Object.keys(databases[this.name].stores) : []); }
  });
  IDBDatabase.prototype.createObjectStore = function (name, options) {
    if (!this._upgrade) throw err("InvalidStateError", "createObjectStore outside an upgrade.");
    var stores = databases[this.name].stores;
    if (stores[name]) throw err("ConstraintError", "Object store " + name + " already exists.");
    var kp = options && options.keyPath !== undefined ? options.keyPath : null;
    if (options && options.autoIncrement && (kp === "" || Array.isArray(kp))) throw err("InvalidAccessError", "autoIncrement needs a simple keyPath.");
    stores[name] = newStoreData(kp, options && options.autoIncrement);
    this._upgrade.objectStoreNames = nameList(Object.keys(stores));
    return new IDBObjectStore(this._upgrade, name);
  };
  IDBDatabase.prototype.deleteObjectStore = function (name) {
    if (!this._upgrade) throw err("InvalidStateError", "deleteObjectStore outside an upgrade.");
    var stores = databases[this.name].stores;
    if (!stores[name]) throw err("NotFoundError", "No object store " + name);
    delete stores[name];
    this._upgrade.objectStoreNames = nameList(Object.keys(stores));
  };
  IDBDatabase.prototype.transaction = function (names, mode) {
    if (this._closed) throw err("InvalidStateError", "The database connection is closing.");
    if (typeof names === "string") names = [names];
    names = Array.prototype.slice.call(names);
    if (!names.length) throw err("InvalidAccessError", "No object store names given.");
    var stores = databases[this.name].stores;
    for (var i = 0; i < names.length; i++) if (!stores[names[i]]) throw err("NotFoundError", "No object store named " + names[i]);
    mode = mode || "readonly";
    if (mode !== "readonly" && mode !== "readwrite") throw new TypeError("Invalid transaction mode " + mode);
    return new IDBTransaction(this, names, mode);
  };
  IDBDatabase.prototype.close = function () { this._closed = true; };
  // ---------------------------------------------------------------- factory
  var IDBFactory = function IDBFactory() {};
  IDBFactory.prototype.open = function (name, version) {
    name = String(name);
    if (version !== undefined && !(version >= 1)) throw new TypeError("IDBFactory.open: version must be a positive integer");
    var req = new IDBOpenDBRequest();
    later(function () {
      var existing = databases[name];
      var old = existing ? existing.version : 0;
      var next = version === undefined ? (old || 1) : Math.floor(version);
      if (next < old) {
        req.readyState = "done"; req.error = err("VersionError", "The requested version (" + next + ") is less than the existing version (" + old + ").");
        req.dispatchEvent(event("error"));
        return;
      }
      if (!existing) databases[name] = existing = { version: 0, stores: {} };
      var db = new IDBDatabase(name);
      if (next === old) {
        req.readyState = "done"; req.result = db;
        req.dispatchEvent(event("success"));
        return;
      }
      var snapshot = { version: old, stores: Object.assign({}, existing.stores) };
      existing.version = next; db.version = next;
      var tx = new IDBTransaction(db, Object.keys(existing.stores), "versionchange");
      db._upgrade = tx;
      req.transaction = tx; req.result = db; req.readyState = "done";
      tx._after = function (e) {
        db._upgrade = null; req.transaction = null;
        if (e) {
          databases[name].version = snapshot.version; databases[name].stores = snapshot.stores;
          if (!snapshot.version) delete databases[name];
          req.result = undefined; req.error = err("AbortError", "The upgrade transaction was aborted.");
          req.dispatchEvent(event("error"));
        } else {
          req.dispatchEvent(event("success"));
        }
      };
      try {
        req.dispatchEvent(event("upgradeneeded", { oldVersion: old, newVersion: next }));
      } catch (e) {
        tx._abort(e);
        return;
      }
      tx._run();
    });
    return req;
  };
  IDBFactory.prototype.deleteDatabase = function (name) {
    var req = new IDBOpenDBRequest();
    later(function () {
      var old = databases[name] ? databases[name].version : 0;
      delete databases[String(name)];
      req.readyState = "done"; req.result = undefined;
      req.dispatchEvent(event("success", { oldVersion: old, newVersion: null }));
    });
    return req;
  };
  IDBFactory.prototype.databases = function () {
    return Promise.resolve(Object.keys(databases).map(function (n) { return { name: n, version: databases[n].version }; }));
  };
  IDBFactory.prototype.cmp = function (a, b) { return cmp(a, b); };
  var factory = new IDBFactory();
  var define = function (name, value) {
    Object.defineProperty(window, name, { value: value, writable: true, configurable: true, enumerable: false });
  };
  define("indexedDB", factory);
  define("IDBFactory", IDBFactory);
  define("IDBDatabase", IDBDatabase);
  define("IDBTransaction", IDBTransaction);
  define("IDBObjectStore", IDBObjectStore);
  define("IDBIndex", IDBIndex);
  define("IDBCursor", IDBCursor);
  define("IDBCursorWithValue", IDBCursorWithValue);
  define("IDBKeyRange", IDBKeyRange);
  define("IDBRequest", IDBRequest);
  define("IDBOpenDBRequest", IDBOpenDBRequest);
}
