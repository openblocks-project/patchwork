//! P6 Stage 2.8: capture and decode subprocesses end with Patchwork.
//!
//! Camera and video/audio decoding run ffmpeg as a child process. A normal
//! shutdown stops it (the decoder's `Drop` kills it). If Patchwork crashes or
//! is ended forcibly no `Drop` runs, and on Windows a child process outlives
//! its parent, so ffmpeg could keep the camera open.
//!
//! On Windows each such child is therefore assigned to one Job Object owned
//! by this process, created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The
//! only handle to the job is this process's; when the process ends for any
//! reason Windows closes it and ends every process in the job. Only children
//! Patchwork started itself are ever assigned: no other process (another
//! ffmpeg included) is touched. Elsewhere this does nothing.

use std::process::Child;

/// Tie `child` to this process's lifetime (Windows; elsewhere a no-op).
/// Failure is not fatal: the child still stops on normal shutdown.
pub fn adopt(child: &Child) {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        if let Err(e) = windows::process_job().and_then(|job| job.assign(child.as_raw_handle())) {
            crate::system_log::warn(format!(
                "A subprocess is not tied to Patchwork's lifetime ({e}); it still stops when \
                 Patchwork closes normally."
            ));
        }
    }
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    type Handle = *mut c_void;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimitInformation {
        basic: BasicLimitInformation,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: i32,
            information: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    /// A Job Object that ends its processes when its last handle closes.
    pub(crate) struct KillOnCloseJob(Handle);

    // The handle is only passed to thread-safe kernel calls.
    unsafe impl Send for KillOnCloseJob {}
    unsafe impl Sync for KillOnCloseJob {}

    impl KillOnCloseJob {
        pub(crate) fn new() -> Result<Self, String> {
            // SAFETY: no security attributes, unnamed (so no other process
            // can open it by name).
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(format!("CreateJobObject failed ({})", unsafe {
                    GetLastError()
                }));
            }
            let job = KillOnCloseJob(handle);
            let mut info = ExtendedLimitInformation::default();
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: a valid job handle and a correctly sized structure.
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    (&info as *const ExtendedLimitInformation).cast(),
                    std::mem::size_of::<ExtendedLimitInformation>() as u32,
                )
            };
            if ok == 0 {
                return Err(format!("SetInformationJobObject failed ({})", unsafe {
                    GetLastError()
                }));
            }
            Ok(job)
        }

        /// Add a process (a handle of a child this process started).
        pub(crate) fn assign(&self, process: *mut c_void) -> Result<(), String> {
            // SAFETY: both handles are valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
                return Err(format!("AssignProcessToJobObject failed ({})", unsafe {
                    GetLastError()
                }));
            }
            Ok(())
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // SAFETY: our own handle, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// This process's job: created once and never closed before the
    /// process ends (statics are not dropped), which is what ends its
    /// children if the process is terminated.
    pub(crate) fn process_job() -> Result<&'static KillOnCloseJob, String> {
        static JOB: OnceLock<Result<KillOnCloseJob, String>> = OnceLock::new();
        JOB.get_or_init(KillOnCloseJob::new)
            .as_ref()
            .map_err(Clone::clone)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::windows::KillOnCloseJob;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// A long-running process that does nothing (no window, no output).
    fn idle_child() -> std::process::Child {
        Command::new("ping")
            .args(["-n", "120", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("ping")
    }

    fn exits_within(child: &mut std::process::Child, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Closing the job ends exactly the processes assigned to it.
    #[test]
    fn closing_the_job_ends_its_children_and_nothing_else() {
        let job = KillOnCloseJob::new().unwrap();
        let mut adopted = idle_child();
        let mut unrelated = idle_child();
        job.assign(adopted.as_raw_handle()).unwrap();
        assert!(!exits_within(&mut adopted, Duration::from_millis(300)));
        drop(job);
        assert!(
            exits_within(&mut adopted, Duration::from_secs(5)),
            "an adopted child ends with the job"
        );
        assert!(
            !exits_within(&mut unrelated, Duration::from_millis(500)),
            "a process not in the job is never touched"
        );
        unrelated.kill().unwrap();
        let _ = unrelated.wait();
    }

    /// End to end: a process that adopted a child is terminated forcibly
    /// (as a crash or Task Manager would); the child ends with it.
    #[test]
    fn a_forcibly_ended_parent_takes_its_adopted_child_with_it() {
        if let Ok(pid_file) = std::env::var("PATCHWORK_CHILD_JOB_HELPER") {
            // Helper mode (run by the test below): adopt a child, report its
            // id, then wait to be killed.
            let child = idle_child();
            super::adopt(&child);
            std::fs::write(&pid_file, child.id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(120));
            return;
        }
        let dir = std::env::temp_dir().join(format!("pw_child_job_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("child.pid");
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "video_io::child_job::tests::a_forcibly_ended_parent_takes_its_adopted_child_with_it",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("PATCHWORK_CHILD_JOB_HELPER", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    break pid;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "helper did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let alive = |pid: u32| {
            let out = Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        };
        assert!(alive(pid), "the adopted child runs");
        // TerminateProcess: no destructors, no cleanup in the helper.
        helper.kill().unwrap();
        let _ = helper.wait();
        let start = Instant::now();
        while alive(pid) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let survived = alive(pid);
        if survived {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .output();
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!survived, "the child outlived its forcibly ended parent");
    }
}
