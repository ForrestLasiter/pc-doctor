const { invoke } = window.__TAURI__.core;

let scanBtn;
let fixAllBtn;
let historyBtn;
let closeHistoryBtn;
let statusLine;
let checkList;
let historyPanel;
let historyContent;
let summaryBar;
let summaryOkCount;
let summaryIssueCount;

let restorePointDone = false;

const ICONS = {
  restore_point: "🛟",
  temp_files: "🗑️",
  disk_space: "💾",
  dns_cache: "🌐",
  windows_updates: "🪟",
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
};

function renderChecks(checks) {
  checkList.innerHTML = "";
  for (const check of checks) {
    const li = document.createElement("li");
    li.className = "check-item";
    li.dataset.id = check.id;
    const icon = ICONS[check.id] || "🔧";
    li.innerHTML = `
      <div class="check-row">
        <div class="check-icon">${icon}</div>
        <div class="check-info">
          <span class="check-name">${check.name}</span>
          <span class="check-desc">${check.description}</span>
          <div class="progress-track hidden"><div class="progress-bar"></div></div>
          <span class="check-detail"></span>
        </div>
        <div class="check-actions">
          <span class="check-badge">pending</span>
          <button class="fix-btn" disabled>Fix</button>
        </div>
      </div>
    `;
    checkList.appendChild(li);
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

async function ensureRestorePoint() {
  if (restorePointDone) return;
  restorePointDone = true;
  statusLine.textContent = "Creating a restore point before making changes...";
  const result = await invoke("fix_check", { id: "restore_point" });
  await logEvent(`Restore point: ${result.output}`);

  const li = checkList.querySelector('[data-id="restore_point"]');
  if (li) {
    li.querySelector(".check-detail").textContent = result.output;
    setBadge(li, result.success ? "fixed" : "error");
    li.querySelector(".fix-btn").disabled = true;
    updateSummary();
  }
}

async function scanAll() {
  scanBtn.disabled = true;
  fixAllBtn.disabled = true;
  restorePointDone = false;
  statusLine.textContent = "Scanning...";
  summaryBar.classList.add("hidden");
  const checks = await invoke("list_checks");
  renderChecks(checks);
  await logEvent("Scan started.");

  let issueCount = 0;

  for (const check of checks) {
    const li = checkList.querySelector(`[data-id="${check.id}"]`);
    const detailEl = li.querySelector(".check-detail");
    setBadge(li, "scanning");
    try {
      const result = await invoke("scan_check", { id: check.id });
      detailEl.textContent = result.detail;
      setBadge(li, result.status);
      const fixBtn = li.querySelector(".fix-btn");
      if (result.status === "issue") {
        fixBtn.disabled = false;
        issueCount++;
      }
      fixBtn.addEventListener("click", () => runFix(check.id, li));
    } catch (e) {
      detailEl.textContent = String(e);
      setBadge(li, "error");
    }
    updateSummary();
  }

  await logEvent(`Scan complete. ${issueCount} issue(s) found.`);
  statusLine.textContent = `Scan complete. ${issueCount} issue(s) found.`;
  scanBtn.disabled = false;
  fixAllBtn.disabled = issueCount === 0;
}

async function runFix(id, li) {
  if (id !== "restore_point") {
    await ensureRestorePoint();
  }
  const fixBtn = li.querySelector(".fix-btn");
  const detailEl = li.querySelector(".check-detail");
  fixBtn.disabled = true;
  fixBtn.textContent = "Fixing...";
  setBadge(li, "fixing");
  try {
    const result = await invoke("fix_check", { id });
    detailEl.textContent = result.output || (result.success ? "Fixed." : "Fix failed.");
    setBadge(li, result.success ? "fixed" : "error");
    await logEvent(`${id}: ${result.output}`);
  } catch (e) {
    detailEl.textContent = String(e);
    setBadge(li, "error");
    await logEvent(`${id}: error - ${String(e)}`);
  }
  fixBtn.textContent = "Fix";
  updateSummary();
}

async function fixAll() {
  fixAllBtn.disabled = true;
  scanBtn.disabled = true;
  await ensureRestorePoint();

  const items = checkList.querySelectorAll(".check-item");
  for (const li of items) {
    const id = li.dataset.id;
    if (id === "restore_point") continue;
    const badge = li.querySelector(".check-badge");
    if (!badge.classList.contains("badge-issue")) continue;
    statusLine.textContent = `Fixing: ${li.querySelector(".check-name").textContent}...`;
    await runFix(id, li);
  }

  statusLine.textContent = "Fix All complete.";
  await logEvent("Fix All complete.");
  scanBtn.disabled = false;
  fixAllBtn.disabled = true;
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
  historyBtn = document.querySelector("#history-btn");
  closeHistoryBtn = document.querySelector("#close-history-btn");
  statusLine = document.querySelector("#status-line");
  checkList = document.querySelector("#check-list");
  historyPanel = document.querySelector("#history-panel");
  historyContent = document.querySelector("#history-content");
  summaryBar = document.querySelector("#summary-bar");
  summaryOkCount = document.querySelector("#summary-ok-count");
  summaryIssueCount = document.querySelector("#summary-issue-count");

  scanBtn.addEventListener("click", scanAll);
  fixAllBtn.addEventListener("click", fixAll);
  historyBtn.addEventListener("click", showHistory);
  closeHistoryBtn.addEventListener("click", hideHistory);
});
