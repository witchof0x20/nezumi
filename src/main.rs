mod mouse;

use crate::mouse::Mouse;
use clap::Parser;
use futures_util::stream::StreamExt;
use hex::FromHex;
use hidapi::{HidApi, HidDevice};
use linked_hash_map::LinkedHashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use tokio::time::{self, Duration, Instant};
use tokio_udev::{AsyncMonitorSocket, Event, EventType, MonitorBuilder};
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::FmtSubscriber;

/// Daemon to monitor mouse battery status
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to config
    #[arg(short, long, default_value = "mouse.toml")]
    config: PathBuf,
    /// How long to wait each time we check the battery
    #[arg(short, long, default_value_t = 30)]
    interval: u64,
}

/// Profile describing a mouse
#[derive(Debug, serde::Deserialize)]
struct MouseProfile {
    /// Model name of the mouse
    model: String,
    /// Product id
    #[serde(deserialize_with = "deserialize_id")]
    product: u16,
    /// Vendor id
    #[serde(deserialize_with = "deserialize_id")]
    vendor: u16,
    /// USB endpoint
    endpoint: i32,
}
fn deserialize_id<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let bytes: [u8; 2] = hex::serde::deserialize(deserializer)?;
    Ok(u16::from_be_bytes(bytes))
}

fn open_first_mouse<'a>(
    hid_api: &HidApi,
    mice: impl Iterator<Item = (&'a String, &'a MouseProfile)>,
) -> Result<Box<dyn Mouse>, OpenFirstMouseError> {
    let mut mouse = None;
    for (name, profile) in mice {
        for cur_device in hid_api.device_list() {
            if cur_device.vendor_id() == profile.vendor
                && cur_device.product_id() == profile.product
                && cur_device.interface_number() == profile.endpoint
            {
                info!("Found {name}");
                let device = cur_device.open_device(hid_api)?;
                let cur_mouse = mouse::get_mouse(&profile.model, device)?;
                mouse = Some(cur_mouse);
                break;
            }
        }
    }
    mouse.ok_or(OpenFirstMouseError::NotFound)
}
#[derive(Debug, thiserror::Error)]
enum OpenFirstMouseError {
    #[error("No mouse found")]
    NotFound,
    #[error("Error opening the found mouse: {0}")]
    OpenMouse(#[from] hidapi::HidError),
    #[error("Error wrapping the mouse device: {0}")]
    WrapMouse(#[from] crate::mouse::GetMouseError),
}

fn process_udev_event<'a>(
    event: &Event,
    mice: impl Iterator<Item = (&'a String, &'a MouseProfile)>,
) -> Result<bool, UdevEventError> {
    if event.event_type() == EventType::Bind {
        let device = event.device();
        let vendor_id = device
            .attribute_value("idVendor")
            .ok_or(UdevEventError::MissingVendor)?
            .to_str()
            .ok_or(UdevEventError::InvalidVendor)?;
        let vendor_id = <[u8; 2]>::from_hex(vendor_id)
            .map(u16::from_be_bytes)
            .map_err(|_| UdevEventError::InvalidVendor)?;
        let product_id = device
            .attribute_value("idProduct")
            .ok_or(UdevEventError::MissingProduct)?
            .to_str()
            .ok_or(UdevEventError::InvalidProduct)?;
        let product_id = <[u8; 2]>::from_hex(product_id)
            .map(u16::from_be_bytes)
            .map_err(|_| UdevEventError::InvalidProduct)?;
        for (name, profile) in mice {
            if profile.vendor == vendor_id && profile.product == product_id {
                info!("Device {name} has been connected");
                return Ok(true);
            }
        }
        Ok(false)
    } else {
        Ok(false)
    }
}
#[derive(Debug, thiserror::Error)]
enum UdevEventError {
    #[error("Event missing vendor id")]
    MissingVendor,
    #[error("Vendor id not in proper format")]
    InvalidVendor,
    #[error("Event missing product id")]
    MissingProduct,
    #[error("Product id not in proper format")]
    InvalidProduct,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    // Parse CLI args
    let args = Args::parse();
    // Initialize a logger
    // a builder for `FmtSubscriber`.
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .with_writer(io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    // Load the mouse config file
    let mouse_config = fs::read(args.config).map_err(Error::OpenConfig)?;
    let mouse_config: LinkedHashMap<String, MouseProfile> = toml::from_slice(&mouse_config)?;
    // Create a single sleep future
    // Initially we sleep for 0 (immediately get status)
    let sleep = time::sleep(Duration::from_secs(0));
    let interval = Duration::from_secs(args.interval);
    tokio::pin!(sleep);
    // Main loop
    loop {
        // Initialize hidapi
        let hid_api = HidApi::new().map_err(Error::InitializeHidApi)?;
        // Look through the list of mice and try to find one
        match open_first_mouse(&hid_api, mouse_config.iter()) {
            Ok(mouse) => {
                // Repeatedly send battery commands
                loop {
                    tokio::select! {
                        () = &mut sleep => {
                            // Get the battery status of the mouse
                            match mouse.battery() {
                                Ok(Some(battery_status)) => {
                                    println!(
                                        "\u{f8cc}{} {}%",
                                        if battery_status.is_charging { "\u{f0e7}" } else {""},
                                        battery_status.percent
                                    )
                                }
                                Ok(None) => warn!("Error in response, will try again"),
                                Err(err) => {
                                    error!("Error reading battery status: {err}");
                                    break;
                                }
                            }
                            // Wait for next interval
                            sleep.as_mut().reset(Instant::now() + interval);
                        },
                    }
                }
            }
            Err(err) => {
                error!("Error opening first mouse: {err}");
            }
        }
        // Print an empty line because we don't know the status of the mouse
        println!();
        // Do a udev wait loop until one of our desired mice show up
        info!("Using udev to wait until our mouse appears");
        let mut monitor: AsyncMonitorSocket = MonitorBuilder::new()
            .map_err(Error::UdevBuildMonitor)?
            .match_subsystem_devtype("usb", "usb_device")
            .map_err(Error::UdevBuildMonitor)?
            .listen()
            .map_err(Error::UdevListen)?
            .try_into()
            .map_err(Error::UdevAsync)?;
        // Set up the sleep timer to have a timeout before we stop checking udev
        sleep.as_mut().reset(Instant::now() + interval);
        // Process udev usb events
        while let Some(event) = tokio::select! {
            event = monitor.next() => { event },
            _ = &mut sleep => { None },
        } {
            match event {
                Ok(event) => match process_udev_event(&event, mouse_config.iter()) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(err) => {
                        error!("Unexpected error handling udev event: {err:?}");
                    }
                },
                Err(err) => error!("Error processing udev event: {err}"),
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("Error setting tracing subscriber: {0}")]
    SetTracingSubscriber(#[from] tracing::subscriber::SetGlobalDefaultError),
    #[error("Error opening config file: {0}")]
    OpenConfig(io::Error),
    #[error("Error parsing config file: {0}")]
    ParseConfig(#[from] toml::de::Error),
    #[error("Error initializing hidapi: {0}")]
    InitializeHidApi(hidapi::HidError),
    #[error("Error building udev monitor builder: {0}")]
    UdevBuildMonitor(io::Error),
    #[error("Error listening to udev: {0}")]
    UdevListen(io::Error),
    #[error("Error creating async udev socket: {0}")]
    UdevAsync(io::Error),
}
