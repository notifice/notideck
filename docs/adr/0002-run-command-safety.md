# ADR 0002: Run-command action safety

- Status: accepted (2026-10-11)
- Resolves: SPEC §15.2

## Context
A "run command" or "launch app" action executes arbitrary programs on the PC. Profiles can be imported,
edited through the config UI, or come from plugins, so a profile must not silently gain code execution.

## Decision
Command-running actions are **disabled by default**. An action only runs if it carries an explicit
per-action `confirmed` flag, set by the user in the config UI after a confirmation prompt that shows the
exact command line. Imported or plugin-supplied profiles have the flag cleared on load. Actions without
the flag are skipped and logged.

## Consequences
- Importing a profile cannot execute anything until the user reviews each command.
- Slight friction when authoring command actions; acceptable for the safety gain.
- Needs a `confirmed` field on the action model and a check in the action router
  (the current router does not enforce this yet).
