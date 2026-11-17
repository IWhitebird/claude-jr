// The few things files and processes do differently on Unix and on Windows:
// reading a file at an offset, telling two files apart, keeping a file to
// its owner, and starting, finding and stopping processes.

use std::fs::{File, OpenOptions};
use std::io;
use std::process::Command;

/// Reads into `buf` from `at`, without moving the file's own position.
#[cfg(unix)]
pub fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, at)
}

#[cfg(windows)]
pub fn read_at(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    match std::os::windows::fs::FileExt::seek_read(file, buf, at) {
        // Windows reports the end of a file as an error here.
        Err(e) if e.raw_os_error() == Some(38) => Ok(0),
        other => other,
    }
}

pub fn read_exact_at(file: &File, mut buf: &mut [u8], mut at: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(file, buf, at) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                at += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn write_all_at(file: &File, mut bytes: &[u8], mut at: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        #[cfg(unix)]
        let wrote = std::os::unix::fs::FileExt::write_at(file, bytes, at);
        #[cfg(windows)]
        let wrote = std::os::windows::fs::FileExt::seek_write(file, bytes, at);
        match wrote {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                bytes = &bytes[n..];
                at += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Two numbers that are the same for the same file and differ between files.
#[cfg(unix)]
pub fn file_id(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(windows)]
pub fn file_id(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    let mut info = win::FileInfo::default();
    // SAFETY: the handle is this open file's, and `info` is the struct the call fills in.
    if unsafe { win::GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.volume as u64, (info.index_high as u64) << 32 | info.index_low as u64))
}

/// Makes a file that is about to be created readable by its owner only
/// (`mode` on Unix; on Windows a file in the user's folder already is).
pub fn owner_only(options: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(options, mode);
    #[cfg(windows)]
    let _ = mode;
    options
}

/// The same for a file that already exists.
pub fn keep_to_owner(file: &File, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    return file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode));
    #[cfg(windows)]
    {
        let _ = (file, mode);
        Ok(())
    }
}

/// Starts `cmd` apart from us, so that a Ctrl+C meant for claude-jr doesn't
/// reach it and it can be stopped together with everything it starts.
/// `detached`: it also outlives the terminal it was started from.
pub fn apart(cmd: &mut Command, detached: bool) -> &mut Command {
    #[cfg(unix)]
    {
        let _ = detached;
        std::os::unix::process::CommandExt::process_group(cmd, 0);
    }
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        let flags = CREATE_NEW_PROCESS_GROUP | if detached { DETACHED_PROCESS } else { 0 };
        std::os::windows::process::CommandExt::creation_flags(cmd, flags);
    }
    cmd
}

/// Kills a process started with `apart` and everything it started.
pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Whether a process with this id exists.
#[cfg(unix)]
pub fn alive(pid: u64) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: signal 0 sends nothing; it only asks whether the process exists.
    pid > 0 && unsafe { kill(pid as i32, 0) } == 0
}

#[cfg(windows)]
pub fn alive(pid: u64) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const STILL_RUNNING: u32 = 258; // WAIT_TIMEOUT
    if pid == 0 {
        return false;
    }
    // SAFETY: plain system calls on a handle that is opened and closed here.
    unsafe {
        let handle = win::OpenProcess(SYNCHRONIZE, 0, pid as u32);
        if handle.is_null() {
            return false;
        }
        let running = win::WaitForSingleObject(handle, 0) == STILL_RUNNING;
        win::CloseHandle(handle);
        running
    }
}

/// The Windows calls used above and in signals.rs, declared here so that no
/// extra crate is needed for them.
#[cfg(windows)]
pub mod win {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    pub struct FileInfo {
        pub attributes: u32,
        pub times: [u32; 6],
        pub volume: u32,
        pub size_high: u32,
        pub size_low: u32,
        pub links: u32,
        pub index_high: u32,
        pub index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetFileInformationByHandle(file: *mut c_void, info: *mut FileInfo) -> i32;
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        pub fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
        pub fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_read_and_written_at_an_offset_and_told_apart() {
        let dir = std::env::temp_dir().join(format!("cjr-os-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::write(&a, "0123456789").unwrap();
        std::fs::write(&b, "").unwrap();
        let file = OpenOptions::new().read(true).write(true).open(&a).unwrap();
        let mut four = [0u8; 4];
        read_exact_at(&file, &mut four, 3).unwrap();
        assert_eq!(&four, b"3456");
        assert_eq!(read_at(&file, &mut four, 8).unwrap(), 2, "what is left");
        assert_eq!(read_at(&file, &mut four, 10).unwrap(), 0, "the end is not an error");
        assert_eq!(read_exact_at(&file, &mut four, 8).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        write_all_at(&file, b"ab", 9).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "012345678ab");

        let again = File::open(&a).unwrap();
        assert_eq!(file_id(&file).unwrap(), file_id(&again).unwrap());
        assert_ne!(file_id(&file).unwrap(), file_id(&File::open(&b).unwrap()).unwrap());

        let private = dir.join("private");
        let made = owner_only(OpenOptions::new().create(true).write(true), 0o600).open(&private).unwrap();
        keep_to_owner(&made, 0o600).unwrap();
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&made.metadata().unwrap().permissions()) & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_is_found_while_it_runs_and_killed_with_what_it_started() {
        assert!(alive(std::process::id() as u64));
        assert!(!alive(0));
        let shell = crate::sys::shell::shell();
        let mut child = apart(&mut shell.command("sleep 30 & sleep 30"), false).spawn().unwrap();
        let pid = child.id();
        assert!(alive(pid as u64));
        kill_tree(pid);
        let _ = child.wait();
        assert!(!alive(pid as u64));
    }
}
