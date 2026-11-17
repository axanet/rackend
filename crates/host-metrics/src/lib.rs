//! `rackend-host-metrics`: the `HostMetrics` port (SPEC §14.3).
//!
//! Host numbers come from `sysinfo`. Inside a container with cgroup v2,
//! memory and CPU come from the cgroup (`memory.max`, `memory.current`,
//! `cpu.max`, `cpu.stat`), so the dashboard shows the container's limits,
//! not the host's RAM.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use rackend_core::ports::{HostMetrics, HostSample};
use sysinfo::{Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// cgroup v2 numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CgroupLimits {
    /// `memory.max` in bytes, `None` for `max`.
    pub memory_max: Option<u64>,
    /// `memory.current`.
    pub memory_current: Option<u64>,
    /// CPUs from `cpu.max` (`quota / period`), `None` for `max`.
    pub cpus: Option<f64>,
    /// `cpu.stat` `usage_usec`.
    pub cpu_usage_usec: Option<u64>,
}

fn read(dir: &Path, file: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(file))
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Reads cgroup v2 files from `dir` (normally `/sys/fs/cgroup`).
pub fn read_cgroup(dir: &Path) -> Option<CgroupLimits> {
    // cgroup v2 has `cgroup.controllers` at its root.
    read(dir, "cgroup.controllers")?;
    let num = |s: Option<String>| s.filter(|v| v != "max").and_then(|v| v.parse::<u64>().ok());
    let cpus = read(dir, "cpu.max").and_then(|v| {
        let mut parts = v.split_whitespace();
        let quota = parts.next()?;
        let period: f64 = parts
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(100_000.0);
        (quota != "max")
            .then(|| quota.parse::<f64>().ok().map(|q| q / period))
            .flatten()
    });
    let usage = read(dir, "cpu.stat").and_then(|s| {
        s.lines().find_map(|l| {
            l.strip_prefix("usage_usec ")
                .and_then(|v| v.trim().parse().ok())
        })
    });
    Some(CgroupLimits {
        memory_max: num(read(dir, "memory.max")),
        memory_current: num(read(dir, "memory.current")),
        cpus,
        cpu_usage_usec: usage,
    })
}

/// Whether we run in a container (Docker, Podman, Kubernetes).
fn in_container() -> bool {
    Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists()
        || std::env::var_os("KUBERNETES_SERVICE_HOST").is_some()
        || std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|s| {
            s.lines()
                .any(|l| l.contains("docker") || l.contains("kubepods") || l.contains("containerd"))
        })
}

#[derive(Debug)]
struct State {
    sys: Option<System>,
    disks: Option<Disks>,
    nets: Option<Networks>,
    last_cgroup_cpu: Option<(Instant, u64)>,
}

/// Host metrics for the filesystem of `data_dir`.
#[derive(Debug)]
pub struct Host {
    data_dir: PathBuf,
    cgroup_dir: PathBuf,
    container: bool,
    pid: Option<Pid>,
    /// Opened on the first sample: reading disks and interfaces is slow.
    state: Mutex<Option<State>>,
}

impl Host {
    /// Measures the host, the cgroup at `/sys/fs/cgroup` and the disk
    /// holding `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self::with_cgroup_dir(data_dir, "/sys/fs/cgroup")
    }

    /// Like [`Host::new`] with another cgroup directory (tests).
    pub fn with_cgroup_dir(data_dir: impl Into<PathBuf>, cgroup_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            cgroup_dir: cgroup_dir.into(),
            container: in_container(),
            pid: sysinfo::get_current_pid().ok(),
            state: Mutex::new(None),
        }
    }
}

impl HostMetrics for Host {
    fn sample(&self) -> HostSample {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let st = guard.get_or_insert_with(|| {
            let sys = System::new().ok().map(|mut s| {
                s.refresh_memory();
                s.refresh_cpu_usage();
                s
            });
            if sys.is_none() {
                tracing::warn!("host metrics are not available on this platform");
            }
            State {
                sys,
                disks: Disks::new_with_refreshed_list().ok(),
                nets: Networks::new_with_refreshed_list().ok(),
                last_cgroup_cpu: None,
            }
        });
        let mut out = HostSample {
            container: self.container,
            ..HostSample::default()
        };
        if let Some(sys) = st.sys.as_mut() {
            sys.refresh_memory();
            sys.refresh_cpu_usage();
            out.cpu_percent = f64::from(sys.global_cpu_usage());
            out.cpus = sys.cpus().len().max(1) as f64;
            out.load_avg = System::load_average().map_or([0.0; 3], |l| [l.one, l.five, l.fifteen]);
            out.memory_used = sys.used_memory();
            out.memory_total = sys.total_memory();
            out.host_memory_total = sys.total_memory();
            if let Some(pid) = self.pid {
                sys.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    false,
                    ProcessRefreshKind::nothing().with_memory().with_cpu(),
                );
                if let Some(p) = sys.process(pid) {
                    out.process_rss = p.memory();
                    out.process_cpu_seconds = p.accumulated_cpu_time() as f64 / 1000.0;
                    out.open_fds = p.open_files().map(|n| n as u64);
                }
            }
        }
        if let Some(disks) = st.disks.as_mut() {
            disks.refresh(true);
            // The disk whose mount point is the longest prefix of the data dir.
            let data =
                std::fs::canonicalize(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
            if let Some(d) = disks
                .iter()
                .filter(|d| data.starts_with(d.mount_point()))
                .max_by_key(|d| d.mount_point().as_os_str().len())
            {
                out.disk_total = d.total_space();
                out.disk_free = d.available_space();
            }
        }
        if let Some(nets) = st.nets.as_mut() {
            nets.refresh(true);
            for n in nets.values() {
                out.net_rx += n.total_received();
                out.net_tx += n.total_transmitted();
            }
        }
        if let Some(cg) = read_cgroup(&self.cgroup_dir) {
            if let Some(max) = cg.memory_max {
                out.container = true;
                // A limit above the host's RAM cannot be reached.
                out.memory_total = if out.host_memory_total > 0 {
                    max.min(out.host_memory_total)
                } else {
                    max
                };
            }
            if let Some(cur) = cg.memory_current
                && cg.memory_max.is_some()
            {
                out.memory_used = cur;
            }
            if let Some(cpus) = cg.cpus {
                out.container = true;
                out.cpus = cpus;
            }
            // CPU usage of the cgroup since the last sample, against its quota.
            if let Some(usage) = cg.cpu_usage_usec
                && out.container
            {
                let now = Instant::now();
                if let Some((then, prev)) = st.last_cgroup_cpu {
                    let wall = now.duration_since(then).as_secs_f64();
                    if wall > 0.0 {
                        let used = usage.saturating_sub(prev) as f64 / 1_000_000.0;
                        out.cpu_percent = (used / (wall * out.cpus) * 100.0).clamp(0.0, 100.0);
                    }
                }
                st.last_cgroup_cpu = Some((now, usage));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn cgroup_files_are_read() {
        let dir = std::env::temp_dir().join(format!("rk-cgroup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cgroup.controllers"), "cpu memory").unwrap();
        std::fs::write(dir.join("memory.max"), "268435456\n").unwrap();
        std::fs::write(dir.join("memory.current"), "1048576\n").unwrap();
        std::fs::write(dir.join("cpu.max"), "150000 100000\n").unwrap();
        std::fs::write(dir.join("cpu.stat"), "usage_usec 42\nuser_usec 1\n").unwrap();
        let cg = read_cgroup(&dir).unwrap();
        assert_eq!(cg.memory_max, Some(268_435_456));
        assert_eq!(cg.memory_current, Some(1_048_576));
        assert_eq!(cg.cpus, Some(1.5));
        assert_eq!(cg.cpu_usage_usec, Some(42));

        let host = Host::with_cgroup_dir(".", &dir);
        let s = host.sample();
        assert!(s.container);
        assert_eq!(
            s.memory_total, 268_435_456,
            "the container limit, not host RAM"
        );
        assert_eq!(s.memory_used, 1_048_576);
        assert!(s.host_memory_total > s.memory_total || s.host_memory_total > 0);
        assert!((s.cpus - 1.5).abs() < f64::EPSILON);

        std::fs::write(dir.join("memory.max"), "max\n").unwrap();
        std::fs::write(dir.join("cpu.max"), "max 100000\n").unwrap();
        let cg = read_cgroup(&dir).unwrap();
        assert_eq!((cg.memory_max, cg.cpus), (None, None));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(read_cgroup(&dir).is_none(), "no cgroup v2");
    }

    #[test]
    fn host_sample_has_numbers() {
        let s = Host::new(".").sample();
        assert!(s.host_memory_total > 0);
        assert!(s.disk_total > 0, "the data dir's disk is found");
        assert!(s.process_rss > 0);
    }
}
