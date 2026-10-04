#!/bin/sh
# Shows the last events, and each new one as it's logged, until the pane
# closes.
log="$CRYSTAL_PLUGIN_STATE_DIR/events.jsonl"
touch "$log"
exec tail -n 20 -f "$log"
