// /stats: memory and CPU used by claude-jr and the processes it started,
// read straight from /proc on Linux and asked of `ps` elsewhere.

use std::time::Duration;

struct Proc {
    name: String,
    rss_kb: u64,
    peak_kb: u64,
    threads: u64,
    cpu: Duration,
}

/// What is known about process `pid` ("self" for us).
fn read(pid: &str) -> Option<Proc> {
    read_proc(pid).or_else(|| read_ps(pid))
}

/// Where there is no /proc (macOS): what `ps` says. It doesn't know the
/// peak or, in a form every system shares, the number of threads.
fn read_ps(pid: &str) -> Option<Proc> {
    let pid = if pid == "self" { std::process::id().to_string() } else { pid.to_string() };
    let out = std::process::Command::new("ps").args(["-o", "rss=,time=,comm=", "-p", &pid]).output().ok()?;
    let line = String::from_utf8_lossy(&out.stdout);
    let mut fields = line.split_whitespace();
    let rss_kb: u64 = fields.next()?.parse().ok()?;
    let cpu = cpu_time(fields.next()?)?;
    let name = fields.next()?.rsplit('/').next()?.to_string();
    Some(Proc { name, rss_kb, peak_kb: rss_kb, threads: 0, cpu })
}

/// CPU time as `ps` prints it: "0:01.50" (minutes, seconds) on macOS,
/// "00:00:01" (hours, minutes, seconds) on Linux, with days in front
/// ("1-02:03:04") for a long-lived process.
fn cpu_time(text: &str) -> Option<Duration> {
    let (days, rest) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<f64>().ok()?, rest),
        None => (0.0, text),
    };
    let mut seconds = days * 86400.0;
    let parts: Vec<&str> = rest.split(':').collect();
    for (i, part) in parts.iter().rev().enumerate() {
        seconds += part.parse::<f64>().ok()? * [1.0, 60.0, 3600.0].get(i)?;
    }
    Some(Duration::from_secs_f64(seconds))
}

/// Reads /proc/<pid>.
fn read_proc(pid: &str) -> Option<Proc> {
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
    let Some(me) = read("self") else { return "Couldn't read this process's memory and CPU use.".into() };
    let busy = me.cpu.as_secs_f64() / uptime.as_secs_f64().max(0.001) * 100.0;
    let threads = if me.threads > 0 { format!("{} threads  ", me.threads) } else { String::new() };
    let mut s = format!(
        "claude-jr     {:>6} memory (peak {})  {threads}{:.2}s CPU in {} ({busy:.2}% average)",
        kb(me.rss_kb),
        kb(me.peak_kb),
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
        assert!(me.rss_kb > 0);
        assert!(super::report(std::time::Duration::from_secs(1), &[]).starts_with("claude-jr"));
    }

    /// The way it is read where there is no /proc; `ps` is on Linux too.
    #[test]
    fn ps_can_be_asked_instead_of_proc() {
        let me = super::read_ps("self").expect("ps knows this process");
        assert!(me.rss_kb > 500 && me.peak_kb == me.rss_kb && !me.name.is_empty(), "{} {}", me.rss_kb, me.name);
        assert!(super::read_ps(&std::process::id().to_string()).is_some());
        assert!(super::read_ps("4294967294").is_none() && super::read_ps("no-such-pid").is_none());
        let secs = |t: &str| super::cpu_time(t).map(|d| d.as_secs_f64());
        assert_eq!(secs("0:01.50"), Some(1.5));
        assert_eq!(secs("00:02:03"), Some(123.0));
        assert_eq!(secs("1-00:00:01"), Some(86401.0));
        assert_eq!((secs("soon"), secs("1:2:3:4")), (None, None));
    }

    #[test]
    fn children_are_listed_with_a_total() {
        let me = std::process::id();
        let report = super::report(std::time::Duration::from_secs(90), &[me, u32::MAX - 1]);
        let lines: Vec<&str> = report.lines().collect();
        assert!(lines[0].contains("1m 30s") && lines[0].contains("memory (peak"), "{report}");
        // The live process is shown; the one that doesn't exist is skipped without a fuss.
        assert_eq!(lines.len(), 3, "{report}");
        assert!(lines[1].contains(&format!("(pid {me})")) && lines[1].contains("memory"), "{report}");
        assert!(lines[2].starts_with("total"), "{report}");
        let alone = super::report(std::time::Duration::from_secs(1), &[]);
        assert!(!alone.contains("total"), "no children, no total");
    }

    #[test]
    fn sizes_and_durations_are_short() {
        assert_eq!(super::kb(0), "0 KB");
        assert_eq!(super::kb(1023), "1023 KB");
        assert_eq!(super::kb(1024), "1.0 MB");
        assert_eq!(super::kb(2560), "2.5 MB");
        assert_eq!(super::clock(std::time::Duration::from_secs(5)), "5s");
        assert_eq!(super::clock(std::time::Duration::from_secs(59)), "59s");
        assert_eq!(super::clock(std::time::Duration::from_secs(60)), "1m 00s");
        assert_eq!(super::clock(std::time::Duration::from_secs(3725)), "62m 05s");
        assert!(super::read("no-such-pid").is_none());
    }
}
