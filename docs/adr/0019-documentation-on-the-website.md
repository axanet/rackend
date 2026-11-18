# 0019. The user documentation lives on the website, versioned

- Status: accepted
- Date: 2026-11-18
- Supersedes: the documentation layout of [0016](0016-m7-openapi-and-docs-details.md)

## Context

The guides, the deployment guide, the error reference and the realtime protocol lived in `docs/` of this repository. Readers need them for the version they run, with screenshots of the admin UI, and the website is where they look. Two checks depend on the documentation: the error contract test (every problem code is documented, and `type` links to it) and the getting started guide, which CI runs verbatim (SPEC §20 M7).

## Decision

- The user documentation is part of the website, built with Starlight in [rackend-homepage](https://github.com/axanet/rackend-homepage). `/docs/` is the latest release, `/docs/next/` describes `main`, and older minor versions stay under `/docs/<x.y>/`. A version picker switches between them; only the latest release is in the search index.
- Screenshots of the admin UI are taken by a script from the release's container image, in light and dark, per minor version.
- Problem `type` URIs are `https://rackend.axanet.ch/docs/reference/errors/#<code>` from the release after 0.0.1 on. The latest docs are served without a version in the path, so these links stay valid. `docs/errors.md` and `docs/deployment.md` stay as short pages that forward the links rackend 0.0.x sends.
- The error contract test and `scripts/getting-started.sh` read the `next` docs from a rackend-homepage checkout (`RACKEND_DOCS_DIR`, else next to this repository); CI checks it out.
- This repository keeps what maintainers need: ADRs, the threat model and performance measurements.

## Consequences

- A change to documented behaviour needs a pull request in rackend-homepage first, so that CI here passes against its `next` docs.
- Running `mise run test` locally needs the rackend-homepage checkout.
- After a release, `mise run docs:release` in rackend-homepage updates the version the docs name, or archives the latest minor version and promotes `next`.
