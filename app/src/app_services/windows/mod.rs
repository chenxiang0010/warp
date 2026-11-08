#[cfg(feature = "release_bundle")]
use std::path::PathBuf;

use registry::register_uri_handler;
#[cfg(feature = "release_bundle")]
use warp_errors::report_error;
use warpui::AppContext;
#[cfg(feature = "release_bundle")]
use {
    service_impl::forward_uri_to_sole_running_instance,
    single_instance_manager::SingleInstanceManager, thiserror::Error, url::Url,
    warp_core::channel::ChannelState,
};

mod registry;
#[cfg(feature = "release_bundle")]
mod service_impl;
#[cfg(feature = "release_bundle")]
mod single_instance_manager;

#[derive(Error, Debug)]
#[cfg(feature = "release_bundle")]
pub enum StartupArgsForwardingError {
    #[error("should not forward arguments after an auto-update")]
    IgnoredAfterAutoUpdate,
    /// The crash recovery watcher process, spawned by the main instance on
    /// every launch, must not forward a new-window URL back to its parent,
    /// which would open a duplicate window.
    #[error("should not forward arguments from the crash recovery process")]
    IgnoredForCrashRecoveryProcess,
    #[error("there is no other instance of Warp")]
    NoExistingInstance,
    #[error("failed to construct url")]
    CouldNotCreateUrl(#[from] url::ParseError),
    #[error("IPC Client failed to send message")]
    IpcError(#[from] ipc::ClientError),
    #[error("Win32 error")]
    WindowsError(#[from] windows::core::Error),
}

#[cfg(feature = "release_bundle")]
pub fn pass_startup_args_to_existing_instance(
    args: &warp_cli::AppArgs,
) -> Result<(), StartupArgsForwardingError> {
    if args.finish_update {
        return Err(StartupArgsForwardingError::IgnoredAfterAutoUpdate);
    }
    if crate::crash_recovery::is_crash_recovery_process(args) {
        return Err(StartupArgsForwardingError::IgnoredForCrashRecoveryProcess);
    }
    if SingleInstanceManager::is_sole_running_instance()? {
        return Err(StartupArgsForwardingError::NoExistingInstance);
    }

    warpui::r#async::block_on(async {
        if args.urls.is_empty() {
            // If there are no URLs on the command line, send one to open a new
            // window using the same current working directory as this process.
            let mut open_new_url = format!("{}://action/new_window", ChannelState::url_scheme());
            if let Some(current_dir) = startup_directory_for_forwarded_launch() {
                match current_dir.into_os_string().into_string() {
                    Ok(current_dir) => open_new_url.push_str(&format!("?path={}", current_dir)),
                    Err(os_string) => {
                        report_error!(
                            "Failed to convert OsString to String",
                            extra: { "os_string" => ?os_string }
                        );
                    }
                }
            }

            let url = Url::parse(&open_new_url)?;
            forward_uri_to_sole_running_instance(vec![url]).await?
        } else {
            forward_uri_to_sole_running_instance(args.urls.clone()).await?
        }

        Ok(())
    })
}

/// Working directory to request when forwarding a bare launch to an existing
/// instance.
///
/// Shortcuts created without a "Start in" directory launch the app with the
/// install directory as its working directory. Forwarding that directory would
/// open the new window there instead of in the user's home directory, so fall
/// back to home for that case. Launches from any other directory keep the
/// existing inherit-the-caller's-directory behavior.
#[cfg(feature = "release_bundle")]
fn startup_directory_for_forwarded_launch() -> Option<PathBuf> {
    let current_dir = std::env::current_dir().ok();
    let install_dir = crate::util::windows::install_dir().ok();
    match current_dir {
        Some(current_dir)
            if install_dir.is_some_and(|install_dir| {
                current_dir
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&install_dir.to_string_lossy())
            }) =>
        {
            dirs::home_dir()
        }
        other => other,
    }
}

pub(super) fn init(_ctx: &mut AppContext) {
    #[cfg(feature = "release_bundle")]
    _ctx.add_singleton_model(SingleInstanceManager::new);
    register_uri_handler();
}
