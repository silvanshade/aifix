# Local CI with act

Run `.github/workflows/ci.yml` locally via [act](https://github.com/nektos/act) + Docker through the repo wrapper `nu scripts/act-ci.nu`.
Iterate on workflow changes without burning hosted minutes or triggering run approvals — agents never approve GitHub Actions runs, so act-local is the only autonomous iteration loop for CI.

## Requirements

* Docker running (`docker info` succeeds).
* `act` on `PATH`.
  `mise.toml` pins `act` at `0.2.89`; `mise install` resolves it.

## Invocation

The wrapper selects jobs and supplies act with persistent Docker volumes for the Cargo, rustup, mise, and aube caches.
Run a single job:

```sh
mise exec -- nu scripts/act-ci.nu --job docs-gates
```

List jobs without running them:

```sh
mise exec -- nu scripts/act-ci.nu --list
```

Run every workflow job sequentially, cheapest known jobs first (`docs-gates`, `project-lint`, `cargo-build`, `cargo-clippy`, `cargo-nextest`, `cargo-llvm-cov`), removing containers and volumes after failed runs:

```sh
mise exec -- nu scripts/act-ci.nu --all --rm
```

The pre-push hook runs exactly that `--all --rm` form.

## Cache mounts

Cache setup is mount-only: the wrapper passes `--use-gitignore`, `--action-offline-mode`, and `--pull=false`, sets `MISE_ENV=ci`, and points `CARGO_TARGET_DIR` at a per-worktree directory under `/tmp/aifix-act-target/`.
Named volumes (`aifix-act-*`) cover the Cargo registry and git, the target directory, rustup, the mise store/state/cache, the aube store/cache, and the cargo-careful and tree-sitter caches.
Delete a volume to force a cold re-run.

Worktree checkouts use a `.git` file pointing outside the worktree; the wrapper mounts the shared git dir read-only at its absolute path so act containers can read history.
