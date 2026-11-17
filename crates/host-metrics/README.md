# rackend-host-metrics

Implements the core `HostMetrics` port with `sysinfo` plus cgroup v2 reads, so container limits are reported instead of host totals (SPEC §14.3). Implemented in M5.
