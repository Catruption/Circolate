//! Capture everything written to fd 2 (Rust `eprintln!` *and* C++
//! `fprintf(stderr, …)` from VST3 plugins) into a ring buffer while the
//! TUI owns the terminal.  Without this, any diagnostic corrupts the
//! alternate screen — DPF plugins in particular print teardown warnings
//! every time a track is unloaded.

use std::collections::VecDeque;
use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex, OnceLock};

pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    pub fn push(&self, line: String) {
        let Ok(mut buf) = self.lines.lock() else { return };
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        let Ok(buf) = self.lines.lock() else { return Vec::new() };
        buf.iter().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.lock().map_or(true, |b| b.is_empty())
    }
}

static BUFFER: OnceLock<Arc<LogBuffer>> = OnceLock::new();
static CAPTURE: OnceLock<Mutex<Option<StderrCapture>>> = OnceLock::new();

pub fn buffer() -> Arc<LogBuffer> {
    BUFFER
    .get_or_init(|| Arc::new(LogBuffer::new(400)))
    .clone()
}

pub struct StderrCapture {
    saved_fd: RawFd,
    pipe_read: RawFd,
    pipe_write: RawFd,
    reader: Option<std::thread::JoinHandle<()>>,
}

pub fn install() -> Option<()> {
    let buf = buffer();
    let cap = StderrCapture::install(buf)?;
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
    fn install(buffer: Arc<LogBuffer>) -> Option<Self> {
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

        // Reader thread.  `libc::read` / `libc::close` get their own
        // scoped `unsafe` blocks — the outer one above has already ended,
        // so this isn't a nested `unsafe` and `#[warn(unused_unsafe)]`
        // stays quiet.
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
                    buffer.push(strip_ansi(&text));
                }
            }
            if !carry.is_empty() {
                buffer.push(strip_ansi(&String::from_utf8_lossy(&carry)));
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
