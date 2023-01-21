// Copyright 2022 witchof0x20
//
// This file is part of nezumi.
//
// nezumi is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version.
//
// nezumi is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with nezumi. If not, see <https://www.gnu.org/licenses/>.
use hidapi::{HidDevice, HidError};

pub fn get_mouse(model: &str, device: HidDevice) -> Result<Box<dyn Mouse>, GetMouseError> {
    match model {
        "steelseries_aerox_9_wired" => Ok(Box::new(aerox9::Wired::new(device))),
        "steelseries_aerox_9_wireless" => Ok(Box::new(aerox9::Wireless::new(device))),
        other => Err(GetMouseError(other.into())),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid model: {0}")]
pub struct GetMouseError(String);

pub mod aerox9 {
    use super::{BatteryStatus, HidDevice, HidError, Mouse};

    const OP_BATTERY_REQUEST: u8 = 0x92;
    const OP_BATTERY_RESPONSE_LEN: usize = 2;
    const FLAG_BATTERY_CHARGING: u8 = 0b10000000;
    const FLAG_WIRELESS: u8 = 0b01000000;

    fn battery_status_from_response(data: u8) -> Option<BatteryStatus> {
        let percent = u16::from(data & !FLAG_BATTERY_CHARGING).checked_sub(1)? * 5;
        if percent == 630 {
            None
        } else {
            Some(BatteryStatus {
                is_charging: data & FLAG_BATTERY_CHARGING != 0,
                percent,
            })
        }
    }

    pub struct Wired {
        device: HidDevice,
    }
    impl Mouse for Wired {
        fn new(device: HidDevice) -> Self {
            Wired { device }
        }
        fn battery(&self) -> Result<Option<BatteryStatus>, HidError> {
            // First, write the request
            self.device.write(&[0x00, OP_BATTERY_REQUEST])?;
            // Then, read a response
            let mut response = [0; OP_BATTERY_RESPONSE_LEN];
            self.device.read_timeout(&mut response, 200)?;
            // Extract fields
            Ok(battery_status_from_response(response[1]))
        }
    }
    pub struct Wireless {
        device: HidDevice,
    }
    impl Mouse for Wireless {
        fn new(device: HidDevice) -> Self {
            Wireless { device }
        }
        fn battery(&self) -> Result<Option<BatteryStatus>, HidError> {
            // First, write the request
            self.device
                .write(&[0x00, OP_BATTERY_REQUEST | FLAG_WIRELESS])?;
            // Then, read a response
            let mut response = [0; OP_BATTERY_RESPONSE_LEN];
            self.device.read_timeout(&mut response, 200)?;
            // Extract fields
            Ok(battery_status_from_response(response[1]))
        }
    }
}

pub trait Mouse {
    fn new(device: HidDevice) -> Self
    where
        Self: Sized;
    fn battery(&self) -> Result<Option<BatteryStatus>, HidError>;
}
#[derive(Debug)]
pub struct BatteryStatus {
    pub is_charging: bool,
    pub percent: u16,
}
