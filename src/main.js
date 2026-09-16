import { isActionable, fixButtonLabel, needsConfirmation } from "./logic.js";

const { invoke } = window.__TAURI__.core;

let scanBtn;
let fixAllBtn;
let updateBtn;
let historyBtn;
let closeHistoryBtn;
let statusLine;
let checkList;
let historyPanel;
let historyContent;
let summaryBar;
let summaryOkCount;
let summaryIssueCount;
let elevationBanner;

// null = not created yet, true = created OK, false = attempted but failed.
let restorePointState = null;

const ICONS = {
  restore_point: "🛟",
  temp_files: "🗑️",
  disk_space: "💾",
  dns_cache: "🌐",
  windows_update_service: "⚙️",
  print_spooler: "🖨️",
  winsock_reset: "🔌",
  windows_update_cache: "🧹",
  defender_scan: "🛡️",
  startup_programs: "🚀",
  steam_cache: "🎮",
  steam_reset: "🔁",
  epic_cache: "🧩",
  epic_launcher_reset: "♻️",
  system_file_check: "🩹",
  gpu_shader_cache: "🎨",
  gpu_driver_health: "🖥️",
  gpu_restart_driver: "🔄",
  disk_health: "💽",
  device_problems: "🧰",
  battery_health: "🔋",
  memory_check: "🧠",
};

// Extra warning shown before running a disruptive (fix_mode="manual") fix.
const MANUAL_WARNINGS = {
  winsock_reset:
    "This resets your network stack (Winsock). Your internet connection may drop and you'll need to restart your PC afterward.",
  gpu_restart_driver:
    "This restarts your graphics driver. Your screen will go black for a second or two and any full-screen game or video may close.",
};

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

function renderChecks(checks) {
  checkList.replaceChildren();
  for (const check of checks) {
    const li = el("li", "check-item");
    li.dataset.id = check.id;
    li.dataset.fixMode = check.fix_mode;

    const row = el("div", "check-row");

    const icon = el("div", "check-icon", ICONS[check.id] || "🔧");

    const info = el("div", "check-info");
    info.append(
      el("span", "check-name", check.name),
      el("span", "check-desc", check.description)
    );
    const track = el("div", "progress-track hidden");
    track.append(el("div", "progress-bar"));
    info.append(track, el("span", "check-detail"));

    const actions = el("div", "check-actions");
    const badge = el("span", "check-badge", "pending");
    const fixBtn = el("button", "fix-btn", fixButtonLabel(check.fix_mode));
    fixBtn.disabled = true;
    fixBtn.addEventListener("click", () => runFix(check.id, li));
    actions.append(badge, fixBtn);

    row.append(icon, info, actions);
    li.append(row);
    checkList.append(li);
  }
}

function setBadge(li, status) {
  const badge = li.querySelector(".check-badge");
  badge.textContent = status;
  badge.className = `check-badge badge-${status}`;

  const track = li.querySelector(".progress-track");
  const busy = status === "scanning" || status === "fixing";
  track.classList.toggle("hidden", !busy);
}

function updateSummary() {
  const items = checkList.querySelectorAll(".check-item");
  let ok = 0;
  let issue = 0;
  for (const li of items) {
    const badge = li.querySelector(".check-badge");
    if (badge.classList.contains("badge-ok") || badge.classList.contains("badge-fixed")) ok++;
    if (badge.classList.contains("badge-issue")) issue++;
  }
  summaryOkCount.textContent = ok;
  summaryIssueCount.textContent = issue;
  summaryBar.classList.remove("hidden");
}

async function logEvent(text) {
  try {
    await invoke("log_event", { text });
  } catch {
    // best-effort logging only
  }
}

// Creates a restore point once per session. Returns true only after a
// confirmed success, so callers can decide whether it's safe to continue.
// Retries on a later call if a previous attempt failed.
async function ensureRestorePoint() {
  if (restorePointState === true) return true;

  statusLine.textContent = "Creating a restore point before making changes...";
  const li = checkList.querySelector('[data-id="restore_point"]');
  if (li) setBadge(li, "fixing");

  let result;
  try {
    result = await invoke("fix_check", { id: "restore_point" });
  } catch (e) {
    result = { success: false, output: String(e) };
  }
  await logEvent(`Restore point: ${result.output}`);

  restorePointState = result.success;

  if (li) {
    li.querySelector(".check-detail").textContent = result.output;
    setBadge(li, result.success ? "fixed" : "error");
    li.querySelector(".fix-btn").disabled = result.success;
    updateSummary();
  }
  return result.success;
}

const SCAN_CONCURRENCY = 5;

async function runWithConcurrency(items, limit, worker) {
  let nextIndex = 0;
  async function runNext() {
    while (nextIndex < items.length) {
      const item = items[nextIndex++];
      await worker(item);
    }
  }
  const lanes = Array.from({ length: Math.min(limit, items.length) }, runNext);
  await Promise.all(lanes);
}

async function scanAll() {
  scanBtn.disabled = true;
  fixAllBtn.disabled = true;
  restorePointState = null;
  statusLine.textContent = "Scanning...";
  summaryBar.classList.add("hidden");
  const checks = await invoke("list_checks");
  renderChecks(checks);
  await logEvent("Scan started.");

  let issueCount = 0;

  await runWithConcurrency(checks, SCAN_CONCURRENCY, async (check) => {
    const li = checkList.querySelector(`[data-id="${check.id}"]`);
    const detailEl = li.querySelector(".check-detail");
    setBadge(li, "scanning");
    try {
      const result = await invoke("scan_check", { id: check.id });
      detailEl.textContent = result.detail;
      setBadge(li, result.status);
      const fixBtn = li.querySelector(".fix-btn");
      if (isActionable(result.status)) {
        fixBtn.disabled = false;
        if (result.status === "issue") issueCount++;
      } else {
        fixBtn.disabled = true;
      }
    } catch (e) {
      detailEl.textContent = String(e);
      setBadge(li, "error");
    }
    updateSummary();
  });

  await logEvent(`Scan complete. ${issueCount} issue(s) found.`);
  statusLine.textContent =
    issueCount === 0
      ? "Scan complete. No problems found."
      : `Scan complete. ${issueCount} issue(s) found. "Fix All" repairs the automatic ones; open the others individually.`;
  scanBtn.disabled = false;
  fixAllBtn.disabled = issueCount === 0;
}

async function runFix(id, li) {
  const fixMode = li.dataset.fixMode;

  // Disruptive fixes require an explicit confirmation every time.
  if (needsConfirmation(fixMode)) {
    const warning = MANUAL_WARNINGS[id] || "This is a disruptive action.";
    if (!window.confirm(`${warning}\n\nContinue?`)) {
      return;
    }
  }

  // Auto fixes get a restore point first. Guided/manual actions don't change
  // system files the way auto fixes do, so they don't gate on it — but if a
  // restore point was requested and failed, warn before an auto fix proceeds.
  if (fixMode === "auto") {
    const ok = await ensureRestorePoint();
    if (!ok) {
      const proceed = window.confirm(
        "A system restore point could not be created, so this change can't be automatically undone.\n\nRun the fix anyway?"
      );
      if (!proceed) {
        statusLine.textContent = "Fix cancelled — no restore point.";
        return;
      }
    }
  }

  const fixBtn = li.querySelector(".fix-btn");
  const detailEl = li.querySelector(".check-detail");
  const label = fixBtn.textContent;
  fixBtn.disabled = true;
  fixBtn.textContent = "Working...";
  setBadge(li, "fixing");
  try {
    const result = await invoke("fix_check", { id });
    detailEl.textContent = result.output || (result.success ? "Done." : "Fix failed.");
    if (result.success) {
      setBadge(li, "fixed");
      fixBtn.disabled = true;
    } else {
      // Leave the fix retryable on failure.
      setBadge(li, "error");
      fixBtn.disabled = false;
    }
    await logEvent(`${id}: ${result.output}`);
  } catch (e) {
    detailEl.textContent = String(e);
    setBadge(li, "error");
    fixBtn.disabled = false;
    await logEvent(`${id}: error - ${String(e)}`);
  }
  fixBtn.textContent = label;
  updateSummary();
}

async function fixAll() {
  fixAllBtn.disabled = true;
  scanBtn.disabled = true;
  const rpOk = await ensureRestorePoint();
  if (!rpOk) {
    const proceed = window.confirm(
      "A system restore point could not be created, so these changes can't be automatically undone.\n\nRun the automatic fixes anyway?"
    );
    if (!proceed) {
      statusLine.textContent = "Fix All cancelled — no restore point.";
      scanBtn.disabled = false;
      fixAllBtn.disabled = false;
      return;
    }
  }

  // Only run automatic fixes for real problems. Guided (opens a window) and
  // manual (disruptive) actions are never batched — the user runs those one at
  // a time from their own Fix button.
  const items = checkList.querySelectorAll(".check-item");
  let fixedCount = 0;
  for (const li of items) {
    const id = li.dataset.id;
    if (id === "restore_point") continue;
    if (li.dataset.fixMode !== "auto") continue;
    const badge = li.querySelector(".check-badge");
    if (!badge.classList.contains("badge-issue")) continue;
    statusLine.textContent = `Fixing: ${li.querySelector(".check-name").textContent}...`;
    await runFix(id, li);
    if (li.querySelector(".check-badge").classList.contains("badge-fixed")) fixedCount++;
  }

  statusLine.textContent = `Fix All complete. Repaired ${fixedCount} item(s). Any manual or guided actions are still available individually.`;
  await logEvent(`Fix All complete. Repaired ${fixedCount} item(s).`);
  scanBtn.disabled = false;
  fixAllBtn.disabled = true;
}

async function runWindowsUpdate() {
  updateBtn.disabled = true;
  const label = updateBtn.textContent;
  updateBtn.textContent = "Checking...";
  statusLine.textContent = "Asking Windows Update to look for updates...";
  try {
    const res = await invoke("trigger_windows_update");
    statusLine.textContent = res.output;
    await logEvent(`Windows Update: ${res.output}`);
  } catch (e) {
    statusLine.textContent = `Windows Update: ${String(e)}`;
  }
  updateBtn.textContent = label;
  updateBtn.disabled = false;
}

async function checkElevation() {
  let elevated = false;
  try {
    elevated = await invoke("check_elevation");
  } catch {
    elevated = false;
  }
  elevationBanner.classList.toggle("hidden", elevated);
}

async function showHistory() {
  const content = await invoke("get_history");
  historyContent.textContent = content || "No history yet.";
  historyPanel.classList.remove("hidden");
}

function hideHistory() {
  historyPanel.classList.add("hidden");
}

window.addEventListener("DOMContentLoaded", () => {
  scanBtn = document.querySelector("#scan-btn");
  fixAllBtn = document.querySelector("#fix-all-btn");
  updateBtn = document.querySelector("#update-btn");
  historyBtn = document.querySelector("#history-btn");
  closeHistoryBtn = document.querySelector("#close-history-btn");
  statusLine = document.querySelector("#status-line");
  checkList = document.querySelector("#check-list");
  historyPanel = document.querySelector("#history-panel");
  historyContent = document.querySelector("#history-content");
  summaryBar = document.querySelector("#summary-bar");
  summaryOkCount = document.querySelector("#summary-ok-count");
  summaryIssueCount = document.querySelector("#summary-issue-count");
  elevationBanner = document.querySelector("#elevation-banner");

  scanBtn.addEventListener("click", scanAll);
  fixAllBtn.addEventListener("click", fixAll);
  if (updateBtn) updateBtn.addEventListener("click", runWindowsUpdate);
  historyBtn.addEventListener("click", showHistory);
  closeHistoryBtn.addEventListener("click", hideHistory);

  checkElevation();
});
