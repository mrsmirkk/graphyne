//! Child processes that must not outlive Graphyne (debug adapters, SSH tunnels).

/// Put `child` in a job that Windows closes when Graphyne exits, however it exits (also when
/// it is killed): the process ends with it. Elsewhere this does nothing.
#[cfg(windows)]
pub fn end_with_graphyne(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    static JOB: OnceLock<usize> = OnceLock::new();
    // The handle stays open for the life of Graphyne; closing it (at exit) ends the processes.
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void, size) == 0 {
            return 0;
        }
        job as usize
    });
    if job != 0 && unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) } == 0 {
        log::warn!("could not tie a child process to Graphyne's lifetime");
    }
}

#[cfg(not(windows))]
pub fn end_with_graphyne(_child: &std::process::Child) {}
