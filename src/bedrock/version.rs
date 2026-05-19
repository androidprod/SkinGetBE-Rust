use once_cell::sync::Lazy;
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};
use tracing::warn;

// Protocol -> Version mapping (ported from C++ Bedrock/Version.cpp)
static PROTOCOL_MAP: Lazy<HashMap<u32, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert(70, "0.14.3");
    m.insert(82, "0.15.6");
    m.insert(100, "1.0.0");
    m.insert(422, "1.16.201");
    m.insert(428, "1.16.210");
    m.insert(431, "1.16.220");
    m.insert(440, "1.17.0");
    m.insert(448, "1.17.10");
    m.insert(465, "1.17.30");
    m.insert(471, "1.17.40");
    m.insert(475, "1.18.0");
    m.insert(486, "1.18.11");
    m.insert(503, "1.18.30");
    m.insert(527, "1.19.1");
    m.insert(534, "1.19.10");
    m.insert(544, "1.19.20");
    m.insert(545, "1.19.21");
    m.insert(554, "1.19.30");
    m.insert(557, "1.19.40");
    m.insert(560, "1.19.50");
    m.insert(567, "1.19.60");
    m.insert(568, "1.19.63");
    m.insert(575, "1.19.70");
    m.insert(582, "1.19.80");
    m.insert(589, "1.20.0");
    m.insert(594, "1.20.10");
    m.insert(618, "1.20.30");
    m.insert(622, "1.20.40");
    m.insert(630, "1.20.50");
    m.insert(649, "1.20.61");
    m.insert(662, "1.20.71");
    m.insert(671, "1.20.80");
    m.insert(685, "1.21.0");
    m.insert(686, "1.21.2");
    m.insert(712, "1.21.20");
    m.insert(729, "1.21.30");
    m.insert(748, "1.21.42");
    m.insert(766, "1.21.50");
    m.insert(776, "1.21.60");
    m.insert(786, "1.21.70");
    m.insert(800, "1.21.80");
    m.insert(818, "1.21.90");
    m.insert(819, "1.21.93");
    m.insert(827, "1.21.100");
    m.insert(844, "1.21.111");
    m.insert(859, "1.21.120");
    m.insert(860, "1.21.124");
    m.insert(898, "1.21.130");
    m.insert(924, "1.26.0");
    m.insert(944, "1.26.10");
    m.insert(975, "1.26.21");
    m
});

static FALLBACK_VERSION: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));
static FALLBACK_PROTOCOL: Lazy<Mutex<u16>> = Lazy::new(|| Mutex::new(0));
static WARNED_UNKNOWN: Lazy<Mutex<HashSet<u32>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Resolve protocol number to a version string. If unknown, return fallback version.
pub fn resolve_version(protocol: u32) -> String {
    if let Some(v) = PROTOCOL_MAP.get(&protocol) {
        return v.to_string();
    }

    let mut warned = WARNED_UNKNOWN.lock().unwrap();
    if warned.insert(protocol) {
        let fb = {
            let fallback = FALLBACK_VERSION.lock().unwrap().clone();
            if fallback.is_empty() {
                get_latest_version()
            } else {
                fallback
            }
        };
        warn!(
            "Unknown Bedrock protocol {} - falling back to {}",
            protocol, fb
        );
    }

    let fallback = FALLBACK_VERSION.lock().unwrap().clone();
    if fallback.is_empty() {
        get_latest_version()
    } else {
        fallback
    }
}

pub fn get_latest_protocol() -> u16 {
    PROTOCOL_MAP
        .keys()
        .max()
        .copied()
        .map(|k| k as u16)
        .unwrap_or(*FALLBACK_PROTOCOL.lock().unwrap())
}

pub fn get_latest_version() -> String {
    PROTOCOL_MAP
        .iter()
        .max_by_key(|(&k, _)| k)
        .map(|(_, v)| v.to_string())
        .unwrap_or(FALLBACK_VERSION.lock().unwrap().clone())
}

pub fn set_fallback_version(v: &str) {
    *FALLBACK_VERSION.lock().unwrap() = v.to_string();
}

pub fn set_fallback_protocol(p: u16) {
    *FALLBACK_PROTOCOL.lock().unwrap() = p;
}
