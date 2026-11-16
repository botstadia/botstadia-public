//! Seat isolation for the session runner (TRUST.md#a-runtime-vulnerability-
//! that-could-let-a-bot-escape-its-seat), Linux only: an enclave image is
//! Linux. Each seat's process gets its own namespaces, a seccomp filter
//! allowing only what a Wasmtime seat needs, and its own cores (so the
//! host's view of CPU shows nothing per seat, and no seat can starve
//! another).
//!
//! Off Linux (the local runner on macOS) every call returns
//! [`Unsupported`]: the caller logs that the seat runs unisolated, which a
//! local run accepts and an image never sees.

/// Isolation isn't available on this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported;

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "seat isolation needs Linux")
    }
}

impl std::error::Error for Unsupported {}

/// The syscalls a seat's process may make, by name: memory, threads, time
/// and its pipes to the runner. No sockets, no files opened, no exec.
pub const SEAT_SYSCALLS: &[&str] = &[
    "read",
    "write",
    "close",
    "exit",
    "exit_group",
    "futex",
    "mmap",
    "munmap",
    "mprotect",
    "madvise",
    "brk",
    "rt_sigreturn",
    "rt_sigaction",
    "rt_sigprocmask",
    "sigaltstack",
    "clock_gettime",
    "clock_nanosleep",
    "sched_yield",
    "getrandom",
    "gettid",
    "membarrier",
    "mremap",
];

/// What the inference process may make besides: ONNX Runtime asks the
/// kernel about the CPU it's on.
pub const INFERENCE_EXTRA: &[&str] = &["sched_getaffinity", "getpid"];

/// What a worker that starts threads may make besides: a new thread (only
/// a thread: `clone` with `CLONE_THREAD`, checked by argument) and what a
/// thread's start and naming need.
pub const THREAD_EXTRA: &[&str] = &[
    "clone",
    "set_robust_list",
    "rseq",
    "prctl",
    "sched_getaffinity",
    "getpid",
];

/// Which list a worker's filter allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// A seat's Wasmtime process, and the arena's.
    Seat,
    /// A seat's ONNX Runtime process.
    Inference,
    /// Verification's worker, which runs a short timing session of its own
    /// on threads.
    Threaded,
}

impl Filter {
    pub fn syscalls(self) -> Vec<&'static str> {
        let extra: &[&str] = match self {
            Filter::Seat => &[],
            Filter::Inference => INFERENCE_EXTRA,
            Filter::Threaded => THREAD_EXTRA,
        };
        SEAT_SYSCALLS.iter().chain(extra).copied().collect()
    }
}

/// What a seat's process may never do, whatever the list says.
pub const NEVER: &[&str] = &[
    "socket", "connect", "bind", "execve", "open", "openat", "ptrace", "fork", "clone3",
];

/// Pin the calling thread or process to `cores`.
pub fn pin(cores: &[usize]) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mut set = nix::sched::CpuSet::new();
        for &c in cores {
            set.set(c)?;
        }
        nix::sched::sched_setaffinity(nix::unistd::Pid::from_raw(0), &set)?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cores;
        Err(Unsupported.into())
    }
}

/// The cores this process may run on: SMT is off on the enclave machine, so
/// each is a physical core.
pub fn cores() -> Vec<usize> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(set) = nix::sched::sched_getaffinity(nix::unistd::Pid::from_raw(0)) {
            let cores: Vec<usize> = (0..nix::sched::CpuSet::count())
                .filter(|&c| set.is_set(c).unwrap_or(false))
                .collect();
            if !cores.is_empty() {
                return cores;
            }
        }
    }
    (0..std::thread::available_parallelism().map_or(1, |n| n.get())).collect()
}

/// Whether the kernel reports simultaneous multithreading on: where it
/// can't say (no such file, not Linux), it's taken as off.
pub fn smt_active() -> bool {
    std::fs::read_to_string("/sys/devices/system/cpu/smt/active")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// A process's CPU time so far on macOS (the local runner), in
/// nanoseconds, summed over its threads: what `proc_pid_rusage` reports, in
/// Mach time units converted by the timebase. None for a process already
/// gone, or anywhere else (Linux reads `/proc` in the runner itself).
/// libc marks its Mach bindings deprecated in favour of another crate; for
/// one call, they stay.
#[cfg_attr(target_os = "macos", allow(deprecated))]
pub fn process_cpu_ns(pid: u32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: proc_pid_rusage writes one rusage_info_v2 for the flavour
        // we name into the buffer we pass, which is that struct's size.
        #[allow(unsafe_code)]
        let r = unsafe {
            libc::proc_pid_rusage(
                i32::try_from(pid).ok()?,
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast(),
            )
        };
        if r != 0 {
            return None;
        }
        // SAFETY: zeroed, then filled by the call that just succeeded.
        #[allow(unsafe_code)]
        let info = unsafe { info.assume_init() };
        let mut base = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: mach_timebase_info fills the struct we pass.
        #[allow(unsafe_code)]
        let r = unsafe { libc::mach_timebase_info(&mut base) };
        if r != 0 || base.denom == 0 {
            return None;
        }
        let ticks = u128::from(info.ri_user_time) + u128::from(info.ri_system_time);
        u64::try_from(ticks * u128::from(base.numer) / u128::from(base.denom)).ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// Run the calling thread only when nothing else wants its core
/// (`SCHED_IDLE`): the flat-CPU idle loop, which must never slow a seat.
pub fn idle_priority() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let param = libc::sched_param { sched_priority: 0 };
        // SAFETY: sched_setscheduler only reads the param we pass, and pid 0
        // is the calling thread. nix has no wrapper for it.
        #[allow(unsafe_code)]
        let r = unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) };
        anyhow::ensure!(
            r == 0,
            "sched_setscheduler: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    Err(Unsupported.into())
}

/// Give the calling process its own network and mount namespaces (and no
/// network), before any bot code runs.
pub fn isolate() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use nix::sched::{CloneFlags, unshare};
        let rest = CloneFlags::CLONE_NEWNET
            | CloneFlags::CLONE_NEWNS
            | CloneFlags::CLONE_NEWIPC
            | CloneFlags::CLONE_NEWUTS;
        // With the privilege to, directly; without it, inside a user
        // namespace of its own, where it has it.
        if unshare(rest).is_ok() {
            return Ok(());
        }
        unshare(CloneFlags::CLONE_NEWUSER | rest)?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    Err(Unsupported.into())
}

/// Cap the calling process's writable private memory (`RLIMIT_DATA`), so a
/// seat that exhausts it fails alone. Address space merely reserved (as
/// Wasmtime reserves its linear memories, inaccessible) doesn't count.
pub fn cap_memory(bytes: u64) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        nix::sys::resource::setrlimit(nix::sys::resource::Resource::RLIMIT_DATA, bytes, bytes)?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = bytes;
        Err(Unsupported.into())
    }
}

#[cfg(target_os = "linux")]
fn number(name: &str) -> Option<i64> {
    Some(match name {
        "read" => libc::SYS_read,
        "write" => libc::SYS_write,
        "close" => libc::SYS_close,
        "exit" => libc::SYS_exit,
        "exit_group" => libc::SYS_exit_group,
        "futex" => libc::SYS_futex,
        "mmap" => libc::SYS_mmap,
        "munmap" => libc::SYS_munmap,
        "mprotect" => libc::SYS_mprotect,
        "madvise" => libc::SYS_madvise,
        "brk" => libc::SYS_brk,
        "rt_sigreturn" => libc::SYS_rt_sigreturn,
        "rt_sigaction" => libc::SYS_rt_sigaction,
        "rt_sigprocmask" => libc::SYS_rt_sigprocmask,
        "sigaltstack" => libc::SYS_sigaltstack,
        "clock_gettime" => libc::SYS_clock_gettime,
        "clock_nanosleep" => libc::SYS_clock_nanosleep,
        "sched_yield" => libc::SYS_sched_yield,
        "getrandom" => libc::SYS_getrandom,
        "gettid" => libc::SYS_gettid,
        "membarrier" => libc::SYS_membarrier,
        "sched_getaffinity" => libc::SYS_sched_getaffinity,
        "getpid" => libc::SYS_getpid,
        "mremap" => libc::SYS_mremap,
        "clone" => libc::SYS_clone,
        "set_robust_list" => libc::SYS_set_robust_list,
        "rseq" => libc::SYS_rseq,
        "prctl" => libc::SYS_prctl,
        _ => return None,
    })
}

/// Install the seat's seccomp filter: anything off [`SEAT_SYSCALLS`] kills
/// the process (a refused syscall, reported under the deferred report rule).
pub fn seccomp() -> anyhow::Result<()> {
    seccomp_for(Filter::Seat)
}

/// Install `filter`'s seccomp filter: anything off its list kills the
/// process.
pub fn seccomp_for(filter: Filter) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use seccompiler::{
            SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
            SeccompRule, TargetArch,
        };
        let arch =
            TargetArch::try_from(std::env::consts::ARCH).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        // glibc's malloc counts the CPUs, reading
        // /sys/devices/system/cpu/online, the first time a process wants a
        // ninth arena, which a filtered worker's threads can reach (found on
        // TDX: the policy bot's timing run). With the limit fixed first it
        // never asks.
        #[cfg(target_env = "gnu")]
        {
            #[allow(unsafe_code)]
            let set = unsafe { libc::mallopt(libc::M_ARENA_MAX, 8) };
            anyhow::ensure!(set == 1, "mallopt(M_ARENA_MAX) refused");
        }
        // `clone3` can't be checked by argument (its flags are behind a
        // pointer), so it answers "not implemented", and glibc falls back to
        // `clone`, which can. A first filter does only that.
        let clone3 = SeccompFilter::new(
            [(libc::SYS_clone3, vec![])].into_iter().collect(),
            SeccompAction::Allow,
            SeccompAction::Errno(libc::ENOSYS as u32),
            arch,
        )?;
        let program: seccompiler::BpfProgram = clone3.try_into()?;
        seccompiler::apply_filter(&program)?;
        let thread_only = SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Qword,
            SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64),
            libc::CLONE_THREAD as u64,
        )?])?;
        let mut rules = filter
            .syscalls()
            .iter()
            .map(|n| {
                let rules = if *n == "clone" {
                    vec![thread_only.clone()]
                } else {
                    vec![]
                };
                number(n)
                    .map(|s| (s, rules))
                    .ok_or_else(|| anyhow::anyhow!("no syscall number for {n}"))
            })
            .collect::<anyhow::Result<std::collections::BTreeMap<_, _>>>()?;
        // Stacked filters answer with the most restrictive of their
        // actions, so this one must pass `clone3` for the first one's
        // "not implemented" to stand: otherwise its kill wins, and every
        // thread start on x86_64 glibc ends the worker (found on TDX). It
        // never runs: the first filter answers it.
        rules.insert(libc::SYS_clone3, vec![]);
        // BOTSTADIA_SECCOMP_TRAP=1 (debugging only) refuses with SIGSYS
        // rather than killing outright, which still ends the process but
        // lets a tracer name the syscall.
        let refuse = if std::env::var("BOTSTADIA_SECCOMP_TRAP").is_ok_and(|v| v == "1") {
            SeccompAction::Trap
        } else {
            SeccompAction::KillProcess
        };
        let filter = SeccompFilter::new(rules, refuse, SeccompAction::Allow, arch)?;
        let program: seccompiler::BpfProgram = filter.try_into()?;
        seccompiler::apply_filter(&program)?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = filter;
        Err(Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_list_allows_anything_that_reaches_out() {
        for filter in [Filter::Seat, Filter::Inference, Filter::Threaded] {
            for n in NEVER {
                assert!(!filter.syscalls().contains(n), "{n} must never be allowed");
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn off_linux_every_call_says_it_is_unsupported() {
        for r in [pin(&[0]), isolate(), seccomp(), cap_memory(1 << 30)] {
            assert!(r.unwrap_err().downcast_ref::<Unsupported>().is_some());
        }
    }

    /// Each filter applied in a child, which then does what its worker
    /// does: a Threaded one starts a thread (glibc tries `clone3` first).
    /// Found on TDX: every verification worker ended on a refused syscall.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_threaded_worker_starts_a_thread_under_its_filter() {
        #[allow(unsafe_code)]
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // Many threads allocating at once, as a timing run's do: past
            // eight malloc arenas glibc would count the CPUs from a file.
            let ok = seccomp_for(Filter::Threaded).is_ok() && {
                let start = std::sync::Arc::new(std::sync::Barrier::new(16));
                let threads: Vec<_> = (0..16)
                    .map(|i| {
                        let start = start.clone();
                        std::thread::spawn(move || {
                            start.wait();
                            (0..1000).map(|n| vec![i; n % 64 + 1].len()).sum::<usize>()
                        })
                    })
                    .collect();
                threads.into_iter().all(|t| t.join().is_ok())
            };
            #[allow(unsafe_code)]
            unsafe {
                libc::_exit(if ok { 0 } else { 1 })
            }
        }
        let mut status = 0;
        #[allow(unsafe_code)]
        unsafe {
            libc::waitpid(pid, &mut status, 0)
        };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "the child ended with status {status:#x} (signal {})",
            libc::WTERMSIG(status)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn every_allowed_syscall_has_a_number_here() {
        for filter in [Filter::Seat, Filter::Inference, Filter::Threaded] {
            for n in filter.syscalls() {
                assert!(number(n).is_some(), "{n}");
            }
        }
    }
}
