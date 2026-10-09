#!/bin/sh
# Regenerates THIRD-PARTY-LICENSES.md: every license that ships inside the gitty binary.
# Needs cargo-about (`cargo install --locked cargo-about --features cli`) and python3. Run from
# the repo root.
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
- Ayu: https://github.com/ayu-theme/ayu-colors (MIT, Copyright (c) Konstantin Pschera)
- Catppuccin: https://github.com/catppuccin/catppuccin (MIT, Copyright (c) 2021 Catppuccin)
- Dracula: https://github.com/dracula/dracula-theme (MIT)
- Everforest: https://github.com/sainnhe/everforest (MIT, Copyright (c) 2019 sainnhe)
- GitHub (dark, light): https://github.com/primer/github-vscode-theme (MIT)
- Gruvbox (dark, light): https://github.com/morhetz/gruvbox (MIT/X11)
- Kanagawa: https://github.com/rebelot/kanagawa.nvim (MIT, Copyright (c) 2021 Tommaso Laurenzi)
- Nightfox: https://github.com/EdenEast/nightfox.nvim (MIT, Copyright (c) 2021 James Simpson)
- Nord: https://github.com/nordtheme/nord (MIT, Copyright (c) 2016-present Sven Greb)
- One Dark, One Light: https://github.com/atom/one-dark-syntax and https://github.com/atom/one-light-syntax (MIT, Copyright (c) 2016 GitHub Inc.), with the dark UI greys from https://github.com/joshdick/onedark.vim (MIT, Copyright (c) 2015 Joshua Dick)
- Rosé Pine: https://github.com/rose-pine/rose-pine-theme (MIT)
- Solarized: https://github.com/altercation/solarized (MIT)
- Tokyo Night (night, storm, day): https://github.com/enkia/tokyo-night-vscode-theme (MIT) and https://github.com/folke/tokyonight.nvim (Apache-2.0)
THEMES
  cat <<'MITTEXT'

The projects marked MIT (or MIT/X11) license their palettes under these terms, with the copyright line shown for each:

```text
Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```
MITTEXT
  printf '\n## Conflict markers\n\nConflict-marker parsing logic adapted from druk (https://github.com/letstri/druk), MIT License, Copyright (c) Valerii Strilets:\n\n```text\nMIT License\n\nCopyright (c) Valerii Strilets\n\n'
  cat <<'DRUKTEXT'
Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```
DRUKTEXT
} > "$out"
echo "wrote $out"
