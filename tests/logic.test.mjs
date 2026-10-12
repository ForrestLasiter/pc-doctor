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

import { formatMB, updateBannerView } from "../src/logic.js";

const info = { version: "0.9.0", current: "0.8.0", size: 1401924, notes: "Notes", can_install: true };

test("formatMB shows one decimal", () => {
  assert.equal(formatMB(1401924), "1.3 MB");
});

test("update banner offers the install only when an update is available", () => {
  const available = updateBannerView("available", info);
  assert.equal(available.showInstall, true);
  assert.equal(available.installEnabled, true);
  assert.equal(available.dismissLabel, "Not now");
  assert.match(available.title, /0\.9\.0/);
  assert.equal(available.notes, "Notes");

  const current = updateBannerView("current", { ...info, version: "0.8.0" });
  assert.equal(current.showInstall, false);
  assert.match(current.title, /up to date/);
});

test("update banner cannot install from a source copy", () => {
  const view = updateBannerView("available", { ...info, can_install: false });
  assert.equal(view.showInstall, true);
  assert.equal(view.installEnabled, false);
});

test("update banner locks while downloading and reports errors", () => {
  const downloading = updateBannerView("downloading", info);
  assert.equal(downloading.busy, true);
  assert.equal(downloading.installEnabled, false);
  assert.equal(downloading.canDismiss, false);

  const error = updateBannerView("error", null, "Could not reach GitHub.");
  assert.equal(error.error, true);
  assert.equal(error.detail, "Could not reach GitHub.");
  assert.equal(error.showInstall, false);
});
