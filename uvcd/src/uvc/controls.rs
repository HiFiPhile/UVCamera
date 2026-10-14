// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

//! Camera terminal and processing unit controls we expose to the host, and
//! which of them the camera supports.

use std::collections::BTreeMap;

/// A control the host can read and set. Values travel as i32 in our
/// protocol; [`ControlId::len`] bytes on the wire to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlId {
    /// PU brightness, 0..=255, 128 = no change.
    Brightness,
    /// CT zoom (absolute), in hundredths of the camera's zoom ratio.
    Zoom,
    /// CT focus, auto (0/1).
    FocusAuto,
    /// CT focus (absolute), 0 = nearest .. max = infinity.
    Focus,
    /// PU white balance temperature, auto (0/1).
    WhiteBalanceAuto,
    /// PU white balance temperature, kelvin.
    WhiteBalanceTemperature,
    /// PU power line frequency: 0 off, 1 50 Hz, 2 60 Hz, 3 auto.
    PowerLineFrequency,
}

/// Where a control lives in the VideoControl interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    CameraTerminal,
    ProcessingUnit,
}

impl ControlId {
    pub const ALL: [Self; 7] = [
        Self::Brightness,
        Self::Zoom,
        Self::FocusAuto,
        Self::Focus,
        Self::WhiteBalanceAuto,
        Self::WhiteBalanceTemperature,
        Self::PowerLineFrequency,
    ];

    /// Id in our protocol.
    pub fn wire(self) -> u8 {
        match self {
            Self::Brightness => 1,
            Self::Zoom => 2,
            Self::FocusAuto => 3,
            Self::Focus => 4,
            Self::WhiteBalanceAuto => 5,
            Self::WhiteBalanceTemperature => 6,
            Self::PowerLineFrequency => 7,
        }
    }

    pub fn from_wire(id: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.wire() == id)
    }

    pub fn unit(self) -> Unit {
        match self {
            Self::Zoom | Self::FocusAuto | Self::Focus => Unit::CameraTerminal,
            _ => Unit::ProcessingUnit,
        }
    }

    /// Control selector (UVC 1.5 tables A-12 and A-13).
    pub fn selector(self) -> u8 {
        match self {
            Self::Brightness => 0x02,
            Self::PowerLineFrequency => 0x05,
            Self::Focus => 0x06,
            Self::FocusAuto => 0x08,
            Self::WhiteBalanceTemperature => 0x0a,
            Self::WhiteBalanceAuto => 0x0b,
            Self::Zoom => 0x0b,
        }
    }

    pub fn from_selector(unit: Unit, selector: u8) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.unit() == unit && c.selector() == selector)
    }

    /// Size of the value on the wire.
    pub fn len(self) -> usize {
        match self {
            Self::FocusAuto | Self::WhiteBalanceAuto | Self::PowerLineFrequency => 1,
            _ => 2,
        }
    }

    /// Brightness is the only signed one we expose.
    pub fn signed(self) -> bool {
        self == Self::Brightness
    }

    /// Bit index in the unit's bmControls bitmap.
    fn bm_bit(self) -> usize {
        match self {
            Self::Brightness => 0,
            Self::Focus => 5,
            Self::WhiteBalanceTemperature => 6,
            Self::Zoom => 9,
            Self::PowerLineFrequency => 10,
            Self::WhiteBalanceAuto => 12,
            Self::FocusAuto => 17,
        }
    }

    /// The auto control that disables this one while on.
    pub fn auto_control(self) -> Option<Self> {
        match self {
            Self::Focus => Some(Self::FocusAuto),
            Self::WhiteBalanceTemperature => Some(Self::WhiteBalanceAuto),
            _ => None,
        }
    }

    pub fn encode(self, value: i32) -> Vec<u8> {
        match self.len() {
            1 => vec![value as u8],
            _ if self.signed() => (value as i16).to_le_bytes().to_vec(),
            _ => (value as u16).to_le_bytes().to_vec(),
        }
    }

    pub fn decode(self, data: &[u8]) -> Option<i32> {
        match self.len() {
            1 => data.first().map(|&b| i32::from(b)),
            _ if data.len() < 2 => None,
            _ if self.signed() => Some(i32::from(i16::from_le_bytes([data[0], data[1]]))),
            _ => Some(i32::from(u16::from_le_bytes([data[0], data[1]]))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlRange {
    pub min: i32,
    pub max: i32,
    pub res: i32,
    pub def: i32,
}

impl ControlRange {
    pub fn contains(&self, value: i32) -> bool {
        (self.min..=self.max).contains(&value)
    }
}

pub const FOCUS_MAX: u16 = 1000;
pub const WB_MIN: i32 = 2800;
pub const WB_MAX: i32 = 6500;
pub const WB_DEF: i32 = 5000;

/// Which optional controls the camera supports, from the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlCaps {
    /// Zoom range in hundredths, e.g. (60, 1000) for 0.6x..10x.
    pub zoom: Option<(u16, u16)>,
    /// Manual focus and autofocus.
    pub focus: bool,
    /// AWB presets that approximate a colour temperature.
    pub white_balance: bool,
    /// AE antibanding modes.
    pub power_line: bool,
}

impl ControlCaps {
    /// Ranges of every exposed control. Brightness is always available.
    pub fn ranges(&self) -> BTreeMap<ControlId, ControlRange> {
        let r = |min, max, res, def| ControlRange { min, max, res, def };
        let mut map = BTreeMap::new();
        map.insert(ControlId::Brightness, r(0, 255, 1, 128));
        if let Some((min, max)) = self.zoom {
            let (min, max) = (i32::from(min), i32::from(max));
            map.insert(ControlId::Zoom, r(min, max, 1, 100.clamp(min, max)));
        }
        if self.focus {
            map.insert(ControlId::FocusAuto, r(0, 1, 1, 1));
            // Default at infinity, where autofocus would usually sit.
            map.insert(
                ControlId::Focus,
                r(0, i32::from(FOCUS_MAX), 1, i32::from(FOCUS_MAX)),
            );
        }
        if self.white_balance {
            map.insert(ControlId::WhiteBalanceAuto, r(0, 1, 1, 1));
            map.insert(
                ControlId::WhiteBalanceTemperature,
                r(WB_MIN, WB_MAX, 100, WB_DEF),
            );
        }
        if self.power_line {
            map.insert(ControlId::PowerLineFrequency, r(0, 3, 1, 3));
        }
        map
    }

    /// bmControls of a unit: `len` bytes, one bit per exposed control.
    fn bm_controls(&self, unit: Unit, len: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; len];
        for id in self.ranges().keys().filter(|c| c.unit() == unit) {
            let bit = id.bm_bit();
            bytes[bit / 8] |= 1 << (bit % 8);
        }
        bytes
    }

    /// Camera terminal bmControls (3 bytes). Auto-exposure mode (D1) is
    /// always set: hosts query it and the kernel's default has it.
    pub fn camera_terminal_bm_controls(&self) -> Vec<u8> {
        let mut bytes = self.bm_controls(Unit::CameraTerminal, 3);
        bytes[0] |= 1 << 1;
        bytes
    }

    /// Processing unit bmControls (2 bytes).
    pub fn processing_unit_bm_controls(&self) -> Vec<u8> {
        self.bm_controls(Unit::ProcessingUnit, 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmaps_follow_the_uvc_spec() {
        // Only brightness and AE mode: the kernel's defaults.
        let none = ControlCaps::default();
        assert_eq!(none.camera_terminal_bm_controls(), vec![0x02, 0, 0]);
        assert_eq!(none.processing_unit_bm_controls(), vec![0x01, 0]);

        let all = ControlCaps {
            zoom: Some((60, 1000)),
            focus: true,
            white_balance: true,
            power_line: true,
        };
        // AE mode D1, focus absolute D5, zoom absolute D9, focus auto D17.
        assert_eq!(all.camera_terminal_bm_controls(), vec![0x22, 0x02, 0x02]);
        // Brightness D0, WB temperature D6, power line D10, WB auto D12.
        assert_eq!(all.processing_unit_bm_controls(), vec![0x41, 0x14]);
    }

    #[test]
    fn selectors_round_trip_and_wire_ids_are_unique() {
        for id in ControlId::ALL {
            assert_eq!(ControlId::from_selector(id.unit(), id.selector()), Some(id));
            assert_eq!(ControlId::from_wire(id.wire()), Some(id));
        }
    }

    #[test]
    fn encoding() {
        assert_eq!(ControlId::Brightness.encode(-1), vec![0xff, 0xff]);
        assert_eq!(ControlId::Brightness.decode(&[0xff, 0xff]), Some(-1));
        assert_eq!(ControlId::Zoom.encode(1000), vec![0xe8, 0x03]);
        assert_eq!(ControlId::Zoom.decode(&[0xe8, 0x03]), Some(1000));
        assert_eq!(ControlId::PowerLineFrequency.encode(2), vec![2]);
        assert_eq!(ControlId::FocusAuto.decode(&[1]), Some(1));
        assert_eq!(ControlId::Focus.decode(&[1]), None);
    }

    #[test]
    fn zoom_default_is_1x_within_range() {
        let caps = ControlCaps {
            zoom: Some((200, 800)),
            ..Default::default()
        };
        assert_eq!(caps.ranges()[&ControlId::Zoom].def, 200);
        let wide = ControlCaps {
            zoom: Some((60, 1000)),
            ..Default::default()
        };
        assert_eq!(wide.ranges()[&ControlId::Zoom].def, 100);
    }
}
