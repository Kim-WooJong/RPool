//! Windows: `rpool.exe` is a console program (it is also the CLI), so a
//! double-click from Explorer opens a console window for the GUI. When this
//! process is the only one on that console, the GUI lets it go and the
//! window closes; started from a terminal, the console stays for its logs.
//! `rpool-gui.exe` starts the GUI without any console window at all.

#[cfg(windows)]
pub(crate) fn release_own_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        fn FreeConsole() -> i32;
    }
    let mut ids = [0u32; 2];
    // SAFETY: the buffer outlives the call and its length is passed.
    let attached = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) };
    if attached == 1 {
        // SAFETY: no arguments; writes to the released console are ignored.
        unsafe {
            FreeConsole();
        }
    }
}

#[cfg(not(windows))]
pub(crate) fn release_own_console() {}
