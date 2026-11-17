# Release version commits

The release workflow commits the editor and addon versions to `main` before
building. Its `prepare` job uses a dedicated write deploy key stored as
`RELEASE_VERSION_DEPLOY_KEY` in the `release-version` environment. That environment
allows tag refs only; branch and pull-request workflows cannot use its secret.
The key is not exposed to the build or publish jobs. The preparation job's normal
`GITHUB_TOKEN` has read-only contents permission.

Two active rulesets apply to `refs/heads/main`:

- `main` keeps deletion and force-push protection, Code Quality error checks, and
  Copilot reviews, with no bypass actors.
- `main - pull requests` keeps the existing pull-request requirements and allows
  deploy keys to bypass only those requirements. Normal users and workflow tokens
  still need a pull request.

GitHub's deploy-key bypass applies to all repository deploy keys, not an individual
key. Keep the release key as the only write deploy key. Adding another write
deploy key would grant that key the same pull-request exception.

GitHub rulesets cannot limit a bypass by workflow name or changed file. The
environment limits access to the credential; `scripts/release/commit_version.py`
requires a clean `main` checkout, stages only the editor/addon Cargo manifests,
addon manifests, and matching lockfiles, and pushes without force. Changes to the
workflow and release scripts still go through the ordinary pull-request rules.

To revoke the exception, remove the deploy-key bypass from `main - pull requests`
and delete the release deploy key and environment secret. Never disable the
`main` ruleset to publish a release.
