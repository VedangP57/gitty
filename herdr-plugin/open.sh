#!/bin/sh
# The "Open gitty" action: opens the gitty popup on the folder of the pane you were in.
set -eu

herdr="${HERDR_BIN_PATH:-herdr}"
context="${HERDR_PLUGIN_CONTEXT_JSON:-}"

# A JSON string field, read without jq. Folder paths in herdr's context are plain
# strings; one with a quote or backslash in its name falls back to the next field.
field() {
  printf '%s' "$context" | sed -n "s/.*\"$1\":\"\\([^\"\\\\]*\\)\".*/\\1/p"
}

repo=$(field focused_pane_cwd)
[ -n "$repo" ] || repo=$(field workspace_cwd)
[ -n "$repo" ] || repo="$HOME"

# the popup starts in the plugin's folder, where run.sh is; the repo travels in GITTY_REPO
exec "$herdr" plugin pane open \
  --plugin vedangp57.gitty \
  --entrypoint gitty \
  --env "GITTY_REPO=$repo" \
  --focus
