# Example plugins

Plugins to copy, or to install as they are: `crystal plugin install examples/plugins/<name>`, then
`crystal plugin enable <name>`. See [Plugins](../../README.md#plugins) for what a plugin can do.

| Plugin | Hears | Does |
|---|---|---|
| [`event-log`](event-log/) | `*` | Keeps every event as a line of JSON in its state directory; its `follow` pane, split off below, shows them as they come, and `clear` empties it. |
| [`slack`](slack/) | `task.closed`, `flow.gate`, `flow.ended`, `plugin.paused` | Posts `$CRYSTAL_EVENT_TEXT` to a Slack incoming webhook, its URL in `webhook-url` in the plugin's settings directory, with `curl`; a task or a flow run that ended done isn't posted. |
| [`worktree-env`](worktree-env/) | `worktree.created` | Copies the project's `.env` files into each worktree crystal makes. Ship it in a repository's `.crystal/plugins/`, and turn it on there with `crystal plugin enable worktree-env --project`. |

crystal's own tests install each one and run its hooks.
