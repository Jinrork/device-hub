//! HID++ 2.0 IRoot discovery framing for offline analysis.
//! This module does not open a device or transmit a report.

const SHORT: u8 = 0x10;
const LONG: u8 = 0x11;
const ERROR: u8 = 0xff;

#[derive(Clone, Copy, Debug)]
pub enum Target {
    WiredMouse,
    // Use this only after separately verifying the paired mouse and slot.
    FirstReceiverSlot,
}

#[derive(Clone, Copy, Debug)]
pub enum Feature {
    DeviceInformation = 0x0003,
    Battery = 0x1004,
    ReprogrammableControls = 0x1b04,
    ExtendedDpi = 0x2202,
    ExtendedReportRate = 0x8061,
    ModeStatus = 0x8090,
    OnboardProfiles = 0x8100,
    MouseButtonSpy = 0x8110,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiscoveryReply {
    Present { index: u8, flags: u8, version: u8 },
    Absent,
    DeviceError(u8),
    Unrelated,
    Malformed,
}

pub struct GetFeature {
    device: u8,
    software_id: u8,
    feature: Feature,
}

impl GetFeature {
    pub fn new(target: Target, software_id: u8, feature: Feature) -> Option<Self> {
        if !(1..=15).contains(&software_id) {
            return None;
        }
        Some(Self {
            device: match target {
                Target::WiredMouse => 0xff,
                Target::FirstReceiverSlot => 1,
            },
            software_id,
            feature,
        })
    }

    pub fn short_report(&self) -> [u8; 7] {
        let [high, low] = (self.feature as u16).to_be_bytes();
        [SHORT, self.device, 0, self.software_id, high, low, 0]
    }

    pub fn classify_reply(&self, frame: &[u8]) -> DiscoveryReply {
        match frame.first() {
            Some(&SHORT) if frame.len() == 7 => {}
            Some(&LONG) if frame.len() == 20 => {}
            Some(&SHORT) | Some(&LONG) | None => return DiscoveryReply::Malformed,
            _ => return DiscoveryReply::Unrelated,
        }
        if frame[1] != self.device {
            return DiscoveryReply::Unrelated;
        }
        match *frame {
            [_, _, 0, software_id, index, flags, version, ..]
                if software_id == self.software_id =>
            {
                if index == 0 {
                    DiscoveryReply::Absent
                } else {
                    DiscoveryReply::Present {
                        index,
                        flags,
                        version,
                    }
                }
            }
            [_, _, ERROR, 0, software_id, error, ..] if software_id == self.software_id => {
                DiscoveryReply::DeviceError(error)
            }
            _ => DiscoveryReply::Unrelated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_feature_id_are_encoded_separately() {
        let wired = GetFeature::new(Target::WiredMouse, 9, Feature::MouseButtonSpy).unwrap();
        let wireless = GetFeature::new(Target::FirstReceiverSlot, 9, Feature::ExtendedDpi).unwrap();
        assert_eq!(wired.short_report(), [0x10, 0xff, 0, 9, 0x81, 0x10, 0]);
        assert_eq!(wireless.short_report(), [0x10, 1, 0, 9, 0x22, 2, 0]);
        assert!(GetFeature::new(Target::WiredMouse, 0, Feature::Battery).is_none());
        assert!(GetFeature::new(Target::WiredMouse, 16, Feature::Battery).is_none());
    }

    #[test]
    fn discovers_a_dynamic_index_in_a_long_reply() {
        let query = GetFeature::new(Target::FirstReceiverSlot, 9, Feature::MouseButtonSpy).unwrap();
        let mut report = [0; 20];
        report[..7].copy_from_slice(&[0x11, 1, 0, 9, 13, 0x40, 2]);
        assert_eq!(
            query.classify_reply(&report),
            DiscoveryReply::Present {
                index: 13,
                flags: 0x40,
                version: 2
            }
        );
        report[4] = 0;
        assert_eq!(query.classify_reply(&report), DiscoveryReply::Absent);
    }

    #[test]
    fn rejects_other_sessions_notifications_and_malformed_frames() {
        let query = GetFeature::new(Target::WiredMouse, 9, Feature::Battery).unwrap();
        for unrelated in [
            [0x10, 1, 0, 9, 1, 0, 0],
            [0x10, 0xff, 0, 8, 1, 0, 0],
            [0x10, 0xff, 13, 0, 0, 1, 0],
            [0x10, 0xff, 0xff, 0, 8, 2, 0],
        ] {
            assert_eq!(query.classify_reply(&unrelated), DiscoveryReply::Unrelated);
        }
        assert_eq!(query.classify_reply(&[0x11; 7]), DiscoveryReply::Malformed);
        assert_eq!(
            query.classify_reply(&[0x10, 0xff, 0xff, 0, 9, 2, 0]),
            DiscoveryReply::DeviceError(2)
        );
    }
}
