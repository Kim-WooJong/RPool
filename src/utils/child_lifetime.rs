//! Long-lived rclone children (the mount, the rc daemons) end with RPool.
//!
//! On Windows each such child is put into one job object of this process
//! with "kill on job close": when RPool exits in any way, also when it is
//! killed from Task Manager or by the GUI's forced stop, Windows closes the
//! job and ends the children. A leftover rclone otherwise kept its mount and
//! files in the workspace open (os error 32 on the next workspace switch).
//! Elsewhere this is a no-op: graceful shutdown signals are handled
//! (`mount::shutdown_signal`) and a mount orphaned by a crash is stopped by
//! the next mount (`MountLease::prepare`).

/// Ties `child` to this process's lifetime (Windows); best effort.
#[cfg(windows)]
pub(crate) fn tie_to_this_process(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    /// The job handle as an integer (handles are process-wide and never closed).
    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    let job = JOB.get_or_init(|| {
        // SAFETY: plain Win32 calls with valid arguments; the handle is kept
        // for the whole process so the job closes exactly when it exits.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            (ok != 0).then_some(job as usize)
        }
    });
    let Some(job) = *job else {
        eprintln!("[warning] rclone child not tied to RPool's lifetime (job object unavailable)");
        return;
    };
    // SAFETY: both handles are valid for the duration of the call.
    if unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) } == 0 {
        eprintln!("[warning] rclone child not tied to RPool's lifetime (job assignment failed)");
    }
}

/// No-op outside Windows (see the module docs).
#[cfg(not(windows))]
pub(crate) fn tie_to_this_process(_child: &std::process::Child) {}
