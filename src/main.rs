#![deny(clippy::all)]
#![warn(clippy::pedantic)]

use anyhow::Context;
use clap::Parser;
use futures::StreamExt;
use nameof::{name_of, name_of_type};
use nix::{
    fcntl::{self, OFlag},
    libc,
    sys::stat::Mode,
    unistd::{self, ForkResult},
};
use procfs::{Current, Meminfo};
use std::{
    cell::Cell,
    collections::HashMap,
    env, process,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::runtime::Runtime;
use tokio::{
    runtime, select,
    signal::unix::{self, SignalKind},
    time,
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use zbus::{proxy, zvariant::Value};

#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait DbusProxy {
    #[zbus(name = "CloseNotification")]
    async fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(name = "NotificationClosed", signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::fdo::Result<()>;

    /// Notify (String app_name, UInt32 replaces_id, String app_icon, String summary, String body, Array of [String] actions, Dict of {String, Variant} hints, Int32 timeout) ↦ (UInt32 arg_0)
    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "Notify")]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: &HashMap<&str, &Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

/// Displays a notification when free system memory falls below a threshold
#[derive(Parser)]
#[command(author, version, about)]
struct MemalertArgs {
    /// Run as a daemon
    #[arg(long = "daemon", short = 'd')]
    daemon: bool,

    /// A notification will be displayed when the percentage of system memory that is free falls below this threshold. If not specified, a default value is used. Must be greater than or equal to 1, and less than or equal to 99.
    #[arg(long = "memory-free-percent", short = 'm')]
    memory_free_percent: Option<u8>,
}

const MAXIMUM: u64 = 1_u64 << f64::MANTISSA_DIGITS;
const DURATION: Duration = Duration::from_secs(1_u64);
const SLICE: [&str; 0_usize] = [];
/// Display notification when the proportion of system memory that is free is smaller than this value
const DEFAULT_THRESHOLD: f64 = 0.33_f64;
// "Critical"
const VALUE: Value = Value::U8(2_u8);

fn main() -> Result<(), i32> {
    // TODO
    env::set_var("RUST_BACKTRACE", "1");
    // TODO
    env::set_var("RUST_LOG", "debug");

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().pretty())
        .init();

    let result = start();

    if let Err(er) = result {
        tracing::error!(
            backtrace = %er.backtrace(),
            error = %er,
        );

        return Err(1_i32);
    }

    Ok(())
}

// TODO
// Fix logging
#[allow(clippy::too_many_lines)]
fn start() -> anyhow::Result<()> {
    let MemalertArgs {
        daemon,
        memory_free_percent,
    } = MemalertArgs::parse();

    // https://0xjet.github.io/3OHA/2022/04/11/post.html
    // https://unix.stackexchange.com/questions/56495/whats-the-difference-between-running-a-program-as-a-daemon-and-forking-it-into/56497#56497
    // https://stackoverflow.com/questions/5384168/how-to-make-a-daemon-process#comment64993302_5384168
    if daemon {
        tracing::info!("Attempting to run as a daemon");

        let fork_result = unsafe { unistd::fork() }?;

        if let ForkResult::Parent { .. } = fork_result {
            process::exit(libc::EXIT_SUCCESS);
        }

        unistd::setsid()?;

        unistd::chdir("/")?;

        // TODO
        // Close all file descriptors, not just these?
        {
            unistd::close(libc::STDIN_FILENO)?;

            unistd::close(libc::STDOUT_FILENO)?;

            unistd::close(libc::STDERR_FILENO)?;
        }

        {
            anyhow::ensure!(
                fcntl::open("/dev/null", OFlag::O_RDWR, Mode::empty())? == libc::STDIN_FILENO
            );

            anyhow::ensure!(unistd::dup(libc::STDIN_FILENO)? == libc::STDOUT_FILENO);

            anyhow::ensure!(unistd::dup(libc::STDIN_FILENO)? == libc::STDERR_FILENO);
        }

        let fork_result = unsafe { unistd::fork() }?;

        if let ForkResult::Parent { .. } = fork_result {
            process::exit(libc::EXIT_SUCCESS);
        }
    }

    let threshold_to_use = if let Some(ue) = memory_free_percent {
        anyhow::ensure!((1_u8..100_u8).contains(&ue));

        f64::from(ue) / 100_f64
    } else {
        DEFAULT_THRESHOLD
    };

    tracing::info!("Threshold being used: {threshold_to_use}");

    tracing::info!("Creating `{}`", name_of_type!(Runtime));

    let runtime = runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let result = runtime.block_on(async {
        let hash_map = HashMap::from([("urgency", &VALUE)]);

        let first_meminfo = Meminfo::current()?;

        let mem_total = first_meminfo.mem_total;

        let mem_total_float = u_six_four_to_f_six_four(mem_total)?;

        let total_memory_gibibytes_string = bytes_to_gibibytes_string(mem_total)?;

        let arc= Arc::new(Mutex::new(Cell::new(Option::<u32>::None)));

        let first_join_handle_cancellation_token = CancellationToken::new();

        let first_join_handle = {
            let arc = arc.clone();
            let first_join_handle_cancellation_token = first_join_handle_cancellation_token.clone();

            runtime.spawn(async move {
                let connection = zbus::Connection::session().await?;

                let dbus_proxy_proxy = DbusProxyProxy::new(&connection).await?;

                let mut notification_closed_stream =
                    dbus_proxy_proxy.receive_notification_closed().await?;

                loop {
                    tokio::select! {
                        () = first_join_handle_cancellation_token.cancelled() => {
                            tracing::error!("\"{}\" received, breaking", name_of!(first_join_handle_cancellation_token));

                            break;
                        },
                        op = notification_closed_stream.next() => {
                            let no = op.context("TODO")?;

                            tracing::info!("Received `{}` from \"{}\"", name_of_type!(NotificationClosed), name_of!(notification_closed_stream));

                            {
                                let mut mutex_guard = arc.lock().map_err(|po| anyhow::anyhow!("{po:?}"))?;

                                tracing::info!("Acquired `MutexGuard` from \"{}\"", name_of!(arc));

                                if let Some(ut) = mutex_guard.get_mut() {
                                    let notification_closed_args = no.args()?;

                                    let id = notification_closed_args.id();

                                    if ut == id {
                                        mutex_guard.set(None);
                                    }
                                }

                                drop(mutex_guard);
                            }
                        }
                    }
                }

                anyhow::Ok(())
            })
        };

        let first_join_handle_name_of = name_of!(first_join_handle);

        let second_join_handle_cancellation_token = CancellationToken::new();

        let second_join_handle = {
            let arc = arc.clone();
            let second_join_handle_cancellation_token =
                second_join_handle_cancellation_token.clone();

            runtime.spawn(async move {
                tracing::debug!("Inside \"spawn\"");

                let connection = zbus::Connection::session().await?;

                let dbus_proxy_proxy = DbusProxyProxy::new(&connection).await?;

                loop {
                    tracing::debug!("Start of loop");

                    let current_meminfo = Meminfo::current()?;

                    let mem_available = current_meminfo.mem_available.context("TODO")?;

                    let mem_free_float = u_six_four_to_f_six_four(mem_available)?;

                    let fraction_free = mem_free_float / mem_total_float;

                    tracing::debug!("\"{}\": {fraction_free}", name_of!(fraction_free));

                    let option = arc
                        .lock()
                        .map_err(|po| anyhow::anyhow!("{po:?}"))?
                        .get();

                    if fraction_free < threshold_to_use {
                        tracing::debug!(
                            "\"{}\" less than \"{}\"",
                            name_of!(fraction_free),
                            name_of!(threshold_to_use)
                        );

                        let (replaces_id, set_id) = {
                            match option {
                                Some(ut) => (ut, false),
                                // "A value of value of 0 means that this notification won't replace any existing notifications."
                                None => (0, true),
                            }
                        };

                        let fraction_in_use_percentage = (1_f64 - fraction_free) * 100_f64;

                        let free_memory_gibibytes_string = bytes_to_gibibytes_string(mem_available)?;

                        let used_memory_gibibytes_string = bytes_to_gibibytes_string(
                            mem_total - mem_available
                        )?;

                        let swap_total = current_meminfo.swap_total;

                        let swap_used = swap_total - current_meminfo.swap_free;

                        let swap_used_gibibytes_string = bytes_to_gibibytes_string(swap_used)?;

                        let swap_total_gibibytes_string = bytes_to_gibibytes_string(swap_total)?;

                        let body = format!(
                            "{fraction_in_use_percentage:.2}% of RAM ({total_memory_gibibytes_string} GiB) is in use!\n\
                            {free_memory_gibibytes_string} GiB is free\n\
                            {used_memory_gibibytes_string} GiB is used\n\
                            {swap_used_gibibytes_string} of {swap_total_gibibytes_string} GiB swap used"
                        );

                        // https://specifications.freedesktop.org/notification-spec/notification-spec-latest.html
                        // "The optional name of the application sending the notification. Can be blank."
                        // "The optional program icon of the calling application. See Icons and Images. Can be an empty string, indicating no icon."
                        // "Can be empty."
                        // "If 0, never expire."
                        let id = dbus_proxy_proxy.notify(
                            "",
                            replaces_id,
                            "",
                            "Low RAM",
                            &body,
                            &SLICE,
                            &hash_map,
                            0_i32
                        ).await?;

                        if set_id {
                            arc.lock()
                                .map_err(|po| anyhow::anyhow!("{po:?}"))?
                                .set(Some(id));
                        }
                    } else if let Some(ut) = option {
                        dbus_proxy_proxy.close_notification(ut).await?;
                    }

                    tracing::debug!("About to sleep");

                    select! {
                        () = second_join_handle_cancellation_token.cancelled() => {
                            tracing::error!("\"{}\" received, breaking", name_of!(second_join_handle_cancellation_token));

                            break;
                        }
                        () = time::sleep(DURATION) => {
                            tracing::debug!("Finished sleeping without receiving cancellation signal, continuing loop");
                        },
                    }
                }

                anyhow::Ok(())
            })
        };

        let second_join_handle_name_of = name_of!(second_join_handle);

        let third_join_handle_cancellation_token = CancellationToken::new();

        let third_join_handle = {
            let first_join_handle_cancellation_token = first_join_handle_cancellation_token.clone();
            let second_join_handle_cancellation_token =
                second_join_handle_cancellation_token.clone();
            let third_join_handle_cancellation_token = third_join_handle_cancellation_token.clone();

            runtime.spawn(async move {
                let mut interrupt_signal = unix::signal(SignalKind::interrupt())?;
                let mut terminate_signal = unix::signal(SignalKind::terminate())?;

                select! {
                    _ = interrupt_signal.recv() => {
                        tracing::error!("SIGINT received, shutting down");
                    },
                    _ = terminate_signal.recv() => {
                        tracing::error!("SIGTERM received, shutting down");
                    },
                }

                tracing::error!("Cancelling CancellationTokens");

                first_join_handle_cancellation_token.cancel();
                second_join_handle_cancellation_token.cancel();

                loop {
                    select! {
                        _ = interrupt_signal.recv() => {
                            tracing::error!("SIGINT received");
                        },
                        _ = terminate_signal.recv() => {
                            tracing::error!("SIGTERM received");
                        },
                        () = third_join_handle_cancellation_token.cancelled() => {
                            tracing::error!("\"{}\" received, breaking", name_of!(third_join_handle_cancellation_token));

                            break;
                        }
                    }
                }

                anyhow::Ok(())
            })
        };

        let third_join_handle_name_of = name_of!(third_join_handle);

        tracing::error!("Awaiting `JoinHandle`s");

        first_join_handle.await??;

        tracing::error!("\"{first_join_handle_name_of}\" has been awaited");

        second_join_handle.await??;

        tracing::error!("\"{second_join_handle_name_of}\" has been awaited");

        // Clear any existing notification
        let option = arc
            .lock()
            .map_err(|po| anyhow::anyhow!("{po:?}"))?
            .get();

        if let Some(ut) = option {
            tracing::error!("Attempting to close open notification (id: {ut})");

            let connection = zbus::Connection::session().await?;

            let dbus_proxy_proxy = DbusProxyProxy::new(&connection).await?;

            dbus_proxy_proxy.close_notification(ut).await?;

            tracing::error!("Successfully closed open notification (id: {ut})");
        }

        // Safe to shutdown shutdown listener now
        third_join_handle_cancellation_token.cancel();

        third_join_handle.await??;

        tracing::error!("\"{third_join_handle_name_of}\" has been awaited");

        tracing::error!("Exiting future passed to \"block_on\"");

        anyhow::Ok(())
    });

    tracing::error!("Exiting");

    result
}

fn u_six_four_to_f_six_four(i: u64) -> anyhow::Result<f64> {
    anyhow::ensure!(i < MAXIMUM);

    #[allow(clippy::cast_precision_loss)]
    {
        Ok(i as f64)
    }
}

fn bytes_to_gibibytes_string(bytes: u64) -> anyhow::Result<String> {
    let bytes_float = u_six_four_to_f_six_four(bytes)?;

    // 1024^3 = 1,073,741,824
    let bytes_gibibytes = bytes_float / 1_073_741_824_f64;

    let bytes_gibibytes_string = format!("{bytes_gibibytes:.2}");

    Ok(bytes_gibibytes_string)
}
