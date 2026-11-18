# Release version commits

The release workflow commits the editor and addon versions to `main` before
building. Its `prepare` job uses a dedicated GitHub App owned by `franchet-ovh`,
installed only on `NewCode`, with repository **Contents: read and write**
permission (plus mandatory Metadata read permission). Webhooks are disabled;
no additional permissions are needed.

The `release-version` environment stores the `RELEASE_APP_CLIENT_ID` variable
and `RELEASE_APP_PRIVATE_KEY` secret. That environment
allows tag refs only; branch and pull-request workflows cannot use its secret.
The preparation job generates a short-lived installation token scoped to this
repository with Contents write permission and passes it to checkout for HTTPS
pushes. The token action revokes it when the job ends. Neither the private key
nor the token is passed to build or publish jobs. The preparation job's normal
`GITHUB_TOKEN` has read-only contents permission.

Two active rulesets apply to `refs/heads/main`:

- `main` keeps deletion and force-push protection, Code Quality error checks, and
  Copilot reviews, with no bypass actors.
- `main - pull requests` keeps the existing pull-request requirements and allows
  only the release App to bypass those requirements, using an Integration bypass
  actor with mode `always`. Normal users and workflow tokens
  still need a pull request.

GitHub rulesets cannot limit a bypass by workflow name or changed file. The
environment limits access to the credential; `scripts/release/commit_version.py`
requires a clean `main` checkout, stages only the editor/addon Cargo manifests,
addon manifests, and matching lockfiles, and pushes without force. Changes to the
workflow and release scripts still go through the ordinary pull-request rules.

## Initial setup

1. In the `franchet-ovh` organization settings, register a private GitHub App
   named `NewCode Release` (use a unique suffix if unavailable). Use the repository
   URL as its homepage, disable webhooks, and grant only repository Contents read
   and write permission. Install it only on `NewCode`.
2. Copy its Client ID into the `RELEASE_APP_CLIENT_ID` variable in the repository's
   `release-version` environment. Generate and download an App private key, then
   upload the PEM file as the `RELEASE_APP_PRIVATE_KEY` environment secret.
   Do not commit the key or paste it into logs or chat. For example:

   ```sh
   gh variable set RELEASE_APP_CLIENT_ID --repo franchet-ovh/NewCode --env release-version --body '<client-id>'
   gh secret set RELEASE_APP_PRIVATE_KEY --repo franchet-ovh/NewCode --env release-version < /path/to/private-key.pem
   ```

3. Add the App to the bypass list of `main - pull requests` with mode `always`.
   Keep the separate `main` ruleset unchanged.
4. Merge the workflow change through a pull request, then validate a release
   using the updated workflow. Confirm preparation pushes the version commit
   and builds use the resulting SHA before publication.
5. After successful validation, delete the obsolete release deploy key and the
   `RELEASE_VERSION_DEPLOY_KEY` environment secret.

To revoke the exception, remove the App bypass from `main - pull requests`,
uninstall the App, revoke its private key, and delete the environment variable
and secret. Never disable the `main` ruleset to publish a release.
