// /stats: memory and CPU used by claude-jr and the processes it started,
// read straight from /proc (Linux only).

use std::time::Duration;

struct Proc {
    name: String,
    rss_kb: u64,
    peak_kb: u64,
    threads: u64,
    cpu: Duration,
}

/// Reads /proc/<pid> ("self" for us).
fn read(pid: &str) -> Option<Proc> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |key: &str| {
        status.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.split_whitespace().next()?.parse().ok())
    };
    let name = status.lines().find_map(|l| l.strip_prefix("Name:")).unwrap_or("").trim().to_string();

    // utime and stime are fields 14 and 15; the command name before them
    // can contain spaces, so count from the closing parenthesis.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 2..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    let ticks: u64 = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    let cpu = Duration::from_millis(ticks * 1000 / clock_ticks());

    Some(Proc {
        name,
        rss_kb: field("VmRSS:").unwrap_or(0),
        peak_kb: field("VmHWM:").unwrap_or(0),
        threads: field("Threads:").unwrap_or(0),
        cpu,
    })
}

/// Kernel clock ticks per second. 100 on every common Linux setup; reading it
/// properly needs libc's sysconf, which isn't worth a dependency.
fn clock_ticks() -> u64 {
    100
}

pub fn report(uptime: Duration, children: &[u32]) -> String {
    let Some(me) = read("self") else { return "Stats need Linux (/proc).".into() };
    let busy = me.cpu.as_secs_f64() / uptime.as_secs_f64().max(0.001) * 100.0;
    let mut s = format!(
        "claude-jr     {:>6} memory (peak {})  {} threads  {:.2}s CPU in {} ({busy:.2}% average)",
        kb(me.rss_kb),
        kb(me.peak_kb),
        me.threads,
        me.cpu.as_secs_f64(),
        clock(uptime),
    );
    let mut total = me.rss_kb;
    for pid in children {
        if let Some(p) = read(&pid.to_string()) {
            total += p.rss_kb;
            s.push_str(&format!(
                "\n{:<13} {:>6} memory  {:.2}s CPU  (pid {pid})",
                p.name,
                kb(p.rss_kb),
                p.cpu.as_secs_f64()
            ));
        }
    }
    if !children.is_empty() {
        s.push_str(&format!("\ntotal         {:>6}", kb(total)));
    }
    s
}

fn kb(k: u64) -> String {
    if k < 1024 { format!("{k} KB") } else { format!("{:.1} MB", k as f64 / 1024.0) }
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_own_process() {
        let me = super::read("self").unwrap();
        assert!(me.rss_kb > 0 && me.threads > 0);
        assert!(super::report(std::time::Duration::from_secs(1), &[]).starts_with("claude-jr"));
    }
}
