# Repository Workflow

- Always do development work on a `feature/*`, `bugfix/*`, or similarly named non-`main` branch.
- Do not make code changes directly on `main`.
- Make regular, intentional commits as work progresses instead of batching all changes into one final commit.
- Run the relevant tests before merging changes back into `main`.
- Merge into `main` only after the tests pass.

# Rust Formatting

- Keep Rust code formatted with `rustfmt`.
- This repository uses a Git pre-commit hook in `.githooks/pre-commit` to run `rustfmt` on staged Rust files automatically.
