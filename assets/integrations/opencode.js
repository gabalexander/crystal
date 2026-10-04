// crystal's plugin for __AGENT__: in a crystal session, it tells crystal what
// __AGENT__ is doing and which of its sessions it's in, so that crystal shows it
// and picks the session up again after a restart. Outside one it does nothing.
// `crystal integration install __AGENT__` wrote this file, and `uninstall` takes
// it out again: it's written again whole, so change nothing here.
// Adapted from herdr's (Apache-2.0).

import { spawn } from "node:child_process";

const CRYSTAL = __CRYSTAL__;
const AGENT = "__AGENT__";

// A subagent runs in a session of its own, a child of the one the user is
// in: what it asks the user is the session's, and the rest is its work.
const children = new Map();
const CHILD_EVENTS = new Map([
  ["permission.asked", "PermissionRequest"],
  ["question.asked", "PermissionRequest"],
  ["permission.replied", "PostToolUse"],
  ["question.replied", "PostToolUse"],
  ["question.rejected", "PostToolUse"],
]);

const EVENTS = new Map([
  ["tool.execute.before", "PostToolUse"],
  ["tool.execute.after", "PostToolUse"],
  ["permission.replied", "PostToolUse"],
  ["question.replied", "PostToolUse"],
  ["question.rejected", "PostToolUse"],
  ["session.compacted", "PostToolUse"],
  ["permission.asked", "PermissionRequest"],
  ["permission.updated", "PermissionRequest"],
  ["question.asked", "PermissionRequest"],
  ["session.error", "PermissionRequest"],
  ["session.idle", "Stop"],
]);

const WORKING = new Set(["active", "busy", "pending", "retry", "running", "streaming", "working"]);

// What crystal was told last, so that it's told only what changed, a
// report at a time, in order.
let told;
let telling = Promise.resolve();

function tell(event, sessionID) {
  if (!sessionID) return;
  const said = `${event} ${sessionID}`;
  if (said === told) return;
  told = said;
  const input = JSON.stringify({ session_id: sessionID });
  telling = telling.then(
    () =>
      new Promise((resolve) => {
        const child = spawn(CRYSTAL, ["hook", AGENT, "--event", event], {
          stdio: ["pipe", "ignore", "ignore"],
        });
        child.on("error", resolve);
        child.on("close", resolve);
        child.stdin.on("error", () => {});
        child.stdin.end(input);
      }),
  );
}

function rootOf(sessionID) {
  const seen = new Set();
  while (children.has(sessionID) && !seen.has(sessionID)) {
    seen.add(sessionID);
    sessionID = children.get(sessionID);
  }
  return sessionID;
}

export const CrystalPlugin = async () => {
  if (!process.env.CRYSTAL_SESSION) {
    return {};
  }
  return {
    "chat.message": async ({ sessionID }) => {
      if (sessionID && !children.has(sessionID)) {
        tell("UserPromptSubmit", sessionID);
      }
    },
    event: async ({ event }) => {
      const type = event?.type;
      const properties = event?.properties ?? {};
      const info = properties.info;
      if (info?.id && info.parentID) {
        children.set(info.id, info.parentID);
      }
      const sessionID =
        typeof properties.sessionID === "string" && properties.sessionID
          ? properties.sessionID
          : info?.id;
      if (sessionID && children.has(sessionID)) {
        const said = CHILD_EVENTS.get(type);
        if (said) tell(said, rootOf(sessionID));
        return;
      }
      if (type === "session.status") {
        const status = properties.status;
        const kind = typeof status === "string" ? status : status?.type;
        if (typeof kind !== "string") return;
        if (kind.toLowerCase() === "idle") tell("Stop", sessionID);
        else if (WORKING.has(kind.toLowerCase())) tell("UserPromptSubmit", sessionID);
        return;
      }
      if (type === "session.updated" && sessionID && !told?.endsWith(` ${sessionID}`)) {
        tell("SessionNamed", sessionID);
        return;
      }
      const said = EVENTS.get(type);
      if (said) tell(said, sessionID);
    },
  };
};
