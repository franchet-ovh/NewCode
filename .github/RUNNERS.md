# Workflow runners

Release preparation, publication, and AMD64 builds and branch tests use the
organization's `arc-franchet-ovh` runner scale set in `NewCode-group`. Workflows
select both the group and scale-set label explicitly. The group allows public
repositories and grants access only to `franchet-ovh/NewCode`. ARC's
`runnerGroup` setting must also be `NewCode-group`; a workflow group selector
alone does not move the scale set out of `Default`.

The scale set uses ARC Kubernetes container mode. Each job supplies an Ubuntu
24.04 container and installs its prerequisites before checkout. The shared Linux
checks bootstrap Rust because the container does not include GitHub-hosted
runner toolchains. AMD64 jobs require x86-64 runner nodes.

After each checkout, the `git-workspace` composite action adds the exact
`GITHUB_WORKSPACE` path to the shell container's global Git `safe.directory`
configuration and verifies repository discovery. ARC action and shell containers
can use different users and home directories, so checkout's temporary Git trust
configuration may not cover later shell steps. An ownership mismatch can make
`git config user.name ...` report `fatal: not in a git directory` even when `.git`
exists. Trust is limited to the current checkout, rather than all repositories.

ARM64 builds and tests temporarily retain the native `ubuntu-24.04-arm` runner.
The tests and packaging execute the compiled binaries, so assigning both targets
to an AMD64 scale set alone would not work. Moving ARM64 requires either a native
ARM64 scale set or explicit cross-compilation and emulation support. The shared
checks reject a runner whose native Rust host differs from the requested target.

Workflow edits take effect after being merged; rerunning an older workflow does
not update its runner selection.
