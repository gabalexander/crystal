"""crystal's plugin for Hermes Agent: in a crystal session, it tells crystal
which Hermes session it's in, so that crystal picks it up again after a
restart. Outside one it does nothing. What Hermes is doing is read off its
screen. `crystal integration install hermes` wrote this file, and
`uninstall` takes it out again: it's written again whole, so change nothing
here. Adapted from herdr's (Apache-2.0)."""

from __future__ import annotations

import json
import os
import subprocess

_CRYSTAL = __CRYSTAL__
# Hermes's own screens: its other platforms run elsewhere.
_INTERACTIVE = {"cli", "tui", "desktop", "acp"}
# The session crystal was told of last.
_told = None


def _tell(event: str, **kwargs) -> None:
    global _told
    if not os.environ.get("CRYSTAL_SESSION"):
        return
    if kwargs.get("platform") not in _INTERACTIVE:
        return
    session_id = kwargs.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        return
    # Each call of its model names the session: only a new one is news.
    if event == "SessionNamed" and session_id == _told:
        return
    _told = session_id
    try:
        subprocess.run(
            [_CRYSTAL, "hook", "hermes", "--event", event],
            input=json.dumps({"session_id": session_id}).encode(),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
    except Exception:
        pass


def _session_started(**kwargs) -> None:
    _tell("SessionStart", **kwargs)


def _session_named(**kwargs) -> None:
    _tell("SessionNamed", **kwargs)


def register(ctx):
    ctx.register_hook("on_session_start", _session_started)
    ctx.register_hook("on_session_reset", _session_started)
    # As it calls its model: the session it's in may have changed.
    ctx.register_hook("pre_llm_call", _session_named)
