#!/bin/sh
# Regenerates THIRD-PARTY-LICENSES.md: every license that ships inside the gitty binary.
# Needs cargo-about (`cargo install --locked cargo-about --features cli`). Run from the repo root.
set -eu
out=THIRD-PARTY-LICENSES.md
onig=$(cargo metadata --format-version 1 | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["manifest_path"] for p in m["packages"] if p["name"]=="onig_sys").rsplit("/",1)[0])')
{
  printf '# Third-party licenses\n\ngitty is MIT licensed (see LICENSE). The binary also contains the software below; the source of each crate is at the linked repository or on crates.io.\n\n'
  cargo about generate --fail about.hbs
  printf '\n## Oniguruma (C library compiled into onig_sys)\n\n```text\n'
  cat "$onig/oniguruma/COPYING"
  printf '```\n\n## Syntax definitions (bundled through two-face, from bat)\n\n'
  cargo run -q -p gitty-highlight --example syntax_licenses
  printf '\n## Colour themes\n\nThe built-in themes reproduce the palettes of these projects:\n\n'
  cat <<'THEMES'
- Catppuccin: https://github.com/catppuccin/catppuccin (MIT)
- Dracula: https://github.com/dracula/dracula-theme (MIT)
- GitHub (dark, light): https://github.com/primer/github-vscode-theme (MIT)
- Gruvbox: https://github.com/morhetz/gruvbox (MIT/X11)
- Rosé Pine: https://github.com/rose-pine/rose-pine-theme (MIT)
- Solarized: https://github.com/altercation/solarized (MIT)
- Tokyo Night: https://github.com/enkia/tokyo-night-vscode-theme (MIT) and https://github.com/folke/tokyonight.nvim (Apache-2.0)
THEMES
} > "$out"
echo "wrote $out"
