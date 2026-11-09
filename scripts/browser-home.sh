#!/bin/bash
# Point this branch's home at the installed app's layouts and personal mods.
# Shipped mods stay copies: starting this build replaces those, and the
# installed app's copies must be left alone. Each window keeps its own
# tabs, window position, and shared.json.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
main="${HERDR_MAIN_HOME:-$HOME/.config/herdr-app}"
home="${HERDR_APP_HOME:-$HOME/.config/herdr-browser}"

[[ -d "$main" ]] || { echo "no installed app home at $main" >&2; exit 1; }
mkdir -p "$home/mods" "$home/data"

# Replace a copy in the browser home with a symlink. Never deletes $main.
link() {
  local src=$1 dest=$2
  [[ -e "$src" || -L "$src" ]] || return 0
  case "$dest" in
    "$home"/*) ;;
    *) echo "refusing to replace $dest" >&2; exit 1 ;;
  esac
  if [[ -L "$dest" ]]; then
    rm "$dest"
  elif [[ -d "$dest" ]]; then
    rm -rf "$dest"
  elif [[ -e "$dest" ]]; then
    rm -f "$dest"
  fi
  ln -s "$src" "$dest"
}

link "$main/layouts" "$home/layouts"
link "$main/themes" "$home/themes"
for name in keys.json settings.json cockpit.json; do
  link "$main/$name" "$home/$name"
done

# Names this build installs on startup. Anything else in mods/ is the person's.
shipped_list=$(python3 - "$home/installed.json" <<'PY'
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
if not path.exists():
    raise SystemExit(0)
for name in json.loads(path.read_text()):
    if not name.startswith("layout:"):
        print(name)
PY
)

is_shipped() {
  local name=$1
  [[ -n "$shipped_list" ]] || return 1
  grep -qx "$name" <<<"$shipped_list"
}

for dir in "$main/mods"/*; do
  [[ -e "$dir" || -L "$dir" ]] || continue
  name=$(basename "$dir")
  is_shipped "$name" && continue
  link "$dir" "$home/mods/$name"
done

if [[ ! -e "$home/mods/browser" && -d "$root/mods/browser" ]]; then
  link "$root/mods/browser" "$home/mods/browser"
fi

for dir in "$main/data"/*; do
  [[ -d "$dir" ]] || continue
  name=$(basename "$dir")
  # Only data for a mod this home links. Shipped and frame data stay per window.
  [[ -L "$home/mods/$name" ]] || continue
  link "$dir" "$home/data/$name"
done
