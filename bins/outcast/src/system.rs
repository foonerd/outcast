//! The machine under the uplinks, read once a second from what the kernel
//! keeps anyway: the whole player's CPU and load, the core's own share and its
//! encoders', memory, the processor's temperature where there is a sensor, and
//! what goes out: the uplinks' total, and the player's interface. No process
//! is run, no library used; a handful of small files under `/proc` and `/sys`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use crate::encoder;
use crate::status::Status;

/// One reading, as the status carries it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct System {
    /// The whole player's CPU busy, percent of all cores.
    pub cpu_pct: f32,
    pub cores: u32,
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    /// The core and its encoders together, percent of all cores.
    pub outcast_cpu_pct: f32,
    /// The encoders alone, percent of all cores.
    pub encoders_cpu_pct: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    /// The core's resident memory with its encoders'.
    pub outcast_rss_mb: u64,
    /// The processor's temperature; none without a sensor.
    pub temp_c: Option<f32>,
    /// What every uplink sent in the last second, together.
    pub uplinks_kbps: f32,
    /// What the player's interface sent in the last second.
    pub net_tx_kbps: f32,
    pub net_iface: String,
}

/// The reader, holding the last reading for the deltas.
pub struct Sampler {
    at: Option<Instant>,
    cpu: (u64, u64),
    own: u64,
    children: HashMap<u32, u64>,
    tx: u64,
    uplink_bytes: u64,
    thermal: Option<PathBuf>,
    clk_tck: f64,
    page: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            at: None,
            cpu: (0, 0),
            own: 0,
            children: HashMap::new(),
            tx: 0,
            uplink_bytes: 0,
            thermal: thermal_zone(),
            clk_tck: clk_tck(),
            page: page_size(),
        }
    }

    /// One reading taken and written into the status: the system, and each
    /// uplink's encoder cost. The first call only remembers; the deltas need two.
    pub fn sample(&mut self, status: &Arc<Mutex<Status>>) {
        let now = Instant::now();
        let stat = read("/proc/stat");
        let (busy, total, cores) = parse_stat(&stat).unwrap_or((0, 0, 1));
        let own = read("/proc/self/stat");
        let (own_ticks, own_rss) = parse_pid_stat(&own).unwrap_or((0, 0));
        let encoders: Vec<(String, u32)> = encoder::encoders()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let mut child_now: HashMap<u32, (u64, u64)> = HashMap::new();
        for (_, pid) in &encoders {
            if let Some(v) = parse_pid_stat(&read(&format!("/proc/{pid}/stat"))) {
                child_now.insert(*pid, v);
            }
        }
        let iface = default_iface(&read("/proc/net/route")).unwrap_or_default();
        let tx = parse_net_dev(&read("/proc/net/dev"), &iface).unwrap_or(0);
        let (mem_total, mem_available) = parse_meminfo(&read("/proc/meminfo")).unwrap_or((0, 0));
        let (load1, load5, load15) =
            parse_loadavg(&read("/proc/loadavg")).unwrap_or((0.0, 0.0, 0.0));
        let temp_c = self
            .thermal
            .as_ref()
            .and_then(|p| read(&p.to_string_lossy()).trim().parse::<f64>().ok())
            .map(|milli| (milli / 1000.0) as f32);
        let uplink_bytes: u64 = {
            let s = status.lock().unwrap_or_else(|e| e.into_inner());
            s.uplinks.iter().map(|u| u.bytes).sum()
        };

        if let Some(at) = self.at {
            let dt = now.duration_since(at).as_secs_f64().max(0.001);
            let cores_f = cores.max(1) as f64;
            let cpu_pct = {
                let dbusy = busy.saturating_sub(self.cpu.0) as f64;
                let dtotal = total.saturating_sub(self.cpu.1) as f64;
                if dtotal > 0.0 {
                    (100.0 * dbusy / dtotal) as f32
                } else {
                    0.0
                }
            };
            let pct_of_all =
                |dticks: u64| (100.0 * dticks as f64 / self.clk_tck / dt / cores_f) as f32;
            let own_pct = pct_of_all(own_ticks.saturating_sub(self.own));
            let mut per_uplink: HashMap<String, f32> = HashMap::new();
            let mut encoders_pct = 0.0f32;
            let mut rss_pages = own_rss;
            for (id, pid) in &encoders {
                if let Some((ticks, rss)) = child_now.get(pid) {
                    rss_pages += rss;
                    if let Some(before) = self.children.get(pid) {
                        let p = pct_of_all(ticks.saturating_sub(*before));
                        encoders_pct += p;
                        *per_uplink.entry(id.clone()).or_insert(0.0) += p;
                    }
                }
            }
            let reading = System {
                cpu_pct,
                cores,
                load1,
                load5,
                load15,
                outcast_cpu_pct: own_pct + encoders_pct,
                encoders_cpu_pct: encoders_pct,
                mem_used_mb: mem_total.saturating_sub(mem_available) / 1024,
                mem_total_mb: mem_total / 1024,
                outcast_rss_mb: rss_pages * self.page / 1_048_576,
                temp_c,
                uplinks_kbps: (uplink_bytes.saturating_sub(self.uplink_bytes) as f64 * 8.0
                    / dt
                    / 1000.0) as f32,
                net_tx_kbps: (tx.saturating_sub(self.tx) as f64 * 8.0 / dt / 1000.0) as f32,
                net_iface: iface,
            };
            let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
            for u in s.uplinks.iter_mut() {
                u.cpu_pct = per_uplink.get(&u.id).copied().unwrap_or(0.0);
            }
            s.system = Some(reading);
        }

        self.at = Some(now);
        self.cpu = (busy, total);
        self.own = own_ticks;
        self.children = child_now
            .into_iter()
            .map(|(pid, (t, _))| (pid, t))
            .collect();
        self.tx = tx;
        self.uplink_bytes = uplink_bytes;
    }
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn clk_tck() -> f64 {
    // SAFETY: sysconf reads a constant of the C library; no memory is touched.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        v as f64
    } else {
        100.0
    }
}

fn page_size() -> u64 {
    // SAFETY: as above.
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 {
        v as u64
    } else {
        4096
    }
}

/// The thermal zone that is the processor's, where the kernel offers one: by
/// the names the common boards give it, else the first zone there is.
fn thermal_zone() -> Option<PathBuf> {
    let zones = std::fs::read_dir("/sys/class/thermal").ok()?;
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in zones.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let kind = std::fs::read_to_string(entry.path().join("type"))
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        found.push((kind, entry.path().join("temp")));
    }
    found.sort();
    let preferred = [
        "cpu-thermal",
        "cpu_thermal",
        "soc-thermal",
        "soc_thermal",
        "x86_pkg_temp",
        "coretemp",
        "cpu",
    ];
    for p in preferred {
        if let Some((_, path)) = found.iter().find(|(k, _)| k == p) {
            return Some(path.clone());
        }
    }
    found.into_iter().next().map(|(_, p)| p)
}

/// `/proc/stat`: the whole machine's busy and total ticks, and the cores.
pub fn parse_stat(text: &str) -> Option<(u64, u64, u32)> {
    let mut cores = 0u32;
    let mut first: Option<(u64, u64)> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("cpu") {
            if rest.starts_with(' ') {
                let v: Vec<u64> = rest
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                if v.len() >= 4 {
                    let total: u64 = v.iter().sum();
                    let idle = v[3] + v.get(4).copied().unwrap_or(0);
                    first = Some((total.saturating_sub(idle), total));
                }
            } else if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                cores += 1;
            }
        }
    }
    first.map(|(busy, total)| (busy, total, cores.max(1)))
}

/// `/proc/loadavg`: the three averages.
pub fn parse_loadavg(text: &str) -> Option<(f32, f32, f32)> {
    let mut it = text.split_whitespace().map(|x| x.parse::<f32>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// `/proc/meminfo`: total and available, in kB.
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let value = || {
            line.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
        };
        if line.starts_with("MemTotal:") {
            total = value();
        } else if line.starts_with("MemAvailable:") {
            available = value();
        }
    }
    Some((total?, available?))
}

/// `/proc/<pid>/stat`: the process's user and system ticks together, and its
/// resident pages. The command's name is in parentheses and may hold spaces,
/// so the fields are counted from after the last closing one.
pub fn parse_pid_stat(text: &str) -> Option<(u64, u64)> {
    let rest = &text[text.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After the name: state is field 3, so index 0; utime 14 → 11, stime 15 → 12, rss 24 → 21.
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let rss: u64 = f.get(21)?.parse().ok()?;
    Some((utime + stime, rss))
}

/// `/proc/net/route`: the interface of the default route.
pub fn default_iface(text: &str) -> Option<String> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let iface = it.next()?;
            let dest = it.next()?;
            (dest == "00000000").then(|| iface.to_string())
        })
        .next()
}

/// `/proc/net/dev`: the bytes an interface has sent.
pub fn parse_net_dev(text: &str, iface: &str) -> Option<u64> {
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == iface {
            return rest.split_whitespace().nth(8).and_then(|v| v.parse().ok());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proc_files_parse() {
        let stat = "cpu  239060 51257 273779 9845692 170107 0 4128 0 0 47715\ncpu0 1 2 3 4 5 0 0 0 0 0\ncpu1 1 2 3 4 5 0 0 0 0 0\nintr 1 2 3\n";
        let (busy, total, cores) = parse_stat(stat).unwrap();
        assert_eq!(cores, 2);
        assert_eq!(
            total,
            239060 + 51257 + 273779 + 9845692 + 170107 + 4128 + 47715
        );
        assert_eq!(busy, total - 9845692 - 170107);
        assert_eq!(
            parse_loadavg("0.42 0.37 0.31 2/400 1234\n"),
            Some((0.42, 0.37, 0.31))
        );
        assert_eq!(
            parse_meminfo(
                "MemTotal:       3891000 kB\nMemFree: 1 kB\nMemAvailable:   3149000 kB\n"
            ),
            Some((3891000, 3149000))
        );
        let pid = "1234 (evopeg (x64)) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 3 0 100 50000000 9700 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(parse_pid_stat(pid), Some((300, 9700)));
        let route =
            "Iface\tDestination\tGateway\nwlan0\t0001A8C0\t00000000\neth0\t00000000\t0101A8C0\n";
        assert_eq!(default_iface(route).as_deref(), Some("eth0"));
        let dev = "Inter-|   Receive |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n    lo: 1 2 0 0 0 0 0 0 3 4 0 0 0 0 0 0\n  eth0: 626527087 533429 0 0 0 0 0 20 118552582 171555 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev(dev, "eth0"), Some(118552582));
        assert_eq!(parse_net_dev(dev, "wlan0"), None);
    }

    #[test]
    fn two_readings_make_a_system_with_the_deltas() {
        let status = Arc::new(Mutex::new(Status::new("test")));
        let mut sampler = Sampler::new();
        sampler.sample(&status);
        assert!(
            status.lock().unwrap().system.is_none(),
            "the first reading only remembers"
        );
        std::thread::sleep(std::time::Duration::from_millis(60));
        sampler.sample(&status);
        let s = status.lock().unwrap();
        let sys = s.system.as_ref().expect("a reading after two");
        assert!(sys.cores >= 1);
        assert!(sys.mem_total_mb > 0);
        assert!((0.0..=100.0).contains(&sys.cpu_pct));
    }
}
