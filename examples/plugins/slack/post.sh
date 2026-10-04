#!/bin/sh
# Posts what happened, $CRYSTAL_EVENT_TEXT, to the Slack incoming webhook
# whose URL is in webhook-url, in the plugin's settings directory:
#
#   echo 'https://hooks.slack.com/services/…' \
#     > ~/.config/crystal/plugin-config/slack/webhook-url
#
# A task that closed done, and a flow run that ended done, aren't news.
set -eu
url_file="$CRYSTAL_PLUGIN_CONFIG_DIR/webhook-url"
if [ ! -s "$url_file" ]; then
  echo "no webhook yet: put its URL in $url_file"
  exit 0
fi
event=$(cat)
case "$CRYSTAL_EVENT" in
  task.closed)
    case "$event" in *'"failed":true'*) ;; *) exit 0 ;; esac ;;
  flow.ended)
    case "$event" in *'"state":"done"'*) exit 0 ;; esac ;;
esac
text=$(printf '%s' "$CRYSTAL_EVENT_TEXT" | sed 's/\\/\\\\/g; s/"/\\"/g')
curl --silent --show-error --fail --max-time 10 \
  -H 'Content-Type: application/json' \
  --data "{\"text\":\"[$CRYSTAL_EVENT] $text\"}" \
  "$(cat "$url_file")"
