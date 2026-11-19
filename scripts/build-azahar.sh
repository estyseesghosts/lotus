#!/usr/bin/env bash
# Linux counterpart of build-azahar.ps1 (#103): build the pinned native worker
# with an isolated cache per flavor. Same pins, same CMake options except where
# noted in docs/27 section 11.
#
#   scripts/build-azahar.sh <integration|experimental> <azahar-root> <build-dir> [parallel]
set -euo pipefail

branch=${1:?branch}
source_root=$(cd "${2:?azahar root}" && pwd)
build_dir=$(mkdir -p "${3:?build dir}" && cd "$3" && pwd)
parallel=${4:-$(nproc)}
root=$(cd "$(dirname "$0")/.." && pwd)

case $branch in
  integration) backend=AZAHAR ;;
  experimental) backend=CITnLO ;;
  *) echo "unknown branch: $branch" >&2; exit 2 ;;
esac

expected=$(python3 -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["azahar"][sys.argv[2]+"_commit"])' \
  "$root/config/pins.toml" "$branch")
actual=$(git -C "$source_root" rev-parse HEAD)
if [ "$actual" != "$expected" ]; then
  echo "Source pin mismatch: expected $expected, got $actual" >&2; exit 1
fi
if [ -n "$(git -C "$source_root" status --porcelain --untracked-files=normal)" ]; then
  echo "Build inputs must be committed and clean" >&2; exit 1
fi
cache=$build_dir/CMakeCache.txt
if [ -f "$cache" ]; then
  home=$(sed -n 's/^CMAKE_HOME_DIRECTORY:INTERNAL=//p' "$cache")
  if [ "$(cd "$home" 2>/dev/null && pwd)" != "$source_root" ]; then
    echo "Cache belongs to a different source tree: $cache" >&2; exit 1
  fi
fi

flags=(-G Ninja -DCMAKE_BUILD_TYPE=Release -DENABLE_CTR_WORKER=ON
  -UCMAKE_PROJECT_citra_INCLUDE
  -DENABLE_QT=OFF -DENABLE_LIBRETRO=OFF -DENABLE_SDL2=OFF
  -DENABLE_WEB_SERVICE=OFF -DENABLE_SCRIPTING=OFF -DENABLE_ROOM=OFF
  -DENABLE_ROOM_STANDALONE=OFF -DENABLE_OPENAL=OFF -DENABLE_CUBEB=ON
  -DENABLE_LIBUSB=OFF -DENABLE_SOFTWARE_RENDERER=ON -DENABLE_VULKAN=ON
  -DENABLE_OPENGL=OFF -DENABLE_GDBSTUB=OFF -DENABLE_LTO=OFF -DCITRA_WARNINGS_AS_ERRORS=OFF
  "-DCTR_BACKEND_NAME=$backend")

cmake -S "$source_root" -B "$build_dir" "${flags[@]}"
cmake --build "$build_dir" --target ctr-worker --parallel "$parallel"

python3 - "$build_dir/lotus-build-manifest.json" "$actual" "$branch" "$source_root" "${flags[@]}" <<'EOF'
import json, platform, subprocess, sys
out, source, branch, source_root, *flags = sys.argv[1:]
compiler = subprocess.run(["c++", "--version"], capture_output=True, text=True).stdout.splitlines()[0]
json.dump({"source": source, "branch": branch, "source_root": source_root, "target": "ctr-worker",
           "cmake_flags": flags, "compiler": compiler, "host": platform.platform()},
          open(out, "w"), indent=2)
EOF
