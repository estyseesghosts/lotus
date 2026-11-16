# Scenarios

Scenario files are YAML. They describe a bounded, reproducible run: the
content, the emulation profile, the storage isolation, the input schedule, the
stop condition, the captures and the assertions.

The runner validates a scenario before it executes it. An unbounded scenario
is rejected rather than truncated.

Example: [smoke-basic-draw.yaml](smoke-basic-draw.yaml).
Format reference: [../../specs/scenario-example.yaml](../../specs/scenario-example.yaml).
