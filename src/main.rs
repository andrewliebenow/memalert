#![deny(clippy::all)]
#![warn(clippy::pedantic)]

use std::{
    cell::Cell,
    collections::HashMap,
    process,
    sync::{Arc, Mutex},
    time::Duration,
};

use clap::{command, Parser};
use futures::StreamExt;
use nameof::{name_of, name_of_type};
use procfs::{Current, Meminfo};
use tokio::runtime::Runtime;
use tokio::{
    runtime, select,
    signal::unix::{self, SignalKind},
    time,
};
use tokio_util::sync::CancellationToken;
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
struct MemalertArgs {}

const MAXIMUM: u64 = 1 << f64::MANTISSA_DIGITS;
const DURATION: Duration = Duration::from_secs(1);
const SLICE: [&str; 0] = [];
/// Display notification when the proportion of system memory that is free is smaller than this value
const THRESHOLD: f64 = 0.33_f64;
// "Critical"
const VALUE: Value = Value::U8(2);

#[allow(clippy::too_many_lines)]
fn main() {
    env_logger::init();

    let _ = MemalertArgs::parse();

    log::error!("Creating `{}`", name_of_type!(Runtime));

    let runtime = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut hash_map: HashMap<&str, &Value<'_>> = HashMap::new();
        hash_map.insert("urgency", &VALUE);

        let first_meminfo = Meminfo::current().unwrap();

        let mem_total = first_meminfo.mem_total;

        let mem_total_float = u_six_four_to_f_six_four(mem_total);

        let total_memory_gibibytes_string = bytes_to_gibibytes_string(mem_total);

        let arc: Arc<Mutex<Cell<Option<u32>>>> = Arc::new(Mutex::new(Cell::new(None)));

        let first_join_handle_cancellation_token = CancellationToken::new();

        let first_join_handle = {
            let arc = arc.clone();
            let first_join_handle_cancellation_token = first_join_handle_cancellation_token.clone();

            runtime.spawn(async move {
                let connection = zbus::Connection::session().await.unwrap();

                let dbus_proxy_proxy = DbusProxyProxy::new(&connection).await.unwrap();

                let mut notification_closed_stream = dbus_proxy_proxy
                    .receive_notification_closed().await
                    .unwrap();

                loop {
                    tokio::select! {
                        () = first_join_handle_cancellation_token.cancelled() => {
                            log::error!("\"{}\" received, breaking", name_of!(first_join_handle_cancellation_token));

                            break;
                        },
                        op = notification_closed_stream.next() => {
                            let no = op.unwrap();

                            log::info!("Received `{}` from \"{}\" @ {}", name_of_type!(NotificationClosed), name_of!(notification_closed_stream), line!());

                            {
                                let mut mutex_guard = arc.lock().unwrap();

                                log::info!("Acquired `MutexGuard` from \"{}\" @ {}", name_of!(arc), line!());

                                if let Some(ut) = mutex_guard.get_mut() {
                                    let notification_closed_args = no.args().unwrap();

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
            })
        };

        let first_join_handle_name_of = name_of!(first_join_handle);

        let second_join_handle_cancellation_token = CancellationToken::new();

        let second_join_handle = {
            let arc = arc.clone();
            let second_join_handle_cancellation_token =
                second_join_handle_cancellation_token.clone();

            runtime.spawn(async move {
                log::debug!("Inside \"spawn\" @ {}", line!());

                let connection = zbus::Connection::session().await.unwrap();

                let dbus_proxy_proxy = DbusProxyProxy::new(&connection).await.unwrap();

                loop {
                    log::debug!("Start of loop @ {}", line!());

                    let current_meminfo = Meminfo::current().unwrap();

                    let mem_available = current_meminfo.mem_available.unwrap();

                    let mem_free_float = u_six_four_to_f_six_four(mem_available);

                    let fraction_free = mem_free_float / mem_total_float;

                    log::debug!("\"{}\": {fraction_free} @ {}", name_of!(fraction_free), line!());

                    let option = arc.lock().unwrap().get();

                    if fraction_free < THRESHOLD {
                        log::debug!(
                            "\"{}\" less than \"{}\" @ {}",
                            name_of!(fraction_free),
                            name_of!(THRESHOLD),
                            line!()
                        );

                        let (replaces_id, set_id) = {
                            match option {
                                Some(ut) => (ut, false),
                                // "A value of value of 0 means that this notification won't replace any existing notifications."
                                None => (0, true),
                            }
                        };

                        let fraction_in_use_percentage = (1_f64 - fraction_free) * 100_f64;

                        let free_memory_gibibytes_string = bytes_to_gibibytes_string(mem_available);

                        let used_memory_gibibytes_string = bytes_to_gibibytes_string(
                            mem_total - mem_available
                        );

                        let swap_total = current_meminfo.swap_total;

                        let swap_used = swap_total - current_meminfo.swap_free;

                        let swap_used_gibibytes_string = bytes_to_gibibytes_string(swap_used);

                        let swap_total_gibibytes_string = bytes_to_gibibytes_string(swap_total);

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
                        let id = dbus_proxy_proxy
                            .notify(
                                "",
                                replaces_id,
                                "",
                                "Low RAM",
                                &body,
                                &SLICE,
                                &hash_map,
                                0
                            ).await
                            .unwrap();

                        if set_id {
                            arc.lock().unwrap().set(Some(id));
                        }
                    } else if let Some(ut) = option {
                        dbus_proxy_proxy.close_notification(ut).await.unwrap();
                    }

                    let sleep_duration = if fraction_free < 0.25 {
                        log::error!(
                            "\"{}\" less than 0.25, writing to \"/proc/sys/vm/drop_caches\" @ {}",
                            name_of!(fraction_free),
                            line!()
                        );

                        let result = process::Command
                            ::new("sh")
                            .args(["-c", "printf '2\n' | sudo tee /proc/sys/vm/drop_caches"])
                            .output();

                        log::error!(
                            "Wrote to \"/proc/sys/vm/drop_caches\". Sleeping for longer than usual so this operation isn't spammed.\n\
                            \n\
                            result: {result:?}"
                        );

                        Duration::from_secs(10)
                    } else {
                        DURATION
                    };

                    log::debug!("About to sleep @ {}", line!());

                    select! {
                        () = second_join_handle_cancellation_token.cancelled() => {
                            log::error!("\"{}\" received, breaking", name_of!(second_join_handle_cancellation_token));

                            break;
                        }
                        () = time::sleep(sleep_duration) => {
                            log::debug!("Finished sleeping without receiving cancellation signal, continuing loop");
                        },
                    }
                }
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
                let mut interrupt_signal = unix::signal(SignalKind::interrupt()).unwrap();
                let mut terminate_signal = unix::signal(SignalKind::terminate()).unwrap();

                select! {
                    _ = interrupt_signal.recv() => {
                        log::error!("SIGINT received, shutting down");
                    },
                    _ = terminate_signal.recv() => {
                        log::error!("SIGTERM received, shutting down");
                    },
                }

                log::error!("Cancelling CancellationTokens");

                first_join_handle_cancellation_token.cancel();
                second_join_handle_cancellation_token.cancel();

                loop {
                    select! {
                        _ = interrupt_signal.recv() => {
                            log::error!("SIGINT received");
                        },
                        _ = terminate_signal.recv() => {
                            log::error!("SIGTERM received");
                        },
                        () = third_join_handle_cancellation_token.cancelled() => {
                            log::error!("\"{}\" received, breaking", name_of!(third_join_handle_cancellation_token));

                            break;
                        }
                    }
                }
            })
        };

        let third_join_handle_name_of = name_of!(third_join_handle);

        log::error!("Awaiting `JoinHandle`s");

        first_join_handle.await.unwrap();

        log::error!("\"{first_join_handle_name_of}\" has been awaited");

        second_join_handle.await.unwrap();

        log::error!("\"{second_join_handle_name_of}\" has been awaited");

        // Clear any existing notification
        let option = arc.lock().unwrap().get();

        if let Some(ut) = option {
            log::error!("Attempting to close open notification (id: {ut})");

            let connection = zbus::Connection::session().await.unwrap();

            let dbus_proxy_proxy = DbusProxyProxy::new(&connection).await.unwrap();

            dbus_proxy_proxy.close_notification(ut).await.unwrap();

            log::error!("Successfully closed open notification (id: {ut})");
        }

        // Safe to shutdown shutdown listener now
        third_join_handle_cancellation_token.cancel();

        third_join_handle.await.unwrap();

        log::error!("\"{third_join_handle_name_of}\" has been awaited");

        log::error!("Exiting future passed to \"block_on\"");
    });

    log::error!("Exiting");
}

fn u_six_four_to_f_six_four(i: u64) -> f64 {
    assert!(i < MAXIMUM);

    #[allow(clippy::cast_precision_loss)]
    {
        i as f64
    }
}

fn bytes_to_gibibytes_string(bytes: u64) -> String {
    let bytes_float = u_six_four_to_f_six_four(bytes);

    // 1024^3 = 1,073,741,824
    let bytes_gibibytes = bytes_float / 1_073_741_824_f64;

    let bytes_gibibytes_string = format!("{bytes_gibibytes:.2}");

    bytes_gibibytes_string
}
