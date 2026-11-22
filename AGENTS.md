# Working in this repository

Read this before changing anything. `CLAUDE.md` is a symlink to this file, so
Claude Code and Codex read the same rules.

## What this is

A runner for Claude Code and Codex agents, reached from a CLI, a Rust library
and MCP. `README.md` states the goal and the six acceptance criteria. They
decide what is in scope. `README.md` is canonical for what exists; keep it
current in the same change that alters behavior.

Build the documentation book with `mdbook build`, preview it with
`mdbook serve`, and lint it with `mdbook-lint lint docs`.

## Ground rules

- Work from an issue. If none covers the task, file one first.
- Never commit to `main`. Branch, and open a draft pull request when the branch
  is created; the body is the plan. Mark it ready when it is done and green.
- One concern per pull request. `Closes #N` when it finishes the issue.
- Before every push: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
  If a check cannot run, say so in the pull request.
- Get an independent review of the exact head before merge and say what kind of
  review it was and what the reviewer ran.
- Merge only with authorization from Josh. Squash-merge and delete the branch.
- Never change the git author or committer. No co-author or tool attribution
  trailers, and no AI attribution in commits, pull requests, issues or comments.

## Writing

Applies to commits, pull requests, issues, README, docs and code comments.

- No em dashes.
- State facts. No marketing tone. Describe what changed and why.
- Conventional-commit prefixes on commit subjects, pull request titles and
  issue titles.
- A performance claim carries the number and how it was obtained.

## Code

- Code carried over from due or counterpart is moved deliberately, one piece per
  pull request, with its tests, and the pull request says where it came from and
  at which commit.
- Every run ends in a stated outcome. A code path that can hang or return
  nothing is a defect.
- Tests never touch real provider state or the real home directory, never need
  the network, and survive being run concurrently.
- A test name says what would break.
