//! File and folder pickers.
//!
//! On Linux the picker asks the desktop to show its own dialog over D-Bus, and that code only
//! works inside a Tokio runtime. Without one, opening a picker crashes the app, so pickers are
//! always opened through `run`.

/// Opens a picker with `show` and waits for the answer, with the background runtime it needs.
pub fn run<T>(show: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "linux")]
    {
        use std::sync::OnceLock;
        // One runtime for the whole session: the D-Bus connection is opened once and reused,
        // so the runtime it lives on has to stay alive too.
        static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
        let runtime = RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("dialogs")
                .enable_all()
                .build()
                .ok()
        });
        if let Some(runtime) = runtime {
            let _inside = runtime.enter();
            return show();
        }
    }
    show()
}
