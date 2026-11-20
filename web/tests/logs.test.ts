// Unit tests for the log parser: node --test tests/logs.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { guessColumns, parseTime, readTable, toArrays } from "../src/lib/logs.ts";

test("timestamps in every supported form", () => {
  assert.equal(parseTime("1700000000"), 1_700_000_000_000);
  assert.equal(parseTime("1700000000123"), 1_700_000_000_123);
  assert.equal(parseTime("2026-01-15T09:00:00.500Z"), Date.UTC(2026, 0, 15, 9, 0, 0, 500));
  assert.equal(parseTime("10/Oct/2000:13:55:36 -0700"), Date.UTC(2000, 9, 10, 20, 55, 36));
  assert.ok(Number.isNaN(parseTime("not a time")));
});

test("CSV with a header, quoted fields and column guesses", () => {
  const t = readTable('timestamp,api_key,path\n1700000000,"k,1",/a\n1700000001,k2,/b\n');
  assert.equal(t.format, "csv");
  assert.deepEqual(t.rows[0], ["1700000000", "k,1", "/a"]);
  assert.deepEqual(guessColumns(t), { time: 0, key: 1, cost: -1 });
  const p = toArrays(t, 0, 1, -1);
  assert.deepEqual([...p.keys], [0, 1]);
  assert.deepEqual(p.keyNames, ["k,1", "k2"]);
});

test("JSON Lines", () => {
  const t = readTable('{"ts": 1700000000, "user": "a", "tokens": 3}\n{"ts": 1700000002, "user": "b"}\n');
  assert.equal(t.format, "jsonl");
  const g = guessColumns(t);
  const p = toArrays(t, g.time, g.key, g.cost);
  assert.deepEqual([...p.costs], [3, 1]);
});

test("nginx combined log", () => {
  const line = '203.0.113.9 - - [10/Oct/2000:13:55:36 -0700] "GET /a HTTP/1.1" 200 2326 "-" "curl/8"';
  const t = readTable(`${line}\n${line}\n`);
  assert.equal(t.format, "combined");
  const g = guessColumns(t);
  assert.equal(t.columns[g.key], "ip");
  assert.equal(t.columns[g.time], "time");
  const p = toArrays(t, g.time, g.key, g.cost);
  assert.equal(p.atMs.length, 2);
});

test("rows without a readable time are skipped and counted", () => {
  const t = readTable("time,key\n1700000000,a\nnope,b\n");
  const p = toArrays(t, 0, 1, -1);
  assert.equal(p.atMs.length, 1);
  assert.equal(p.skipped, 1);
});
