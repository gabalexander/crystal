// crystal's extension for Pi: in a crystal session, it tells crystal what Pi
// is doing and which session file it's in, so that crystal shows it and picks
// the session up again after a restart. Outside one it does nothing.
// `crystal integration install pi` wrote this file, and `uninstall` takes it
// out again: it's written again whole, so change nothing here.
// Adapted from herdr's (Apache-2.0).
// @ts-nocheck

import { spawn } from "node:child_process";
import path from "node:path";

const CRYSTAL = __CRYSTAL__;

// A report at a time, in order.
let telling = Promise.resolve();

// The session as Pi resumes it: its file, or else its id.
function sessionOf(ctx) {
  try {
    const file = ctx?.sessionManager?.getSessionFile?.();
    if (typeof file === "string" && path.isAbsolute(file)) {
      return { session_id: file, transcript_path: file };
    }
  } catch {}
  try {
    const id = ctx?.sessionManager?.getSessionId?.();
    if (typeof id === "string" && id) return { session_id: id };
  } catch {}
  return {};
}

function tell(event, ctx) {
  const input = JSON.stringify(sessionOf(ctx));
  telling = telling.then(
    () =>
      new Promise((resolve) => {
        const child = spawn(CRYSTAL, ["hook", "pi", "--event", event], {
          stdio: ["pipe", "ignore", "ignore"],
        });
        child.on("error", resolve);
        child.on("close", resolve);
        child.stdin.on("error", () => {});
        child.stdin.end(input);
      }),
  );
  return telling;
}

export default function (pi) {
  if (!process.env.CRYSTAL_SESSION) {
    return;
  }
  // Only Pi's own screen: its other modes run headless, with nothing to show.
  let shown = false;

  pi.on("session_start", async (_event, ctx) => {
    shown = ctx?.mode === "tui";
    if (!shown) return;
    await tell("SessionStart", ctx);
    // An extension loaded again mid-turn hears no start of it.
    if (ctx?.isIdle?.() === false) await tell("UserPromptSubmit", ctx);
  });

  pi.on("agent_start", (_event, ctx) => {
    if (shown) void tell("UserPromptSubmit", ctx);
  });

  pi.on("agent_settled", (_event, ctx) => {
    if (shown && ctx?.isIdle?.() === true) void tell("Stop", ctx);
  });
}
