# rackend

The `rackend` binary and composition root: CLI, bootstrap environment variables, data directory lock, startup sequence and graceful shutdown, logging, the embedded admin UI bundle, and the wiring of all adapters to the core (SPEC §4.1, §5). It is the only crate that depends on every adapter.
