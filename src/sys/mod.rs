// What depends on the operating system: files and processes (os.rs), the
// shell commands run in, the sandbox (Linux), handing unused memory back
// (Linux), and exit signals.

pub mod mem;
pub mod os;
pub mod sandbox;
pub mod shell;
pub mod signals;
