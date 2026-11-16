// --update, against a release server of the test's own.

use super::common::*;

/// A release server: answers GET /<name> with `files[name]`, anything else with 404.
fn releases(files: Vec<(&'static str, Vec<u8>)>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in l.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
            let path = line.split_whitespace().nth(1).unwrap_or("").trim_start_matches('/').to_string();
            let reply = match files.iter().find(|(name, _)| *name == path) {
                Some((_, body)) => {
                    let mut r =
                        format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len())
                            .into_bytes();
                    r.extend_from_slice(body);
                    r
                }
                None => b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
            };
            let _ = stream.write_all(&reply);
        }
    });
    base
}

#[test]
fn update_replaces_the_binary_with_a_newer_release() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    // A copy, so the real binary under test is never replaced.
    let exe = e.root.join("claudejr-copy");
    std::fs::copy(env!("CARGO_BIN_EXE_claudejr"), &exe).unwrap();
    let asset: &'static str =
        Box::leak(format!("claudejr-{}-{}", std::env::consts::OS, std::env::consts::ARCH).into_boxed_str());
    let current = env!("CARGO_PKG_VERSION");
    let update = |base: &str| {
        let mut cmd = Command::new(&exe);
        cmd.arg("--update").env_clear().env("PATH", std::env::var("PATH").unwrap()).env("CLAUDE_JR_RELEASES", base);
        // Coverage runs (bench/coverage.sh) collect the binary's profile through this.
        if let Some(p) = std::env::var_os("LLVM_PROFILE_FILE") {
            cmd.env("LLVM_PROFILE_FILE", p);
        }
        exec(cmd)
    };
    let version = |exe: &Path| String::from_utf8(Command::new(exe).arg("--version").output().unwrap().stdout).unwrap();

    // Nothing newer: nothing changes.
    let o = update(&releases(vec![("version", current.as_bytes().to_vec())]));
    ok(&o);
    assert!(o.out.contains(&format!("claude-jr {current} is the newest version.")), "{}", o.out);

    // A download that isn't the promised version is refused and the binary is left alone.
    let wrong = b"#!/bin/sh\necho claude-jr 1.2.3\n".to_vec();
    let o = update(&releases(vec![("version", b"9.9.9\n".to_vec()), (asset, wrong)]));
    assert_eq!(o.code, 2, "{}", o.err);
    assert!(o.err.contains("the download isn't claude-jr 9.9.9"), "{}", o.err);
    assert_eq!(version(&exe).trim(), format!("claude-jr {current}"));
    assert!(!exe.with_extension("update").exists(), "the failed download is removed");

    // A missing release file is an error too.
    let o = update(&releases(vec![("version", b"9.9.9".to_vec())]));
    assert_eq!(o.code, 2);
    assert_eq!(version(&exe).trim(), format!("claude-jr {current}"));

    // A newer release replaces the binary.
    let newer = b"#!/bin/sh\necho claude-jr 9.9.9\n".to_vec();
    let o = update(&releases(vec![("version", b"v9.9.9\n".to_vec()), (asset, newer)]));
    ok(&o);
    assert!(o.out.contains(&format!("Updated claude-jr {current} to 9.9.9.")), "{}", o.out);
    assert_eq!(version(&exe).trim(), "claude-jr 9.9.9");
    assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111, 0o111, "still executable");
}
