//! Capture everything written to fd 2 (Rust `eprintln!` *and* C++
//! `fprintf(stderr, …)` from VST3 plugins) and route it into the log
//! bus as `ffi.stderr`.  Without this, any diagnostic corrupts the
//! alternate screen — DPF plugins in particular print teardown warnings
//! every time a track is unloaded.
//!
//! After the call-site migration finishes, Rust no longer touches fd 2
//! in TUI mode, so this capture only carries C++ output.  It feeds the
//! bus uniformly; the widget reads from `log::ring()`.

use std::os::unix::io::RawFd;
use std::sync::{Mutex, OnceLock};

static CAPTURE: OnceLock<Mutex<Option<StderrCapture>>> = OnceLock::new();

pub struct StderrCapture {
    saved_fd:   RawFd,
    pipe_read:  RawFd,
    pipe_write: RawFd,
    reader:     Option<std::thread::JoinHandle<()>>,
}

pub fn install() -> Option<()> {
    if std::env::var_os("CIRCOLATE_NO_LOG_CAPTURE").is_some() {
        return None;
    }
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let cap = StderrCapture::install()?;
    let cell = CAPTURE.get_or_init(|| Mutex::new(None));
    *cell.lock().ok()? = Some(cap);
    Some(())
}

pub fn uninstall() {
    if let Some(cell) = CAPTURE.get() {
        if let Ok(mut g) = cell.lock() {
            *g = None;
        }
    }
}

impl StderrCapture {
    fn install() -> Option<Self> {
        // Setup: all fd manipulation in one tight `unsafe` block.
        let (pipe_read, pipe_write, saved_fd) = unsafe {
            let mut fds = [0i32; 2];
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return None;
            }
            let pipe_read = fds[0];
            let pipe_write = fds[1];

            let saved_fd = libc::dup(2);
            if saved_fd < 0 {
                libc::close(pipe_read);
                libc::close(pipe_write);
                return None;
            }

            if libc::dup2(pipe_write, 2) < 0 {
                libc::close(pipe_read);
                libc::close(pipe_write);
                libc::close(saved_fd);
                return None;
            }

            (pipe_read, pipe_write, saved_fd)
        };

        // Reader thread.  Each line goes through the log bus as
        // `ffi.stderr`; in TUI mode there is no StderrSink, so no loop.
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut carry = Vec::new();
            loop {
                let n = unsafe {
                    libc::read(pipe_read, buf.as_mut_ptr() as *mut _, buf.len())
                };
                if n <= 0 {
                    break;
                }
                carry.extend_from_slice(&buf[..n as usize]);
                while let Some(nl) = carry.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = carry.drain(..=nl).collect();
                    let text =
                    String::from_utf8_lossy(&line[..line.len().saturating_sub(1)]);
                    let clean = strip_ansi(&text);
                    if !clean.is_empty() {
                        crate::log_event!("ffi.stderr", line: clean);
                    }
                }
            }
            if !carry.is_empty() {
                let clean = strip_ansi(&String::from_utf8_lossy(&carry));
                if !clean.is_empty() {
                    crate::log_event!("ffi.stderr", line: clean);
                }
            }
            unsafe {
                libc::close(pipe_read);
            }
        });

        Some(StderrCapture {
            saved_fd,
            pipe_read,
            pipe_write,
            reader: Some(reader),
        })
    }
}

impl Drop for StderrCapture {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved_fd, 2);
            libc::close(self.saved_fd);
            libc::close(self.pipe_write);
        }
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }
}

/// Drop ANSI escapes and C0 control characters so log lines are safe to
/// render in a ratatui paragraph.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            i += 1;
        } else if bytes[i] < 0x20 && bytes[i] != b'\t' {
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
