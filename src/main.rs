#![deny(clippy::all)]
#![warn(clippy::pedantic)]

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context;
use futures::StreamExt;
use nameof::{name_of, name_of_type};
use nix::unistd::{sysconf, SysconfVar};
use tokio::runtime::Runtime;
use tokio::{
    runtime, select,
    signal::unix::{self, SignalKind},
    time,
};
use tokio_util::sync::CancellationToken;
use zbus::{dbus_proxy, zvariant::Value};

#[dbus_proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait DbusProxy {
    #[dbus_proxy(name = "CloseNotification")]
    async fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[dbus_proxy(name = "NotificationClosed", signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::fdo::Result<()>;

    /// Notify (String app_name, UInt32 replaces_id, String app_icon, String summary, String body, Array of [String] actions, Dict of {String, Variant} hints, Int32 timeout) ↦ (UInt32 arg_0)
    #[allow(clippy::too_many_arguments)]
    #[dbus_proxy(name = "Notify")]
    async fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &Vec<&str>,
        hints: &HashMap<&str, &Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

const MAXIMUM: u64 = 1 << f64::MANTISSA_DIGITS;
const DURATION: Duration = Duration::from_secs(1);
/// Display notification when the proportion of system memory that is free is smaller than this value
const THRESHOLD: f64 = 0.55_f64;
// "Critical"
const VALUE: Value = Value::U8(2);
const VEC: Vec<&str> = vec![];

#[allow(clippy::too_many_lines)]
fn main() {
    env_logger::init();

    log::error!("Creating `{}`", name_of_type!(Runtime));

    let runtime = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    runtime.block_on(async {
        let mut hash_map: HashMap<&str, &Value<'_>> = HashMap::new();
        hash_map.insert("urgency", &VALUE);

        let phys_pages = sysconf(SysconfVar::_PHYS_PAGES).unwrap().context("").unwrap();

        let phys_pages_unsigned = u64::try_from(phys_pages).unwrap();

        let phys_pages_unsigned_floating = i_to_f(phys_pages_unsigned);

        let page_size = sysconf(SysconfVar::PAGE_SIZE).unwrap().context("").unwrap();

        let page_size_unsigned = u64::try_from(page_size).unwrap();

        let total_memory_gigabytes_string = pages_to_gigabytes(
            page_size_unsigned,
            phys_pages_unsigned
        );

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

                    let avphys_pages = sysconf(SysconfVar::_AVPHYS_PAGES)
                        .unwrap()
                        .context("")
                        .unwrap();

                    let avphys_pages_unsigned = u64::try_from(avphys_pages).unwrap();

                    let avphys_pages_unsigned_float = i_to_f(avphys_pages_unsigned);

                    let fraction_free = avphys_pages_unsigned_float / phys_pages_unsigned_floating;

                    log::debug!("\"{}\": {fraction_free} @ {}", name_of!(fraction_free), line!());

                    if fraction_free < THRESHOLD {
                        log::debug!(
                            "\"{}\" less than \"{}\" @ {}",
                            name_of!(fraction_free),
                            name_of!(THRESHOLD),
                            line!()
                        );

                        let fraction_in_use = 1_f64 - fraction_free;

                        let free_memory_gigabytes_string = pages_to_gigabytes(
                            page_size_unsigned,
                            avphys_pages_unsigned
                        );

                        // "A value of value of 0 means that this notification won't replace any existing notifications."
                        let replaces_id = arc.lock().unwrap().get().unwrap_or(0);

                        // Hack for rustfmt
                        let f = &free_memory_gigabytes_string;
                        let t = &total_memory_gigabytes_string;

                        let body = format!(
                            "{:.2}% of RAM is in use!\n({f} of {t} gigabytes are free)",
                            fraction_in_use * 100_f64
                        );

                        // https://specifications.freedesktop.org/notification-spec/notification-spec-latest.html
                        // "The optional name of the application sending the notification. Can be blank."
                        // "The optional program icon of the calling application. See Icons and Images. Can be an empty string, indicating no icon."
                        // "Can be empty."
                        // "If 0, never expire."
                        let id = dbus_proxy_proxy
                            .notify("", replaces_id, "", "Low RAM", &body, &VEC, &hash_map, 0).await
                            .unwrap();

                        arc.lock().unwrap().set(Some(id));
                    }

                    log::debug!("About to sleep @ {}", line!());

                    select! {
                        () = second_join_handle_cancellation_token.cancelled() => {
                            log::error!("\"{}\" received, breaking", name_of!(second_join_handle_cancellation_token));

                            break;
                        }
                        () = time::sleep(DURATION) => {
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

fn i_to_f(i: u64) -> f64 {
    assert!(i < MAXIMUM);

    #[allow(clippy::cast_precision_loss)]
    {
        i as f64
    }
}

fn pages_to_gigabytes(page_size: u64, pages: u64) -> String {
    let total = page_size.checked_mul(pages).unwrap();

    let total_float = i_to_f(total);

    let total_gigabytes = total_float / 1_000_000_000_f64;

    let total_gigabytes_string = format!("{total_gigabytes:.2}");

    total_gigabytes_string
}
