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
- Bundled third-party material keeps its own license; see [NOTICE](NOTICE).
  Font files and license texts are used unmodified.
- Never use the em dash in code, docs, UI text or commit messages.
