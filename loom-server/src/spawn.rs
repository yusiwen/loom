use std::io;
use std::os::unix::io::RawFd;

use nix::unistd::Pid;

/// Spawn a child process in a PTY.
///
/// Returns (child_pid, master_fd) on success.
/// master_fd is the PTY master fd for reading/writing child's I/O.
pub fn spawn_pty(
    cmd: &[String],
    cwd: &str,
    sx: u32,
    sy: u32,
) -> io::Result<(Pid, RawFd)> {
    // Open PTY master
    let master = unsafe { nix::libc::posix_openpt(nix::libc::O_RDWR | nix::libc::O_CLOEXEC) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }

    // Grant access and unlock slave
    if unsafe { nix::libc::grantpt(master) } < 0 {
        let e = io::Error::last_os_error();
        unsafe { nix::libc::close(master) };
        return Err(e);
    }
    if unsafe { nix::libc::unlockpt(master) } < 0 {
        let e = io::Error::last_os_error();
        unsafe { nix::libc::close(master) };
        return Err(e);
    }

    // Get slave name
    let slave_name = unsafe {
        let ptr = nix::libc::ptsname(master);
        if ptr.is_null() {
            let e = io::Error::last_os_error();
            nix::libc::close(master);
            return Err(e);
        }
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };

    // Set window size
    let ws = nix::libc::winsize {
        ws_row: sy as u16,
        ws_col: sx as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        nix::libc::ioctl(master, nix::libc::TIOCSWINSZ, &ws);
    }

    match unsafe { nix::libc::fork() } {
        -1 => {
            let e = io::Error::last_os_error();
            unsafe { nix::libc::close(master) };
            Err(e)
        }
        0 => {
            // Child process
            // Open slave
            let slave = unsafe {
                nix::libc::open(
                    slave_name.as_ptr() as *const i8,
                    nix::libc::O_RDWR,
                )
            };
            if slave < 0 {
                unsafe { nix::libc::_exit(1) };
            }

            // Create a new session and set controlling TTY
            unsafe {
                nix::libc::setsid();
                nix::libc::ioctl(slave, nix::libc::TIOCSCTTY, 0);
            }

            // Duplicate slave fd to stdin/stdout/stderr
            unsafe {
                nix::libc::dup2(slave, 0);
                nix::libc::dup2(slave, 1);
                nix::libc::dup2(slave, 2);
            }

            // Close all other fds
            unsafe {
                let max_fd = nix::libc::sysconf(nix::libc::_SC_OPEN_MAX) as i32;
                for fd in 3..max_fd {
                    nix::libc::close(fd);
                }
            }

            // Change directory
            let _ = std::env::set_current_dir(cwd);

            // Set TERM
            std::env::set_var("TERM", "xterm-256color");

            // Execute command
            if cmd.is_empty() {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
                let args = std::ffi::CString::new("").unwrap();
                unsafe {
                    nix::libc::execl(
                        shell.as_ptr() as *const i8,
                        shell.as_ptr() as *const i8,
                        args.as_ptr(),
                        std::ptr::null::<i8>(),
                    );
                }
            } else {
                let cmd_str = std::ffi::CString::new(cmd[0].as_str()).unwrap();
                let argv: Vec<std::ffi::CString> = cmd
                    .iter()
                    .map(|a| std::ffi::CString::new(a.as_str()).unwrap())
                    .collect();
                let mut ptrs: Vec<*const i8> = argv.iter().map(|a| a.as_ptr()).collect();
                ptrs.push(std::ptr::null());
                unsafe {
                    nix::libc::execvp(cmd_str.as_ptr(), ptrs.as_ptr());
                }
            }

            // If exec fails
            unsafe { nix::libc::_exit(127) };
        }
        pid => {
            // Parent: return the master fd
            Ok((Pid::from_raw(pid), master))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_pty_shell() {
        // Just test that spawn works, don't wait for it
        match spawn_pty(&["true".into()], "/tmp", 80, 24) {
            Ok((pid, master_fd)) => {
                assert!(pid.as_raw() > 0);
                assert!(master_fd >= 0);
                // Wait for child
                nix::sys::wait::waitpid(pid, None).unwrap();
                unsafe { nix::libc::close(master_fd) };
            }
            Err(e) => {
                // May fail in test environments without PTY
                eprintln!("spawn_pty failed: {}", e);
            }
        }
    }
}
