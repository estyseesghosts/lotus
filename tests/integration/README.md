# Integration tests

Checks that span the supervisor, the native worker and the IPC boundary.

Planned checks:

- Start two workers, give them separate storage, run them side by side and
  confirm that saves, logs and configuration never mix.
- Confirm clean shutdown, a second launch and two parallel workers.
- Confirm that a worker crash leaves logs and metadata behind, invalidates
  frame buffer leases and releases the per instance lock.
- Confirm that the same operation through the CLI and through MCP produces
  the same normalized result.

A test that needs a ROM or a save file is skipped, not faked, when the
fixture is missing.
