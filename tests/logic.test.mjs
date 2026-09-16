import { test } from "node:test";
import assert from "node:assert/strict";
import {
  isActionable,
  fixButtonLabel,
  isBatchable,
  needsConfirmation,
} from "../src/logic.js";

test("only issue and action statuses are actionable", () => {
  assert.equal(isActionable("issue"), true);
  assert.equal(isActionable("action"), true);
  assert.equal(isActionable("ok"), false);
  assert.equal(isActionable("unavailable"), false);
  assert.equal(isActionable("error"), false);
});

test("fix button label reflects how the fix behaves", () => {
  assert.equal(fixButtonLabel("auto"), "Fix");
  assert.equal(fixButtonLabel("guided"), "Open");
  assert.equal(fixButtonLabel("manual"), "Run");
  assert.equal(fixButtonLabel("restore"), "Fix");
});

test("Fix All batches only automatic fixes for real problems", () => {
  assert.equal(isBatchable({ id: "temp_files", fixMode: "auto", status: "issue" }), true);
  // guided/manual are never batched, even when a real problem is present
  assert.equal(isBatchable({ id: "disk_health", fixMode: "guided", status: "issue" }), false);
  assert.equal(isBatchable({ id: "winsock_reset", fixMode: "manual", status: "issue" }), false);
  // non-issue statuses are never auto-run
  assert.equal(isBatchable({ id: "dns_cache", fixMode: "auto", status: "action" }), false);
  assert.equal(isBatchable({ id: "defender_scan", fixMode: "auto", status: "unavailable" }), false);
  assert.equal(isBatchable({ id: "temp_files", fixMode: "auto", status: "ok" }), false);
});

test("restore_point is never batched by Fix All", () => {
  assert.equal(isBatchable({ id: "restore_point", fixMode: "auto", status: "issue" }), false);
  assert.equal(isBatchable({ id: "restore_point", fixMode: "restore", status: "action" }), false);
});

test("isBatchable is safe on missing input", () => {
  assert.equal(isBatchable(null), false);
  assert.equal(isBatchable(undefined), false);
});

test("only manual fixes require confirmation", () => {
  assert.equal(needsConfirmation("manual"), true);
  assert.equal(needsConfirmation("auto"), false);
  assert.equal(needsConfirmation("guided"), false);
  assert.equal(needsConfirmation("restore"), false);
});
