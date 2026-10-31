//! The command-line program, run as a user would run it.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

fn mctl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mctl"))
}

#[test]
fn version_prints_the_package_version() {
    let out = mctl().arg("--version").output().expect("run mctl");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}

#[test]
fn about_prints_the_licence_the_credits_and_the_notices() {
    let out = mctl().arg("about").output().expect("run mctl");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf-8");
    for needle in [
        "GNU General Public License",
        "Liam Droog",
        "Claude",
        "Third-party notices",
        "MIT License",
        "SIL Open Font License",
    ] {
        assert!(text.contains(needle), "`mctl about` lacks {needle:?}");
    }
}

/// `mctl about | head` closes the pipe long before the notices end; that must not be reported as a crash.
#[test]
fn about_is_not_a_crash_when_its_reader_goes_away() {
    let mut child = mctl()
        .arg("about")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run mctl");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut first = String::new();
    BufReader::new(stdout)
        .read_line(&mut first)
        .expect("read a line");
    assert!(first.starts_with("mctl "), "{first:?}");
    // The reader is dropped here with most of the output (far more than a pipe holds) unread.
    let out = child.wait_with_output().expect("wait for mctl");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked"), "mctl panicked: {err}");
    assert!(out.status.success(), "exit status {:?}: {err}", out.status);
}
