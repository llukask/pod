//! The tokio runtime that sync, downloads and HTTP run on.
//!
//! GPUI drives the UI thread with its own executors, while `reqwest` and the
//! download code need tokio. The app starts one multi-thread runtime whose
//! workers are background threads; the UI thread only ever hands work to it
//! through [`Background`] and awaits the result on GPUI's foreground
//! executor:
//!
//! ```ignore
//! let task = cx.global::<Background>().spawn(async move { fetch().await });
//! cx.spawn(async move |this, cx| {
//!     let result = task.await;
//!     this.update(cx, |view, cx| view.show(result, cx))
//! })
//! .detach();
//! ```
//!
//! Awaiting a tokio `JoinHandle` needs no tokio context, so it works from
//! GPUI's executor.

use std::future::Future;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr};
use gpui_kit::Global;
use tokio::runtime::{Handle, Runtime};
use tokio::task::JoinHandle;

/// Two workers are plenty for IO-bound sync and downloads, and keep the idle
/// footprint small. A multi-thread runtime rather than a current-thread one
/// means a task that blocks for a moment (a large SQLite transaction, say)
/// doesn't stall every transfer.
const WORKER_THREADS: usize = 2;

/// How long quitting waits for blocking tasks before abandoning them.
// TODO: once the sync engine and downloads exist, cancel them and let them
// commit their current transaction before the runtime goes away.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Spawns futures onto the app's tokio runtime. Registered as a GPUI global,
/// so any entity can reach it with `cx.global::<Background>()`.
#[derive(Clone, Debug)]
pub struct Background {
    handle: Handle,
}

impl Global for Background {}

impl Background {
    /// Runs `future` on a runtime worker. The returned handle resolves on any
    /// executor, including GPUI's.
    #[cfg_attr(
        not(any(test, target_os = "linux")),
        expect(
            dead_code,
            reason = "the network monitor spawns only on Linux, the sync engine is next"
        )
    )]
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }
}

/// Starts the runtime. The caller owns the [`Runtime`] and passes it to
/// [`shut_down`] on quit; the [`Background`] goes to GPUI.
pub fn start() -> Result<(Runtime, Background)> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("pod-tokio")
        .enable_all()
        .build()
        .context("build the tokio runtime")?;
    let background = Background {
        handle: runtime.handle().clone(),
    };
    Ok((runtime, background))
}

/// Stops the runtime, dropping pending tasks and waiting up to
/// [`SHUTDOWN_GRACE`] for blocking ones.
pub fn shut_down(runtime: Runtime) {
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    tracing::info!("background runtime stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawned_work_runs_off_the_calling_thread() {
        let (runtime, background) = start().expect("a runtime builds in tests");
        let (sender, receiver) = std::sync::mpsc::channel();
        background.spawn(async move {
            let thread = std::thread::current();
            sender
                .send((thread.id(), thread.name().map(str::to_owned)))
                .expect("the test holds the receiver");
        });
        let (worker, name) = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the task runs within five seconds");
        assert_ne!(worker, std::thread::current().id());
        assert_eq!(name.as_deref(), Some("pod-tokio"));
        shut_down(runtime);
    }
}
