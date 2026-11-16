// Keeps resident memory down to what is in use.
//
// The kernel maps a program's pages in as they are first touched, plus their
// neighbours, and leaves them mapped: everything start-up or a finished turn
// needed stays counted against the process. Those that were never changed are
// plain copies of the file on disk, so they can be handed back at any time
// and come back, from the page cache, the moment they're needed again.

/// Hands back the program's own pages. Called whenever the app goes quiet,
/// so it has to be cheap: the regions are worked out once, and after that it
/// is one system call for each.
#[cfg(target_os = "linux")]
pub fn trim() {
    const MADV_DONTNEED: i32 = 4;
    unsafe extern "C" {
        fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
    }
    static REGIONS: std::sync::OnceLock<Vec<(usize, usize)>> = std::sync::OnceLock::new();
    for &(start, len) in REGIONS.get_or_init(regions) {
        // SAFETY: the range is a mapping of this program's file, which stays
        // mapped for the life of the process, and every page in it is still
        // the file's own; dropping them loses nothing, the next access reads
        // them back.
        unsafe { madvise(start as *mut u8, len, MADV_DONTNEED) };
    }
}

/// The parts of the program's file that are mapped read-only and unchanged,
/// as (start, length).
#[cfg(target_os = "linux")]
fn regions() -> Vec<(usize, usize)> {
    let Ok(maps) = std::fs::read_to_string("/proc/self/smaps") else { return Vec::new() };
    let here = regions as *const () as usize;
    let mut ranges: Vec<(usize, usize, &str, &str, bool)> = Vec::new();
    for line in maps.lines() {
        if let Some((start, end, perms, file)) = range(line) {
            ranges.push((start, end, perms, file, true));
        } else if line.as_bytes().first().is_some_and(u8::is_ascii_hexdigit) && line.contains('-') {
            // A region with no file (heap, stacks): what follows is about it.
            ranges.push((0, 0, "", "", false));
        } else if line.starts_with("Anonymous:") && !line.ends_with(" 0 kB") {
            // Pages the loader patched (addresses filled in at start-up) are
            // no longer the file's: this region must stay as it is.
            if let Some(last) = ranges.last_mut() {
                last.4 = false;
            }
        }
    }
    // The program is whichever file this function was loaded from.
    let Some(&(_, _, _, program, _)) = ranges.iter().find(|(start, end, ..)| (*start..*end).contains(&here)) else {
        return Vec::new();
    };
    ranges
        .iter()
        .filter(|(_, _, perms, file, untouched)| *file == program && *untouched && !perms.contains('w'))
        .map(|(start, end, ..)| (*start, end - start))
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn trim() {}

/// A region's first line in /proc/self/smaps, if it maps a file: start, end, permissions, file.
#[cfg(target_os = "linux")]
fn range(line: &str) -> Option<(usize, usize, &str, &str)> {
    let mut fields = line.splitn(6, ' ');
    let (start, end) = fields.next()?.split_once('-')?;
    let perms = fields.next()?;
    let file = fields.nth(3)?.trim_start();
    if !file.starts_with('/') {
        return None;
    }
    Some((usize::from_str_radix(start, 16).ok()?, usize::from_str_radix(end, 16).ok()?, perms, file))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn program_kb() -> u64 {
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
        let exe = std::env::current_exe().unwrap();
        let (mut ours, mut kb) = (false, 0);
        for line in smaps.lines() {
            if let Some((_, _, perms, file)) = range(line) {
                ours = file == exe.to_str().unwrap() && !perms.contains('w');
            } else if line.as_bytes()[0].is_ascii_hexdigit() && line.contains('-') {
                ours = false;
            } else if ours && line.starts_with("Rss:") {
                kb += line.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap();
            }
        }
        kb
    }

    #[test]
    fn reads_a_maps_line() {
        let line = "55d0c8a00000-55d0c8a5c000 r-xp 00002000 103:02 1234   /usr/bin/claudejr";
        assert_eq!(range(line), Some((0x55d0c8a00000, 0x55d0c8a5c000, "r-xp", "/usr/bin/claudejr")));
        assert_eq!(range("7ffc1000-7ffc2000 rw-p 00000000 00:00 0   [stack]"), None);
        assert_eq!(range("7f001000-7f002000 rw-p 00000000 00:00 0 "), None);
        assert_eq!(range("Rss:                 148 kB"), None);
    }

    #[test]
    fn trimming_shrinks_the_program_and_it_keeps_working() {
        let before = program_kb();
        trim();
        let after = program_kb();
        assert!(after < before, "{after} kB after, {before} kB before");
        // Everything still runs: the pages come back as they're used.
        assert_eq!(format!("{:.1}", 2.5f64.sqrt()), "1.6");
    }
}
