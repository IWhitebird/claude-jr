// --sandbox keeps writes inside the project.

use super::common::*;

#[test]
fn sandbox_confines_writes() {
    if !bwrap_works() {
        eprintln!("bwrap is not usable here; sandbox_error_paths covers the error side");
        return;
    }
    let outside = PathBuf::from(format!("/var/tmp/cjr-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    let target = outside.join("new.txt");
    let cmd = format!("touch {}; echo ok > inside.txt", target.display());
    let e = Env::with(&[("MOCK_ROUNDS", "1"), ("MOCK_CMD", &cmd)]);
    let o = e.run(&["--sandbox", "-y", "-p", "try to escape"]);
    let escaped = target.exists();
    let _ = std::fs::remove_dir_all(&outside);
    ok(&o);
    assert!(!escaped, "wrote outside the project");
    assert!(e.proj.join("inside.txt").exists(), "write inside the project failed: {}", o.err);
    assert!(e.requests_text().contains("Read-only file system"), "touch should have failed: {}", o.err);
}
