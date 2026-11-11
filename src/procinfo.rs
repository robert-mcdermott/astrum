//! A look at the processes in a terminal's foreground job: what is running
//! there and how much processor time it has used, so the agent can tell a
//! command that is working from one that is waiting.

/// The foreground job of a terminal at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The process group's leader, usually the command that was typed.
    pub pid: i32,
    pub name: String,
    /// Processes in the group, the leader included.
    pub processes: usize,
    /// Processor time the group's processes have used so far, in seconds.
    pub cpu_secs: f64,
}

/// What is known about process group `pgid`. `None` when the system gives
/// no way to ask, or the group has gone.
pub fn job(pgid: i32) -> Option<Job> {
    let (processes, cpu_secs) = group_usage(pgid)?;
    Some(Job { pid: pgid, name: name_of(pgid).unwrap_or_else(|| "a program".into()), processes, cpu_secs })
}

#[cfg(target_os = "macos")]
fn group_usage(pgid: i32) -> Option<(usize, f64)> {
    let mut pids = [0 as libc::pid_t; 1024];
    let listed = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), std::mem::size_of_val(&pids) as libc::c_int) };
    if listed <= 0 {
        return None;
    }
    // Task times are in the machine's own clock units; this says how many
    // nanoseconds one of them is.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    let mut timebase = Timebase { numer: 1, denom: 1 };
    unsafe { mach_timebase_info(&mut timebase) };
    let to_secs = |units: u64| units as f64 * timebase.numer as f64 / timebase.denom.max(1) as f64 / 1e9;
    let (mut count, mut cpu) = (0, 0.0);
    for &pid in pids.iter().take(listed as usize).filter(|pid| **pid > 0) {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&mut info as *mut libc::proc_taskinfo).cast(), size) } == size {
            count += 1;
            cpu += to_secs(info.pti_total_user + info.pti_total_system);
        }
    }
    (count > 0).then_some((count, cpu))
}

/// The directory a process is working in.
#[cfg(target_os = "macos")]
pub fn cwd_of(pid: i32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&mut info as *mut libc::proc_vnodepathinfo).cast(), size) } != size {
        return None;
    }
    // The path is a C string laid out as rows of an array.
    let bytes: Vec<u8> = info.pvi_cdir.vip_path.iter().flatten().map(|c| *c as u8).take_while(|b| *b != 0).collect();
    String::from_utf8(bytes).ok().filter(|p| !p.is_empty())
}

#[cfg(target_os = "linux")]
pub fn cwd_of(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn cwd_of(_pid: i32) -> Option<String> {
    None
}

/// Is this the name of a program that is a shell?
pub fn is_shell(name: &str) -> bool {
    let name = name.trim_start_matches('-');
    matches!(name, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "mksh" | "tcsh" | "csh" | "nu" | "xonsh" | "elvish" | "pwsh" | "ash" | "busybox")
}

#[cfg(target_os = "macos")]
pub fn name_of(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

#[cfg(target_os = "linux")]
fn group_usage(pgid: i32) -> Option<(usize, f64)> {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let (mut count, mut cpu) = (0, 0.0);
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        if !entry.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
        if let Some((group, used)) = parse_stat(&stat) {
            if group == pgid {
                count += 1;
                cpu += used as f64 / ticks;
            }
        }
    }
    (count > 0).then_some((count, cpu))
}

#[cfg(target_os = "linux")]
pub fn name_of(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn group_usage(_pgid: i32) -> Option<(usize, f64)> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn name_of(_pid: i32) -> Option<String> {
    None
}

/// From a Linux `/proc/<pid>/stat` line: the process group, and the clock
/// ticks of processor time used (user plus system). The program name, in
/// parentheses, may itself contain spaces and parentheses.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
fn parse_stat(stat: &str) -> Option<(i32, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name: state, ppid, pgrp, ... with utime and stime 11th and 12th.
    let group = fields.get(2)?.parse().ok()?;
    let used = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    Some((group, used))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_lines_are_read_past_awkward_names() {
        let line = "4242 (my (odd) prog) S 1 4200 4200 34816 4242 4194304 120 0 0 0 250 50 0 0 20 0 1 0 100 1000 10 18446744073709551615";
        assert_eq!(parse_stat(line), Some((4200, 300)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn this_process_can_be_seen() {
        // Our own process group has at least us in it, with some time used.
        let group = unsafe { libc::getpgrp() };
        let seen = job(group).expect("our own process group");
        assert!(seen.processes >= 1 && seen.cpu_secs >= 0.0 && !seen.name.is_empty(), "{seen:?}");
        assert_eq!(job(i32::MAX - 7), None);
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_process_says_where_it_is_working() {
        let here = std::env::current_dir().unwrap();
        let seen = cwd_of(std::process::id() as i32).expect("our own directory");
        assert_eq!(std::fs::canonicalize(seen).unwrap(), std::fs::canonicalize(here).unwrap());
        assert_eq!(cwd_of(i32::MAX - 7), None);
        assert!(is_shell("zsh") && is_shell("-bash") && !is_shell("vim"));
    }
}
