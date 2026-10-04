#!/bin/sh
# Empties the log; what it printed goes to `crystal plugin log event-log`.
: > "$CRYSTAL_PLUGIN_STATE_DIR/events.jsonl"
echo "cleared the event log"
