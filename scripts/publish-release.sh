#!/usr/bin/env bash
# Add assets to release TAG, creating it if windows.yml has not yet (#108).
# Existing assets are never replaced (no --clobber).
set -euo pipefail
tag=$1; shift
if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" --verify-tag --title "LOTUS $tag" --notes-file packaging/release-notes.md "$@" && exit 0
  # Lost a creation race with the other workflow: fall through to upload.
fi
gh release upload "$tag" "$@"
