# crates.io Trusted Publishing setup

## Configured baseline

All 12 publishable workspace crates were configured on 2026-08-26 with the
GitHub publisher identity documented below. The optional crates.io setting
that rejects all API-token publication remains disabled, preserving the
existing emergency publication path.

After configuration, retry attempt 2 of
[release run 32911438140](https://github.com/LioRael/lenso-runtime-rust/actions/runs/32911438140)
completed successfully and published the pending release set. The exact
versions are recorded in the verification section.

## Current diagnosis

The release workflow already has the GitHub permission required to request an
OIDC identity token:

```yaml
permissions:
  id-token: write
```

That permission only allows the job to request an OIDC JWT. It does not grant
permission to publish any crate on crates.io. crates.io authorizes a publish
only when the JWT's repository and workflow identity matches a Trusted
Publisher configuration attached to the specific crate.

The failed [release run 32911438140](https://github.com/LioRael/lenso-runtime-rust/actions/runs/32911438140)
shows that release-plz successfully:

1. retrieved a GitHub Actions JWT with audience `crates.io`; and
2. exchanged it for a short-lived crates.io token.

The later upload failed with `The provided access token is not valid for crate
\`lenso-native-adapter-macros\``. This isolates the failure to crate-level
authorization, rather than GitHub's `id-token` permission or OIDC token
exchange.

Release-plz implements the same crates.io token exchange as
`rust-lang/crates-io-auth-action`, so this workflow should not add that action.
It does, however, require Trusted Publishing to be configured for **all** crates
that release-plz may publish. See the [release-plz trusted-publishing
instructions](https://release-plz.dev/docs/github/quickstart#2-set-the-cargo_registry_token-secret).

## Exact configuration for this repository

On each crate's crates.io page, open **Settings → Trusted Publishing**, add a
GitHub Actions publisher, and use:

| Field | Value |
| --- | --- |
| Repository owner | `LioRael` |
| Repository name | `lenso-runtime-rust` |
| Workflow filename | `release-plz.yml` |
| Environment | leave empty |

The workflow field is the filename only, not
`.github/workflows/release-plz.yml`. The environment must remain empty because
the `release` job currently has no GitHub Actions `environment:`. If an
environment is added later, configure the same environment on every crate.

Create the configuration separately for every publishable workspace crate:

- `lenso`
- `lenso-browser-driver`
- `lenso-dylib-adapter`
- `lenso-guest-sdk`
- `lenso-native-adapter`
- `lenso-native-adapter-macros`
- `lenso-plugin-bundle`
- `lenso-plugin-control-plane`
- `lenso-quickjs-adapter`
- `lenso-runner`
- `lenso-runtime-codec`
- `lenso-wasm-component-adapter`

Do not configure `lenso-test`, `lenso-wasip2-driver`, or fixture packages while
their manifests have `publish = false`.

The per-crate requirement follows directly from the crates.io data model: a
GitHub Trusted Publisher configuration contains a single `crate` field, and
the resulting short-lived token contains the IDs of the crates whose
configurations matched the OIDC claims. See the crates.io
[`NewGitHubConfig` API type](https://github.com/rust-lang/crates.io/blob/main/crates/crates_io_api_types/src/trustpub.rs)
and [token matching implementation](https://github.com/rust-lang/crates.io/blob/main/src/controllers/trustpub/tokens/exchange/mod.rs#L129-L219).

## UI, API, and CLI support

The supported setup path is the crates.io UI described in the official
[Trusted Publishing guide](https://crates.io/docs/trusted-publishing).

crates.io also exposes authenticated REST endpoints:

- `GET /api/v1/trusted_publishing/github_configs?crate=<crate>`
- `POST /api/v1/trusted_publishing/github_configs`
- `DELETE /api/v1/trusted_publishing/github_configs/<id>`

The POST body has this shape:

```json
{
  "github_config": {
    "crate": "lenso-native-adapter-macros",
    "repository_owner": "LioRael",
    "repository_name": "lenso-runtime-rust",
    "workflow_filename": "release-plz.yml",
    "environment": null
  }
}
```

Creation requires an authenticated owner of that crate, a verified crates.io
email, a linked GitHub account, and authorization for the Trusted Publishing
endpoint. These requirements and the request schema are defined by the
crates.io [create endpoint](https://github.com/rust-lang/crates.io/blob/main/src/controllers/trustpub/github_configs/create.rs)
and [JSON request type](https://github.com/rust-lang/crates.io/blob/main/src/controllers/trustpub/github_configs/json.rs).

As of 2026-08-26, Cargo has no `cargo trustpub` command; adding one is still an
open [Cargo feature proposal](https://github.com/rust-lang/cargo/issues/17114).
Release-plz also does not create Trusted Publisher configurations. Therefore,
use the crates.io UI unless an appropriately scoped crates.io API token is
already available for controlled API automation.

## First publication

Trusted Publishing cannot create a new crate. A crate must already exist on
crates.io, and the person configuring it must be an owner. The first version of
a new crate must therefore be published with a regular crates.io API token;
Trusted Publishing can be configured after that. This limitation is documented
by both the [crates.io guide](https://crates.io/docs/trusted-publishing) and the
[release-plz quickstart](https://release-plz.dev/docs/github/quickstart#2-set-the-cargo_registry_token-secret).

For an initial publish, first run `cargo publish --dry-run` (or `cargo package`)
and then perform the one-time publish with an authorized token. Cargo's
[publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html)
documents the token-based bootstrap flow and the permanence of published
versions.

## Verification after configuration

The successful workflow published the following versions, each independently
verified with `cargo info <crate>@<version> --registry crates-io`:

| Crate | Published version |
| --- | --- |
| `lenso` | `0.4.1` |
| `lenso-browser-driver` | `0.1.3` |
| `lenso-dylib-adapter` | `0.1.1` |
| `lenso-guest-sdk` | `0.1.1` |
| `lenso-native-adapter` | `0.2.2` |
| `lenso-native-adapter-macros` | `0.1.2` |
| `lenso-plugin-bundle` | `0.1.1` |
| `lenso-plugin-control-plane` | `0.1.1` |
| `lenso-quickjs-adapter` | `0.1.1` |
| `lenso-runner` | `0.1.8` |
| `lenso-runtime-codec` | `0.1.1` |
| `lenso-wasm-component-adapter` | `0.1.1` |

For later releases, require both a successful release workflow and registry
lookups for the exact immutable versions. A merged release PR or successful
package build alone is not publication evidence.
