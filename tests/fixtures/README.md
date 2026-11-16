# Test fixtures

Test content lives here: homebrew programs, `.3dsx` files, save data,
firmware images and reference captures.

**None of it is committed.** ROMs, firmware, save data and captures are
sensitive user assets. See the attribution and evidence rules in
[AGENTS.md](../AGENTS.md).

## Expected layout

```
tests/fixtures/
├── basic-draw.3dsx        # minimal PICA200 draw, built from tools/hardware-tests
├── saves/                 # save states for clone-from-snapshot tests
└── reference/             # reference captures, content addressed
```

Each fixture needs a recorded SHA-256 so a run can prove which content it
used. The scenario format carries that hash in `content.sha256`.
