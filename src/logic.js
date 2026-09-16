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
