//! Child-process helpers. Everything called from `pre_exec` is
//! async-signal-safe (plain syscalls, no allocation).
//!
//! | | Linux | macOS |
//! |---|---|---|
//! | [`detach_session`] | `setsid()` in the child | same |
//! | [`set_parent_death_signal`] | `prctl(PR_SET_PDEATHSIG, SIGTERM)` | unsupported: `Ok(false)`; children are killed by their owner's timeout/`Drop` instead |
//! | [`set_limit`] | `setrlimit` | `setrlimit`; `AddressSpace` is best effort (XNU does not enforce `RLIMIT_AS`) |
//! | [`pin_lowest_cpu`] | `sched_setaffinity` to the lowest allowed CPU | no-op (no affinity API) |
//! | [`capture`] | bounded `Command::output` with kill on timeout | same |
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `subprocess.Popen(..., start_new_session=True)`: `setsid()` in the
/// forked child; its result is ignored like before.
pub fn detach_session(command: &mut Command) {
    // SAFETY: setsid is async-signal-safe and runs in the forked child only.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Ask the kernel to send SIGTERM to the calling (child) process when its
/// parent exits. `Ok(true)`: armed. `Ok(false)`: the platform has no such
/// facility (macOS). `Err`: the call failed.
#[cfg(target_os = "linux")]
pub fn set_parent_death_signal() -> io::Result<bool> {
    // SAFETY: prctl with integer arguments only.
    let rc = unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong)
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

/// Ask the kernel to send SIGTERM to the calling (child) process when its
/// parent exits. `Ok(false)`: not available on this platform.
#[cfg(not(target_os = "linux"))]
pub fn set_parent_death_signal() -> io::Result<bool> {
    Ok(false)
}

/// Resource limits the preview/ink workers apply to themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    AddressSpace,
    Cpu,
    Core,
    FileSize,
}

fn setrlimit_raw(limit: Limit, value: u64) -> io::Result<()> {
    let resource = match limit {
        Limit::AddressSpace => libc::RLIMIT_AS,
        Limit::Cpu => libc::RLIMIT_CPU,
        Limit::Core => libc::RLIMIT_CORE,
        Limit::FileSize => libc::RLIMIT_FSIZE,
    };
    let rlimit = libc::rlimit { rlim_cur: value as libc::rlim_t, rlim_max: value as libc::rlim_t };
    // SAFETY: plain syscall with a valid pointer.
    if unsafe { libc::setrlimit(resource, &rlimit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `resource.setrlimit(resource, (value, value))`.
#[cfg(not(target_os = "macos"))]
pub fn set_limit(limit: Limit, value: u64) -> io::Result<()> {
    setrlimit_raw(limit, value)
}

/// `resource.setrlimit(resource, (value, value))`; on macOS a refused
/// `AddressSpace` limit is ignored (it is not enforced by XNU anyway).
#[cfg(target_os = "macos")]
pub fn set_limit(limit: Limit, value: u64) -> io::Result<()> {
    match setrlimit_raw(limit, value) {
        Err(_) if limit == Limit::AddressSpace => Ok(()),
        other => other,
    }
}

/// Whether [`set_limit`]`(AddressSpace, ..)` is actually enforced.
pub const ADDRESS_SPACE_LIMIT_ENFORCED: bool = !cfg!(target_os = "macos");

/// `os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})`.
#[cfg(target_os = "linux")]
pub fn pin_lowest_cpu() -> io::Result<()> {
    // SAFETY: cpu_set_t is plain data; the syscalls get valid pointers.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Err(io::Error::last_os_error());
        }
        let lowest = (0..libc::CPU_SETSIZE as usize).find(|&cpu| libc::CPU_ISSET(cpu, &set));
        if let Some(cpu) = lowest {
            let mut single: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(cpu, &mut single);
            if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &single) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// CPU pinning is not available on this platform: no-op.
#[cfg(not(target_os = "linux"))]
pub fn pin_lowest_cpu() -> io::Result<()> {
    Ok(())
}

/// Run `argv` with stdin closed, capturing stdout (stderr discarded into a
/// pipe), killing it after `timeout`. `None` when it could not start, timed
/// out, or could not be waited for. Same contract as
/// `rs_engine::engine_core::run_capture`.
pub fn capture(argv: &[&str], timeout: Duration) -> Option<(i32, Vec<u8>)> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    Some((out.status.code().unwrap_or(-1), out.stdout))
}
