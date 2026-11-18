# Contributing

Thanks for your interest in rackend.

rackend is open source under the [Apache License 2.0](LICENSE): you may read,
use, run, fork and adapt it under the terms of that license.

## Issues are welcome

Bug reports, questions and ideas help most. Please
[open an issue](https://github.com/axanet/rackend/issues/new/choose) and
include the rackend version (`rackend version`), how you run it (Docker,
binary, behind which proxy) and what you expected. The request id from an
error response (`X-Request-Id`) makes server logs easy to find.

Report security problems privately, never as a public issue: see
[SECURITY.md](SECURITY.md). Issues with the website belong in the
[homepage repository](https://github.com/axanet/rackend-homepage/issues).

## Pull requests are not accepted

Changes to this repository are made only by the maintainers at axanet. Pull
requests from outside the axanet organization are closed automatically. This
is not a judgement of the change: rackend follows a written specification, and
we prefer to keep design, code and data-safety guarantees in one pair of
hands. If you have a fix in mind, describe it in an issue (a failing test or a
reproduction is perfect) and we will take it from there.

## For maintainers

- Tools and tasks are managed with [mise](https://mise.jdx.dev); see the
  [README](README.md). `mise run lint` and `mise run test` must pass.
- Decisions the specification leaves open are recorded in [docs/adr](docs/adr).
- The user documentation is on the website and lives in
  [rackend-homepage](https://github.com/axanet/rackend-homepage). Its `next`
  docs (`src/content/docs/docs/next/`) describe `main`: a change to behaviour
  updates them in a pull request there, merged before the change here. The
  error contract test and the getting started guide run against them, so
  keep a rackend-homepage checkout next to this one or set `RACKEND_DOCS_DIR`.
- Bundled third-party material keeps its own license; see [NOTICE](NOTICE).
  Font files and license texts are used unmodified.
- Never use the em dash in code, docs, UI text or commit messages.

### Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org),
because releases take their version from them ([cog.toml](cog.toml)):

| Commit | Release |
|---|---|
| `feat: ...` | minor (0.1.0 to 0.2.0) |
| `fix: ...`, `perf: ...` | patch (0.1.0 to 0.1.1) |
| `feat!: ...` or a `BREAKING CHANGE:` footer | major; minor while the version is 0.x |
| `docs`, `refactor`, `test`, `build`, `ci`, `style`, `chore`, `revert` | none on its own |

A scope is optional (`fix(rules): ...`). `feat`, `fix`, `perf`, `revert` and
`docs` commits appear in [CHANGELOG.md](CHANGELOG.md), so write their subject
for users. `mise run hooks` installs a commit-msg hook that checks each
message.

Changes reach `main` through pull requests, merged by squash or rebase:

- **Squash:** the PR title becomes the commit on `main`, so CI checks it like
  a commit message. If any commit in the PR is a breaking change, the title
  must say so too (`feat!: ...`).
- **Rebase:** every commit lands as it is, so CI checks every commit in the
  PR and rejects `fixup!` and `squash!` commits; autosquash them first.

### Releasing

Releases are cut on demand from `main`: **Actions, release, Run workflow**,
with an optional pre-release identifier (`alpha` gives `0.2.0-alpha.1`) or an
exact version. The exact version is the only way to 1.0.0, since breaking
changes bump the minor version while it is 0.x. The workflow refuses a
commit on which CI has not passed. It then

1. takes the next version from the commits since the latest tag, unless an
   exact one is given (`mise run release --dry-run` shows it locally),
2. sets it in `Cargo.toml`, moves the `x.y` image tag in the docs (stable
   releases only), writes the upgrade fixture of the release and runs the
   upgrade tests against it,
3. commits `chore(version): x.y.z` with the new section of CHANGELOG.md, tags
   `x.y.z` and pushes both,
4. publishes `ghcr.io/axanet/rackend` for amd64 and arm64 and a GitHub
   Release with the changelog section.

The workflow pushes as the release GitHub App (repository variable
`RELEASE_APP_ID`, secret `RELEASE_APP_PRIVATE_KEY`), the only actor the
rulesets let push to `main` and create release tags. Its push runs CI on the
release commit, and the tag runs the release workflow again for step 4.
`mise run release` does steps 1 to 3 from a local checkout, for someone the
rulesets let bypass.

After a stable release, update the docs in rackend-homepage with
`mise run docs:release x.y.z`: a patch release only changes the version they
name; a new minor version archives the current docs under `/docs/<x.y>/` and
makes `next` the latest. Take the screenshots of a new minor version with
`mise run docs:screenshots` there. A release needs at least one `feat`, `fix` or `perf`
commit (or a breaking change) since the latest tag.
