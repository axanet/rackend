# Security policy

rackend stores data, accounts and credentials for the applications built on
it, so we take security reports seriously. The threats rackend is designed
against, and its mitigations, are listed in the
[threat model](docs/threat-model.md).

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub's private vulnerability reporting](https://github.com/axanet/rackend/security/advisories/new).
Do not open a public issue, and do not test against instances you do not own.

Please include the rackend version, your configuration (without secrets), the
steps to reproduce and the impact you see. We aim to acknowledge reports
within a few working days, agree on a disclosure date with you, and credit you
in the advisory unless you prefer otherwise.

## Supported versions

Security fixes go into the latest minor release (`x.y`). Before 1.0, only the
latest release is supported. Pin the minor version of the image
(`ghcr.io/axanet/rackend:x.y`) to receive patch releases.

## Scope

In scope: the rackend server and binary, the admin UI, the official container
image and the hosted auth pages. Out of scope: vulnerabilities that require a
superuser account or access to the data directory, denial of service by
traffic volume, and issues in the website (see the
[homepage repository](https://github.com/axanet/rackend-homepage/security/policy)).
