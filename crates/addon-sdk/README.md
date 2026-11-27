# Focus executable addon SDK, protocol 1

An addon is an independently built executable. It communicates with Focus Editor
using JSON-RPC 2.0 objects separated by newlines on stdin/stdout (8 MiB maximum
per message). Send diagnostics to stderr. The host owns native UI rendering;
the addon has no dependency on egui or the editor crate. Other languages can
implement the same protocol. Installed addons are trusted OS programs.

Install each addon under `<application-data>/focus-editor/addons/<id>/`, beside
`logs/`. Open the **Addons** activity below Source Control, choose **Install local
addon…** and select its `addon.toml` or standalone `.tar.gz`, or use the release
installer. Repository directories are never searched for addons.
Executable and PNG icon paths must stay inside the installation directory.
Any addon may declare `panel = "right"` to use the shared right-hand dock.
Background addons declare `panel = "none"` to stay out of panel selectors and the
activity bar while continuing to provide services such as language support.
Every manifest must declare a nonempty `category`: the bundled assistants use
`IA Harness`, language addons use `Language`, and GitHub uses `CI/CD`.
Manifests without a category are rejected; older addons need updated manifests
or rebuilding before installation in this editor.
The default `ui = "widgets"` uses declarative controls; rich interfaces can use
`ui = "surface"` through the optional [UI helper](../addon-support/README.md).

```toml
id = "example"
name = "Example"
category = "CI/CD"
description = "What this addon provides"
publisher = "Your name or organization"
version = "0.1.0"
editor_version_req = ">=0.4.8, <0.5.0"
protocol = 1
executable = "example-addon"
icon = "icon.png"
```

`description` and `publisher` are optional display metadata. Release staging adds
an optional top-level `target`, such as `x86_64-unknown-linux-gnu`; the host rejects
packages for another target. Keep the manifest version equal to the addon Cargo
package version. The ID must match its directory and contain only letters,
digits, `_`, and `-`.

## Independent releases and compatibility

Each addon owns its semantic version, independently of the editor. Releases use
`addon_<id>_<x.y.z>` tags. Main publishes changed addons after successful checks,
incrementing the patch version; the manual release workflow accepts an explicit
stable version. Release stamping updates only the selected addon in an isolated
checkout. Reproduce a tagged version with `scripts/release/publish_addon.py stamp`.

New packages require `editor_version_req`, parsed with Rust's `semver::VersionReq`.
The editor checks it together with `protocol` and `target` during loading and
installation, including local packages. Update the range deliberately when host
changes require a newer editor; bump the protocol for incompatible wire changes.
Legacy manifests without a range retain protocol/platform checks.

Schema-2 `addons.json` contains `schema`, `release`, and `packages`; every package
includes its own version, editor requirement, protocol, target, archive name, size,
and SHA-256. Catalog and archive compatibility fields must agree. Schema 1 remains
readable with its catalog-wide exact editor version. Discovery tracks each
package's source release, selecting an older compatible version when necessary.
Installation and upgrades remain manual and transactional.

The source installer calls `focus-editor --check-addon-compatibility addon.toml`
without opening a window. Use its `--editor` option to choose a specific built
editor executable; incompatibility fails before an existing installation changes.

## Workspace execution

Optional manifest `execution = "workspace"` runs the addon in the active workspace's
container, or on the host when the workspace is local. The default is `"host"`.
Container packages must match the installed addon version and target. The host
translates `Context.workspace` and scratch `Context.storage` to container paths,
clears `Context.runtime`, and transports the existing protocol over `docker exec -i`
without a TTY. Use local child processes; do not invoke Docker inside the addon.

`Context.persistent_state` optionally contains the host's last non-secret snapshot.
The shared runner's `Panel::persistent_state()` hook publishes changed snapshots
through `state.changed` with `{generation, state}`. State must be a JSON object
within the normal protocol size limit. The host writes snapshots atomically to
workspace addon data, isolated by addon and execution environment. On shutdown,
flush the last snapshot before exiting; do not store credentials in snapshots.
Workspace-executed processes restart across runtime changes. Old messages and
cancelled preparations cannot replace the new generation's process or state.

## Lifecycle and UI

1. Host sends request `initialize` with `{ "protocol": 1, "context": Context }`.
   Respond with `{ "protocol": 1 }` within five seconds.
2. Publish notifications `view.update` containing `View`. One activity-bar icon
   opens the addon's sidebar; a nonempty `detail` opens an optional central view.
   The host supports text, Markdown, buttons, inputs, checkboxes, selectors,
   links, separators, and rows. Widget/action IDs must be stable and unique.
3. Host sends `action` notifications with an action ID, current context
   generation, and all rendered form values. Handle actions in the executable,
   publishing a busy view before long operations and a completed view afterward.
4. Host sends `context.changed` with a new generation when the workspace or
   Dev Container changes. Replace repository state; ignore obsolete actions.
   Every view and service request carries its originating generation.
5. `view.activated` announces selection. `tick` arrives approximately every
   30 seconds with `{ "visible": bool }`, allowing visible refresh and pending
   operation checks. `shutdown` requests termination; the host allows two seconds.

`View.form` identifies a form. Changing it resets host input values and opens
nonempty detail content. Keeping it preserves edits during background refresh.
Set `View.reveal_detail` on selection to reopen the same item after dismissal.
The host namespaces widget state per addon. `host.credentials` is a reserved
button action that opens the native credential dialog without transmitting
token text to the executable. Normal file tabs dismiss addon detail views.

`Widget::Select.on_change` optionally names an action emitted when the selection
changes. These reactive choices follow the values in subsequent views, allowing
dependent option lists to update without clearing other form inputs.

## Assistant handoff

`Service::Assistants` returns JSON-encoded `InstalledAssistant` entries for Codex
and OpenCode, including enabled state, errors, and live capabilities. The shared
surface runner publishes generation-scoped `assistant.capabilities` notifications
from `Panel::assistant_capabilities`; older addons without this support cannot
receive a handoff. The host sends catalog changes to GitHub as `assistants.changed`.

`Service::StartAssistant { addon, session }` validates the selected model, mode,
and optional reasoning ID against that addon's current capabilities and forwards
a correlated `assistant.start` request to its runner. `Panel::start_assistant`
opens a fresh session and schedules its first prompt. Success acknowledges startup,
not completion of the AI turn; backend failures appear in the assistant panel.
Requests and replies are scoped to the workspace and addon process incarnation.
The runner rejects stale generations, and the host rejects disabled, unavailable,
or busy assistants. No provider credentials are sent to the requesting addon.

## Host services

Use `Connection::call(generation, Service)` for background tool execution, HTTPS,
or Git. Interleaved notifications are queued until the request completes.

- `Run`: explicit program/arguments and optional stdin; no implicit shell.
  A `missing_tool` reply allows the addon to choose a fallback. Other errors
  should not trigger an identity/backend change or automatic write retry.
- `Http`: HTTPS, optional JSON body, bounded responses, no automatic redirects.
- `Git`: read queries, fetch, checkout, push, and checked branch deletion.
  Mutations share Source Control's lease and use the active Dev Container when
  present. Checkout/cleanup resolves dirty editor buffers through the host.
  Conflicting worktree changes and new branch commits are preserved.

Service replies contain `ok`, `code`, and `output`. HTTP/command failures are
returned as errors; check remote state before retrying uncertain writes.
Unsupported protocol versions, malformed frames, and process failures leave
the editor running and expose Restart in the addon UI.

Optional manifest credentials declare `origin`, `program`, `environment`,
`git_origin`, and user-facing `instructions`. The host reads/stores credentials
in the OS keyring, attaches them only to the matching HTTPS origin or declared
program, and returns sanitized results. It does not expose a token-reading API.
Use `Context.storage` for non-secret persistent addon state; preserve the
generation/workspace association of pending local operations. This path is
`<application-data>/focus-editor/workspaces/<workspace-id>/addon-data/<id>`,
outside the shared executable folder. Updates and uninstall preserve workspace
data and OS-keyring credentials. Old global state is not imported. Do not write
runtime state into the installation directory.

Optional `[[settings]]` manifest entries declare non-secret workspace text fields
with `key`, `label`, and optional `description` and `placeholder` strings. Keys
must be unique and use the same characters as addon IDs. The Addons manager
renders these fields for installed addons, including disabled ones. Saving
merges non-empty text into `[addon_state.<id>]` in the workspace's settings;
blank fields remove their keys. Other state is preserved. Values are delivered
through `Context.state` on initialization or the next workspace context change;
saving does not notify or restart running addons. Tell users to restart the
addon to apply changes. Manifests without settings remain supported.

The GitHub addon is a complete example. Build it separately with:

```sh
cargo build --locked --manifest-path addons/github/Cargo.toml
python3 scripts/addons.py --addon github --install
```

## Release packages and catalog

Place a crate and `addon.toml` under `addons/<id>/`. The build helper and release
workflow discover all such manifests; the editor crate does not compile them.
Use `python3 scripts/addons.py --release --target <triple>` to build and stage all
addons under `target/<triple>/release/addons/`. `--addon <id>` selects one and can
be repeated. `--test` checks formatting and tests the selected crates and their local dependencies;
GitHub-only tests do not build the UI support crate.
Release executables must support `--version`, printing their executable basename
followed by the version without starting the addon protocol. The release job
checks architecture, shared libraries, and that version response. Addon-specific
license/notices files take precedence over the repository defaults.

`scripts/release/publish_addon.py package --addon <id> --tag <tag> --target <triple>`
emits only the selected `focus-addon-<id>-<tag>-<platform>.tar.gz` and
`addons-<platform>.json`. Editor packaging is separate. Each addon archive contains files
directly at its root: `addon.toml`, its declared executable and PNG icon,
`LICENSE`, and `THIRD_PARTY_NOTICES.txt`. Relative asset subdirectories are
supported. Runtime state, credentials, extra files, symlinks, and hardlinks are
excluded. The editor accepts only those declared assets and optional notices;
compressed and expanded size limits are 256 MiB and 512 MiB.

The final release job checks the complete platform matrix and emits `addons.json`
and `SHA256SUMS`. `Catalog` and `Package` in this SDK define catalog schema 2:

```json
{
  "schema": 2,
  "release": "addon_example_0.1.0",
  "packages": [{
    "id": "example", "name": "Example", "description": "An example addon",
    "publisher": "Example project", "version": "0.1.0", "protocol": 1,
    "editor_version_req": ">=0.4.8, <0.5.0",
    "target": "x86_64-unknown-linux-gnu",
    "archive": "focus-addon-example-addon_example_0.1.0-linux-amd64.tar.gz",
    "size": 1234, "sha256": "<64 hexadecimal characters>"
  }]
}
```

The manager fetches the official catalog anonymously, checks editor version,
protocol, target, archive SHA-256/size, and manifest identity before replacement.
Local packages use the same manifest/archive checks without an official
checksum. Installation receipts are separate from addon state. Shared process
leases prevent updates while another editor uses the executable; updates use a
rollback journal and restart only the affected process. No protocol changes or
addon-specific host code are needed to appear in the manager or release catalog.

## Language providers (optional extension 1)

Add `[[languages]]` declarations to a normal addon manifest:

```toml
[[languages]]
id = "example-language"
extensions = ["example", "ex"]
```

The host advertises `"language": 1` in its `initialize` parameters.
Language addons acknowledge `"language": 1` in their `initialize` result alongside
`"protocol": 1`. Existing non-language addons are unaffected. The host activates
providers lazily, without selecting their sidebar. A provider must acknowledge
initialization promptly; server provisioning runs afterward, away from protocol
input processing. The host only starts an installed, enabled provider.

- `lsp.start` notification carries `language::Start`: workspace generation, session
  ID, language ID, runtime-specific `Context`, and optional command/argument
  overrides. The provider installs dependencies and starts its owned server.
- `lsp.status` carries generation/session, `status` text, and `failed` boolean.
  Publish progress and actionable failures; do not silently switch runtimes.
- `lsp.started` carries generation/session, fully namespaced `settings` JSON,
  and optional `initialization_options` JSON (defaulting to `settings`).
  The host then initializes the LSP session and synchronizes editor documents.
- Bidirectional `lsp.message` notifications carry `language::Packet`: generation,
  session, and a standard LSP JSON-RPC object. LSP IDs are independent of addon IDs.
  The addon bridges JSON objects to Content-Length-framed server stdio using
  `language::read` / `language::write` (3 MiB maximum server frame).
- `lsp.stop` carries generation/session. Stop and reap the owned process tree,
  then acknowledge with `lsp.stopped` carrying the same pair. The host waits for
  this acknowledgement before starting another server.
- `lsp.retry` carries the current generation and asks the host for a fresh
  session, including fresh document snapshots. Obsolete incarnations are ignored.

On `context.changed`, shutdown, or host disconnect, stop owned processes and
cancel provisioning, including inside containers. Serialize shared installations
and keep their state outside executable directories. Servers remain the addon's
responsibility: the host never interprets server executable names or installs
language-specific dependencies. Provider-specific options can live in
`Context.state`; the Python addon accepts an `interpreter` path there.

`crates/language-addon` supplies the common runner for the Rust and Python addon
executables, including cancellation, framing, private environments, and local /
container process groups. More languages may implement the protocol directly.

## Shared presentation

Declarative widgets retain protocol v1. Optional metadata supplies semantic roles, never colors:

- `Button.style`: `role` (`primary`, `secondary`, `quiet`, `destructive`), `selected`, and `description` for list rows.
- `Text.role`: `body`, `heading`, `muted`, `success`, `warning`, or `error`.
- `Input.options`: `placeholder`, `help`, and optional `on_change` action ID.
- `Row.layout`: `wrap` (default), `stack`, `card`, or responsive `columns`.

Use the `Widget` builder methods to compose these elements. Omitted metadata keeps existing widgets valid; older hosts ignore the additional fields. Reactive inputs send the existing Action with current values; keep handlers local for filtering loaded results. The host retains edits during busy updates and submits the latest value when ready. Constructors and direct Rust variant initializers must include the new metadata fields when rebuilding against this SDK.
