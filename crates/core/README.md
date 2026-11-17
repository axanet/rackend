# rackend-core

The domain model, application use cases and ports (traits) of rackend. It holds all product behaviour and depends on no I/O crate and no other workspace crate; adapters implement its ports and the `rackend` binary wires them together (SPEC §4).
