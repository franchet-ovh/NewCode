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

ARM64 builds and tests temporarily retain the native `ubuntu-24.04-arm` runner.
The tests and packaging execute the compiled binaries, so assigning both targets
to an AMD64 scale set alone would not work. Moving ARM64 requires either a native
ARM64 scale set or explicit cross-compilation and emulation support. The shared
checks reject a runner whose native Rust host differs from the requested target.

Workflow edits take effect after being merged; rerunning an older workflow does
not update its runner selection.
