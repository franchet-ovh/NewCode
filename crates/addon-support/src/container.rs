//! Linux supervisor for an addon carried over Docker's stdio attachment.
//! Protocol stdout passes through unchanged. No Docker client is needed inside.

/// Handle the private supervisor/stop entry points before starting addon workers.
pub fn entry() -> Option<Result<i32, String>> {
    let mut args = std::env::args_os().skip(1);
    let operation = args.next()?;
    if operation != "--container-supervisor" && operation != "--container-stop" {
        return None;
    }
    let directory = match args.next() {
        Some(path) => std::path::PathBuf::from(path),
        None => return Some(Err("Missing container launch directory".into())),
    };
    #[cfg(target_os = "linux")]
    return Some(if operation == "--container-stop" {
        linux::stop(&directory)
    } else {
        linux::supervise(&directory)
    });
    #[cfg(not(target_os = "linux"))]
    Some(Err(format!(
        "Container execution requires Linux: {}",
        directory.display()
    )))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        fs, io,
        os::unix::{fs::PermissionsExt, process::CommandExt},
        path::Path,
        process::{Command, Stdio},
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };
    static STOP: AtomicBool = AtomicBool::new(false);
    static EOF: AtomicBool = AtomicBool::new(false);
    extern "C" fn signal(_: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }

    fn identity(pid: i32) -> Option<(i32, String)> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        Some((fields.get(1)?.parse().ok()?, fields.get(19)?.to_string()))
    }

    pub(super) fn stop(directory: &Path) -> Result<i32, String> {
        let text = match fs::read_to_string(directory.join("supervisor")) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.to_string()),
        };
        let (pid, start) = text.split_once(' ').ok_or("Invalid supervisor identity")?;
        let pid: i32 = pid.parse().map_err(|_| "Invalid supervisor PID")?;
        if pid > 1 && identity(pid).is_some_and(|(_, actual)| actual == start) {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        } else {
            // An old launch record must never signal a reused PID.
            let _ = fs::remove_file(directory.join("supervisor"));
        }
        Ok(0)
    }

    // The supervisor is a subreaper, so detached backend descendants remain owned.
    fn descendants(parent: i32) -> Vec<i32> {
        let entries: Vec<_> = fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let pid = entry.ok()?.file_name().to_str()?.parse::<i32>().ok()?;
                let (ppid, _) = identity(pid)?;
                Some((pid, ppid))
            })
            .collect();
        let mut owned = vec![parent];
        let mut index = 0;
        while index < owned.len() {
            for &(pid, ppid) in &entries {
                if ppid == owned[index] && !owned.contains(&pid) {
                    owned.push(pid);
                }
            }
            index += 1;
        }
        owned.remove(0);
        owned
    }

    pub(super) fn supervise(directory: &Path) -> Result<i32, String> {
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let pid = std::process::id() as i32;
        let (_, start) = identity(pid).ok_or("Cannot identify container supervisor")?;
        fs::write(directory.join("supervisor"), format!("{pid} {start}"))
            .map_err(|e| e.to_string())?;
        unsafe {
            if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
                return Err("Cannot supervise container addon processes".into());
            }
            libc::signal(libc::SIGTERM, signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, signal as *const () as libc::sighandler_t);
        }
        let mut child = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
            .arg("--container-child")
            .env("FOCUS_ADDON_IN_CONTAINER", "1")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut input = child.stdin.take().ok_or("Addon input unavailable")?;
        std::thread::spawn(move || {
            let _ = io::copy(&mut io::stdin().lock(), &mut input);
            drop(input);
            EOF.store(true, Ordering::Relaxed);
        });
        let mut stopping = None;
        let result = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status.code().unwrap_or(1);
            }
            if STOP.load(Ordering::Relaxed) || EOF.load(Ordering::Relaxed) {
                let since = stopping.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(2) {
                    break 1;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Includes OpenCode's separate server group and double-forked tool children.
        for pid in descendants(pid).into_iter().rev() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            let mut status = 0;
            let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if reaped < 0 {
                break;
            }
            if reaped == 0 {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = fs::remove_file(directory.join("supervisor"));
        Ok(result)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn identity_uses_process_start_time_and_descendants_are_scoped() {
            let pid = std::process::id() as i32;
            assert!(identity(pid).is_some());
            assert!(identity(-1).is_none());
            assert!(!descendants(pid).contains(&pid));
        }
    }
}
