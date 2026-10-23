//! Child processes for verification: own process group, timeouts, and guaranteed cleanup.
//!
//! Every step and the app run in their own process group so that timeouts and cleanup
//! reach grandchildren (`go run`, `npm`). A `Proc` stops its group when dropped, so an
//! early return cannot leak a running app. Live groups are listed in a registry file; if
//! planvil itself is killed, the next verification finds the leftovers and refuses to run
//! until they are stopped (it never kills a recorded ID itself, since IDs can be reused).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);
const KILL_GRACE: Duration = Duration::from_secs(3);

/// File listing process groups started by verification: `pgid<TAB>command` per line.
#[derive(Debug, Clone)]
pub struct Registry {
    path: PathBuf,
}

impl Registry {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn entries(&self) -> Vec<(u32, String)> {
        fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (pid, cmd) = l.split_once('\t')?;
                Some((pid.parse().ok()?, cmd.to_string()))
            })
            .collect()
    }

    fn write(&self, entries: &[(u32, String)]) -> io::Result<()> {
        if entries.is_empty() {
            return match fs::remove_file(&self.path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let body: String = entries.iter().map(|(p, c)| format!("{p}\t{c}\n")).collect();
        fs::write(&self.path, body)
    }

    fn add(&self, pid: u32, cmd: &str) -> io::Result<()> {
        let mut entries = self.entries();
        entries.push((pid, cmd.replace(['\t', '\n'], " ")));
        self.write(&entries)
    }

    fn remove(&self, pid: u32) -> io::Result<()> {
        let entries: Vec<_> = self
            .entries()
            .into_iter()
            .filter(|(p, _)| *p != pid)
            .collect();
        self.write(&entries)
    }

    /// Groups from an interrupted verification that are still running. Dead entries are
    /// dropped from the file.
    pub fn leftovers(&self) -> io::Result<Vec<(u32, String)>> {
        let (alive, _dead): (Vec<_>, Vec<_>) = self
            .entries()
            .into_iter()
            .partition(|(p, _)| group_alive(*p));
        self.write(&alive)?;
        Ok(alive)
    }
}

/// A running child in its own process group. Dropping it stops the whole group.
pub struct Proc {
    child: Child,
    registry: Registry,
    done: bool,
}

impl Proc {
    pub fn spawn(cmd: &mut Command, label: &str, registry: &Registry) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        registry.add(child.id(), label)?;
        Ok(Self {
            child,
            registry: registry.clone(),
            done: false,
        })
    }

    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Wait up to `timeout`. Returns (exit code, timed out). The group is cleaned up either way.
    pub fn wait_timeout(&mut self, timeout: Duration) -> (Option<i32>, bool) {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    // Reap anything the step left running in its group.
                    self.finish(Signal::Kill);
                    return (status.code(), false);
                }
                Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
                Ok(None) | Err(_) => {
                    self.stop();
                    return (None, true);
                }
            }
        }
    }

    /// TERM the group, wait briefly, then KILL whatever remains.
    pub fn stop(&mut self) -> String {
        if self.done {
            return "process already stopped".into();
        }
        let note = match self.child.try_wait() {
            Ok(Some(status)) => format!("process had already exited ({status})"),
            _ => {
                signal_group(self.child.id(), Signal::Term);
                let deadline = Instant::now() + KILL_GRACE;
                let mut stopped = false;
                while Instant::now() < deadline {
                    if let Ok(Some(_)) = self.child.try_wait() {
                        stopped = true;
                        break;
                    }
                    std::thread::sleep(POLL);
                }
                if stopped {
                    "process stopped with TERM".into()
                } else {
                    "process killed after TERM grace period".into()
                }
            }
        };
        self.finish(Signal::Kill);
        note
    }

    fn finish(&mut self, signal: Signal) {
        signal_group(self.child.id(), signal);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.registry.remove(self.child.id());
        self.done = true;
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if !self.done {
            self.stop();
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
fn signal_group(pgid: u32, signal: Signal) {
    let sig = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    if let Ok(pgid) = libc::pid_t::try_from(pgid) {
        // SAFETY: killpg only sends a signal; an invalid or finished group yields ESRCH.
        unsafe {
            libc::killpg(pgid, sig);
        }
    }
}

#[cfg(not(unix))]
fn signal_group(pid: u32, _signal: Signal) {
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(unix)]
fn group_alive(pgid: u32) -> bool {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return false;
    };
    // SAFETY: signal 0 only checks existence; no signal is delivered.
    let rc = unsafe { libc::killpg(pgid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn group_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

/// Registry file for a feature's run directory.
pub fn registry_for(run_dir: &Path) -> Registry {
    Registry::new(run_dir.join("running.pids"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn dropping_a_proc_kills_its_group_and_clears_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry_for(dir.path());
        let pid = {
            let p = Proc::spawn(&mut sh("sleep 60 & sleep 60"), "app", &reg).unwrap();
            assert_eq!(reg.entries().len(), 1);
            p.child.id()
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(!group_alive(pid));
        assert!(reg.entries().is_empty());
    }

    #[test]
    fn background_leftovers_are_reaped_after_the_step_exits() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry_for(dir.path());
        let mut p = Proc::spawn(&mut sh("sleep 60 & exit 0"), "check", &reg).unwrap();
        let pid = p.child.id();
        assert_eq!(p.wait_timeout(Duration::from_secs(5)), (Some(0), false));
        std::thread::sleep(Duration::from_millis(200));
        assert!(!group_alive(pid));
    }

    #[test]
    fn leftovers_reports_only_live_groups() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry_for(dir.path());
        let mut live = sh("sleep 60");
        {
            use std::os::unix::process::CommandExt;
            live.process_group(0);
        }
        let mut child = live.spawn().unwrap();
        reg.add(child.id(), "sleep 60").unwrap();
        reg.add(999_999, "gone").unwrap();
        let found = reg.leftovers().unwrap();
        assert_eq!(found, vec![(child.id(), "sleep 60".to_string())]);
        let _ = child.kill();
        let _ = child.wait();
    }
}
