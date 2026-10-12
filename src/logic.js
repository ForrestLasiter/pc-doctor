// Pure UI-decision helpers, split out from main.js so they can be unit-tested
// in Node without a DOM or the Tauri bridge. No side effects, no globals.

// Scan statuses whose Fix button should be clickable.
export const ACTIONABLE = new Set(["issue", "action"]);

export function isActionable(status) {
  return ACTIONABLE.has(status);
}

// The Fix button's label depends on how the fix behaves.
export function fixButtonLabel(fixMode) {
  if (fixMode === "guided") return "Open";
  if (fixMode === "manual") return "Run";
  return "Fix";
}

// "Fix All" runs a check only when it's an automatic fix for a real problem.
// Guided (opens a window) and manual (disruptive) actions are never batched,
// and non-"issue" statuses (action/ok/unavailable/error) are never auto-run.
export function isBatchable(check) {
  if (!check) return false;
  if (check.id === "restore_point") return false;
  return check.fixMode === "auto" && check.status === "issue";
}

// Whether an individual fix needs an explicit confirmation before running.
export function needsConfirmation(fixMode) {
  return fixMode === "manual";
}

export function formatMB(bytes) {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

// What the update banner shows for each step of checking and installing.
// phase: checking | current | available | downloading | error
export function updateBannerView(phase, info, message) {
  const view = {
    title: "",
    detail: "",
    notes: "",
    showInstall: false,
    installEnabled: false,
    busy: false,
    error: false,
    dismissLabel: "Close",
    canDismiss: true,
  };
  if (phase === "checking") {
    view.title = "Checking for updates...";
    view.busy = true;
  } else if (phase === "current") {
    view.title = "PC Doctor is up to date.";
    view.detail = `You have version ${info.current}, which is the latest.`;
  } else if (phase === "available" || phase === "downloading") {
    view.title = `Version ${info.version} is available.`;
    view.detail = `You have ${info.current}. The download is ${formatMB(info.size)}.`;
    view.notes = info.notes || "";
    view.showInstall = true;
    if (phase === "downloading") {
      view.detail = `Downloading and checking version ${info.version}. PC Doctor will close when the installer starts.`;
      view.busy = true;
      view.canDismiss = false;
    } else if (info.can_install) {
      view.installEnabled = true;
      view.dismissLabel = "Not now";
    } else {
      view.detail += " This copy is running from source, so it cannot install over itself.";
    }
  } else if (phase === "error") {
    view.title = "Could not update.";
    view.detail = message || "Something went wrong. Try again.";
    view.error = true;
  }
  return view;
}
