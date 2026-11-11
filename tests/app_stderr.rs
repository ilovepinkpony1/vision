//! The redirect of standard error, in a test binary of its own because it moves file
//! descriptor 2 of the whole process: while the app code runs, the text goes to the log file of
//! the app, and afterwards file descriptor 2 is back, also after a panic.

use vision::app::stderr;

/// The device and the inode that file descriptor 2 points at.
fn fd2() -> (u64, u64) {
    // SAFETY: fstat writes into a local value.
    unsafe {
        let mut st: libc::stat = std::mem::zeroed();
        assert_eq!(libc::fstat(2, &mut st), 0);
        (st.st_dev as u64, st.st_ino as u64)
    }
}

/// Writes to file descriptor 2 itself: the test harness catches only the print macros.
fn write_fd2(text: &str) {
    // SAFETY: writes the bytes of a live string to a descriptor of this process.
    unsafe {
        libc::write(2, text.as_ptr().cast(), text.len());
    }
}

#[test]
fn file_descriptor_2_is_back_after_the_app_code_ran() {
    let before = fd2();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("app.err");
    {
        let _redirect = stderr::redirect(&log).unwrap();
        assert_ne!(
            fd2(),
            before,
            "file descriptor 2 points at the log while the app runs"
        );
        write_fd2("warning: a library speaks\n");
    }
    assert_eq!(fd2(), before);
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text, "warning: a library speaks\n");

    // A panic in the app code gives file descriptor 2 back too.
    let result = std::panic::catch_unwind(|| {
        let _redirect = stderr::redirect(&log).unwrap();
        write_fd2("before the panic\n");
        panic!("the app code failed");
    });
    assert!(result.is_err());
    assert_eq!(fd2(), before);
    assert!(std::fs::read_to_string(&log)
        .unwrap()
        .ends_with("before the panic\n"));

    // A home that is not set up has no log folder: the text goes nowhere.
    let paths = vision::paths::Paths::new(dir.path().join("missing"));
    assert_eq!(stderr::log_path(&paths).as_os_str(), "/dev/null");
}
