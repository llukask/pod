//! Pod desktop client: a GPUI window over a local store, with sync,
//! downloads and playback on a tokio runtime beside it (design.md, "Desktop
//! client").

mod account;
mod app;
mod download;
mod inbox;
mod logging;
mod net;
mod reconcile;
mod runtime;
mod shell;
mod show_notes;
mod store;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the UI is the first caller of local writes")
)]
mod sync;

use std::cell::RefCell;
use std::rc::Rc;

use color_eyre::eyre::{OptionExt as _, Result, WrapErr, bail, eyre};
use gpui_kit::{App, QuitMode};

fn main() -> Result<()> {
    color_eyre::install().context("install the color-eyre report handler")?;

    // Everything that touches the disk before the first frame happens here,
    // on the main thread but before the UI exists. After this, log writes go
    // through the appender's own thread.
    let data_dir = dirs::data_dir()
        .ok_or_eyre("the platform has no data directory")?
        .join("pod");
    let _log_guard = logging::init(&data_dir.join("logs")).context("set up logging")?;
    tracing::info!(data_dir = %data_dir.display(), "starting pod-desktop");
    let store = store::Store::open(&data_dir.join("pod.db")).context("open the local store")?;

    let (runtime, background) = runtime::start().context("start the background runtime")?;
    let network = net::NetworkMonitor::start(&background);
    let sync_handle = sync::SyncHandle::default();
    let sync_engine =
        sync::engine::SyncEngine::new(store.clone(), sync_handle.clone(), network.subscribe())
            .context("set up the sync engine")?;
    background.spawn(sync_engine.clone().run());
    let downloader = download::Downloader::new(store.clone(), network.subscribe(), &data_dir)
        .context("set up downloads")?;
    let reconciler = reconcile::Reconciler::new(downloader, reconcile::free_space);
    // Startup repair first, then a pass on every trigger of spec 7.4.
    background.spawn(reconciler.clone().run(sync_engine.clone()));
    let session = sync::session::Session::new(
        store.clone(),
        sync_handle.clone(),
        sync_engine.clone(),
        reconciler.clone(),
    );

    // `run` takes a closure that can't return an error, so a failed launch is
    // passed out through this slot and reported once the event loop stops.
    let startup_error: Rc<RefCell<Option<color_eyre::Report>>> = Rc::default();
    let startup_error_slot = startup_error.clone();
    let runtime = RefCell::new(Some(runtime));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        // Pod has one window and no tray icon yet, so closing it quits on
        // every platform, macOS included.
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx| {
            gpui_kit::init(cx);
            app::init(cx);
            cx.set_global(background);
            cx.set_global(store);
            cx.set_global(network);
            cx.set_global(sync_handle);
            cx.set_global(sync_engine);
            cx.set_global(reconciler);
            cx.set_global(session);

            // Shutting down here, not after `run` returns, covers macOS,
            // where `run` never returns.
            cx.on_app_quit(move |_| {
                if let Some(runtime) = runtime.take() {
                    runtime::shut_down(runtime);
                }
                async {}
            })
            .detach();

            if let Err(error) = launch(cx) {
                *startup_error_slot.borrow_mut() = Some(error);
                // The Linux event loop discards a stop requested before it
                // starts, so the quit goes through the loop as a task.
                cx.spawn(async |cx| cx.update(|cx| cx.quit())).detach();
            }
        });

    match startup_error.take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn launch(cx: &mut App) -> Result<()> {
    // Without WAYLAND_DISPLAY or DISPLAY, GPUI on Linux starts headless: the
    // window would open, draw nothing and keep the process running with
    // nothing on screen. (A display variable naming an unreachable server
    // makes GPUI panic before this runs.)
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if cx.graphical_environment().is_none() {
        bail!("no display server: neither WAYLAND_DISPLAY nor DISPLAY is set");
    }
    app::open_main_window(cx)
        .map_err(|error| eyre!("{error:#}"))
        .context("open the main window")?;
    Ok(())
}
