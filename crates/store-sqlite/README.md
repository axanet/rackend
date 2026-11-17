# rackend-store-sqlite

Implements the core `Store` port on SQLite: one writer thread plus a pool of read-only connections per database file, the PRAGMAs every connection uses, the system migration runner with the pre-upgrade backup and downgrade protection (SPEC §6, §7), and later the rule-to-SQL compiler (SPEC §9.5).
