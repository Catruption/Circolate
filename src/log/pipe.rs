// src/log/pipe.rs
use std::io::{BufRead, BufReader};
use std::os::unix::io::FromRawFd;

/// Redirects process-wide `stderr` (fd 2) into the Circolate logger.
/// This is required to capture C++ libraries (like clap-wrapper) 
/// that print directly to the terminal, which corrupts the TUI.
pub fn redirect_stderr_to_logger() {
    unsafe {
        let mut fds = [0; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return; // Pipe failed, leave stderr alone
        }
        
        // Duplicate the write end of the pipe onto stderr (fd 2)
        libc::dup2(fds[1], 2);
        libc::close(fds[1]);

        // Turn the read end of the pipe into a Rust file
        let file = std::fs::File::from_raw_fd(fds[0]);
        let reader = BufReader::new(file);

        // Spawn a background thread to read the C++ spam
        std::thread::spawn(move || {
            for line in reader.lines().map_while(Result::ok) {
                // Ignore empty lines
                if line.trim().is_empty() { continue; }
                
                // Route it into your TUI log!
                crate::log_event!("ffi.stderr", line: line);
            }
        });
    }
}