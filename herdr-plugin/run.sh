#!/bin/sh
# The gitty popup: runs gitty on GITTY_REPO. Uses an installed gitty (Homebrew, cargo,
# the shell installer); without one, downloads the release archive into the plugin's
# state directory once, checks it against the published SHA-256, and uses that copy.
set -u

repo="${GITTY_REPO:-$PWD}"
own="${HERDR_PLUGIN_STATE_DIR:-$HOME/.local/state/herdr-gitty}/bin"

find_gitty() {
  for dir in $(printf '%s' "${PATH:-}" | tr ':' ' ') /opt/homebrew/bin /usr/local/bin \
    /home/linuxbrew/.linuxbrew/bin "$HOME/.cargo/bin" "$HOME/.local/bin" "$own"; do
    if [ -x "$dir/gitty" ]; then
      printf '%s\n' "$dir/gitty"
      return 0
    fi
  done
  return 1
}

pause() {
  printf '\nPress Enter to close.'
  read -r _ || true
}

# The release archive for this machine, checked against its published SHA-256.
download() {
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    *) echo "No gitty release for $(uname -s) $(uname -m)."; return 1 ;;
  esac
  archive="gitty-cli-$target.tar.xz"
  base=https://github.com/VedangP57/gitty/releases/latest/download
  tmp=$(mktemp -d) || return 1
  if ! curl --proto '=https' --tlsv1.2 -fLsS "$base/$archive" -o "$tmp/$archive" ||
    ! curl --proto '=https' --tlsv1.2 -fLsS "$base/$archive.sha256" -o "$tmp/sum"; then
    rm -rf "$tmp"
    return 1
  fi
  want=$(cut -d' ' -f1 <"$tmp/sum")
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$archive" | cut -d' ' -f1)
  else
    got=$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)
  fi
  if [ -z "$want" ] || [ "$got" != "$want" ]; then
    echo "The download does not match its published SHA-256; not using it."
    rm -rf "$tmp"
    return 1
  fi
  tar -xJf "$tmp/$archive" -C "$tmp" && mkdir -p "$own" &&
    mv "$tmp/gitty-cli-$target/gitty" "$own/gitty" && chmod +x "$own/gitty"
  ok=$?
  rm -rf "$tmp"
  return "$ok"
}

if ! gitty=$(find_gitty); then
  echo "gitty is not installed. Downloading the latest release into"
  echo "  $own"
  echo "(nothing outside that folder changes; or: brew install vedangp57/tap/gitty)"
  if ! download; then
    echo
    echo "The download failed. Install gitty yourself (see github.com/VedangP57/gitty)."
    pause
    exit 1
  fi
  echo "Verified and installed."
  gitty="$own/gitty"
fi

cd "$repo" 2>/dev/null || cd "$HOME" || exit 1
"$gitty" "$repo"
status=$?
# gitty prints why it could not start (not a git repository, git too old) and exits;
# keep the popup open long enough to read it
if [ "$status" -ne 0 ]; then
  pause
fi
exit "$status"
