//! Feeds the custom generator flames (fxr/build_fxrs.mjs) through FXR external values:
//! - 900001-900003 (red, green, blue): the player's thruster color (lib.rs STORAGE_COLOR_OFFSET in the paint block),
//!   served as 1 + a tint factor (1..2, see set_channels) so 0 keeps meaning "nothing provides this" (no DLL).
//! - 900006: 1 while the color is dark (brightest channel below DARK_BELOW): the flames then play their dark variant,
//!   which subtracts the inverted color from the background instead of adding light.
//! - 9001X0 / 9001X1 / 9001X2: the min / near / far camera-distance thresholds in meters for flame family X (stock
//!   effect IDs 4Xxxxx: 41 continuous/falling/end smoke, 43, 44 assault boost, 45 quick boost). Each flame's wrapper
//!   compares its own camera distance (game value 2000) against them: from min up to near it plays the tinted (or
//!   dark) variant, below min or above far its stock look. The player's AC is the one in that band (the camera sits closer during assault boost); NPC
//!   flames only turn tinted when they come that close (e.g. in melee). Thresholds come from Game\debug-logs\
//!   paint_menu_thresholds.txt (lines "<family> <min> <near> <far>", e.g. "44 5 30 35", re-read every second), else
//!   the defaults.
//!
//! Effects read external values through 0x142b56480, effectively `float *get(value_block, float *out, int id)`; the
//! hook lets the game's lookup run, then overwrites the result for these IDs only.
//!
//! Tuning aid (only while Game\debug-logs\paint_menu_debug.txt exists at startup): every custom flame reads its
//! family's min threshold first, so each such read also fetches that block's camera distance and counts it in the
//! family's 1 m histogram, logged every 5 s (seconds since start, percentiles, occupied buckets). The defaults below
//! come from such runs.
//!
//! The player's paint block: [[[GameDataMan]+8]+0x788] + 0x14C (verified in game: the Paint screen edits this block
//! in place; CE table AC6_TGA labels its first section "Head" at 0x108 + 0x44). A polling thread finds GameDataMan,
//! reads the chain with checked reads (it's null while loading) and caches the color for the hook.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::{Hook, STORAGE_COLOR_OFFSET, log, orig};

const EXT_RED: i32 = 900001;
const EXT_BLUE: i32 = 900003;
const EXT_DARK: i32 = 900006;
// Colors whose brightest channel (0-255) is below this use the dark variant.
const DARK_BELOW: u32 = 80;
// Family thresholds: EXT_THRESHOLDS_BASE + family * 10 + {0 min, 1 near, 2 far} (must match fxr/build_fxrs.mjs).
const EXT_THRESHOLDS_BASE: i32 = 900100;
const FAMILIES: usize = 10; // indexed by the family digit
const EXT_CAMERA_DISTANCE: i32 = 2000;
// Histogram buckets: 1 m each, the last one is "this far or more".
const DISTANCE_BUCKETS: usize = 151;
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const RELOAD_INTERVAL_TICKS: u32 = 5; // 1 s
const HISTOGRAM_INTERVAL_TICKS: u32 = 25; // 5 s
// Default [min, near, far] in meters of camera distance (value 2000). Measured on the player's AC in AC Test:
// 41 continuous 16-60 (21-39 cruising, higher with quick boosts), 43 17-33 (one 7 at spawn), 44 assault boost 18-29
// (the camera sits closer), 45 quick boost 18-58; arena NPCs mostly read 67-150+. Below min = probably between the
// camera and the player (stock).
const DEFAULT_THRESHOLDS: [f32; 3] = [12.0, 62.0, 67.0];
const FAMILY_THRESHOLDS: [(usize, [f32; 3]); 2] = [(3, [5.0, 40.0, 45.0]), (4, [12.0, 35.0, 40.0])];
const THRESHOLDS_FILE: &str = "paint_menu_thresholds.txt";
const DEBUG_FILE: &str = "paint_menu_debug.txt";
// 0xRRGGBB, for ACs whose thruster color was never set (section 9's colors are plain black then).
const DEFAULT_COLOR: u32 = 0xFF3030;

// GameDataMan's static pointer: `mov rax,[rip+x]` at the start of this pattern (CE table AC6_TGA v0.4.4).
const GAME_DATA_MAN_PATTERN: [Option<u8>; 17] = [
    None, Some(0x8b), Some(0x05), None, None, None, None, None, Some(0x85), Some(0xc0), None, None, None,
    Some(0x8b), Some(0x40), None, Some(0xc3),
];
const PLAYER_DATA_OFFSET: usize = 0x8;
const PAINT_DATA_OFFSET: usize = 0x788;
const PAINT_BLOCK_OFFSET: usize = 0x14c;

static ORIG_GET_EXT_VALUE: AtomicUsize = AtomicUsize::new(0);
// Served values for 900001-900003 (red, green, blue) and 900006 (dark), as f32 bits (see set_channels). start() fills
// in DEFAULT_COLOR until the poll thread reads the stored color.
static CHANNELS: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
static DARK: AtomicU32 = AtomicU32::new(0);
// Per family: [min, near, far] as f32 bits. All 0 (= stock) until the poll thread's first reload.
static THRESHOLDS: [[AtomicU32; 3]; FAMILIES] = [const { [const { AtomicU32::new(0) }; 3] }; FAMILIES];
static DEBUG: AtomicBool = AtomicBool::new(false);
// Per family: camera distances read by the custom flames since the last histogram line (debug only).
static DISTANCES: [[AtomicU32; DISTANCE_BUCKETS]; FAMILIES] =
    [const { [const { AtomicU32::new(0) }; DISTANCE_BUCKETS] }; FAMILIES];

type GetExtValueFn = unsafe extern "system" fn(*mut u8, *mut u32, i32) -> *mut u32;

/// The external value read hook, for lib.rs's hook table.
pub(crate) fn hook() -> Hook {
    // mov [rsp+18],r8d / mov [rsp+10],rdx
    (0x142b56480, [0x44, 0x89, 0x44, 0x24, 0x18, 0x48], get_ext_value_impl as *const (), &ORIG_GET_EXT_VALUE)
}

unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn ReadProcessMemory(process: isize, address: usize, buffer: *mut u8, size: usize, read: *mut usize) -> i32;
    fn GetModuleHandleW(name: *const u16) -> usize;
}

/// Reads memory that may not be mapped (pointer chains while loading) without faulting.
fn read<T: Copy + Default>(addr: usize) -> Option<T> {
    let mut v = T::default();
    let mut n = 0;
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr, &mut v as *mut T as *mut u8, size_of::<T>(), &mut n) };
    (ok != 0 && n == size_of::<T>()).then_some(v)
}

/// Called for every external value read of every effect: anything outside our IDs goes straight to the game after
/// one comparison. Our own IDs don't need the game's lookup at all (nothing else provides them), so it's skipped.
unsafe extern "system" fn get_ext_value_impl(block: *mut u8, out: *mut u32, id: i32) -> *mut u32 {
    let orig = unsafe { orig::<GetExtValueFn>(&ORIG_GET_EXT_VALUE) };
    if !(EXT_RED..EXT_THRESHOLDS_BASE + FAMILIES as i32 * 10).contains(&id) || out.is_null() {
        return unsafe { orig(block, out, id) };
    }
    let value = if id <= EXT_BLUE {
        CHANNELS[(id - EXT_RED) as usize].load(Ordering::Relaxed)
    } else if id == EXT_DARK {
        DARK.load(Ordering::Relaxed)
    } else {
        let rel = id - EXT_THRESHOLDS_BASE;
        let (family, kind) = ((rel / 10) as usize, (rel % 10) as usize);
        if rel < 0 || kind > 2 {
            return unsafe { orig(block, out, id) };
        }
        if kind == 0 && DEBUG.load(Ordering::Relaxed) {
            let mut distance = 0u32;
            unsafe { orig(block, &mut distance, EXT_CAMERA_DISTANCE) };
            let meters = f32::from_bits(distance);
            if meters.is_finite() && meters >= 0.0 {
                DISTANCES[family][(meters as usize).min(DISTANCE_BUCKETS - 1)].fetch_add(1, Ordering::Relaxed);
            }
        }
        THRESHOLDS[family][kind].load(Ordering::Relaxed)
    };
    unsafe { *out = value };
    out
}

fn find_game_data_man() -> Option<usize> {
    let base = unsafe { GetModuleHandleW(std::ptr::null()) };
    let pe = base + read::<u32>(base + 0x3c)? as usize;
    let sections = read::<u16>(pe + 6)? as usize;
    let first_section = pe + 0x18 + read::<u16>(pe + 0x14)? as usize;
    for i in 0..sections {
        let header = first_section + i * 0x28;
        if read::<[u8; 8]>(header)? != *b".text\0\0\0" {
            continue;
        }
        let size = read::<u32>(header + 8)? as usize;
        let start = base + read::<u32>(header + 12)? as usize;
        let code = unsafe { std::slice::from_raw_parts(start as *const u8, size) };
        let n = GAME_DATA_MAN_PATTERN.len();
        let hit = code.windows(n).position(|w| GAME_DATA_MAN_PATTERN.iter().zip(w).all(|(p, b)| p.is_none_or(|p| p == *b)))?;
        let rel = i32::from_le_bytes(code[hit + 3..hit + 7].try_into().ok()?);
        return Some((start + hit + 7).wrapping_add_signed(rel as isize));
    }
    None
}

/// The stored thruster color (a u32 of RGBA bytes), if the player's paint chain resolves right now.
fn stored_color(game_data_man: usize) -> Option<u32> {
    let player = read::<usize>(read::<usize>(game_data_man)? + PLAYER_DATA_OFFSET)?;
    let paint = read::<usize>(player + PAINT_DATA_OFFSET).filter(|&p| p != 0)?;
    read::<u32>(paint + PAINT_BLOCK_OFFSET + STORAGE_COLOR_OFFSET)
}

/// The value served for one 0-255 color channel: 1 + its tint factor. Bright colors use the plain factor c/255 (what
/// looked right in game). The flames multiply the tint into HDR colors up to ~50 in the brightest stock parts, so a
/// dark pick like #050505 (0.02 per channel) still left those above 1 in every channel: white. Dark colors therefore
/// use the channel's linear intensity (#050505 -> 0.0015), blended in smoothly as the color's brightest channel drops
/// from 160 to 64. Dark picks end up dim rather than white (additive flames can't get darker than their background).
fn channel_value(c: u32, brightest: u32) -> f32 {
    let s = c as f32 / 255.0;
    let linear = if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
    let t = ((brightest as f32 - 64.0) / 96.0).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t); // smoothstep: 0 = dark (linear), 1 = bright (plain)
    1.0 + linear + (s - linear) * t
}

/// Serves a 0xRRGGBB color.
/// Dark colors play the dark variant, which subtracts the inverted color: there the plain channel (c/255) keeps the
/// hue visible. Others play the tinted variant (see channel_value).
fn set_channels(rgb: u32) {
    let channels = [rgb >> 16 & 0xFF, rgb >> 8 & 0xFF, rgb & 0xFF];
    let brightest = channels.into_iter().max().unwrap_or(0);
    let dark = brightest < DARK_BELOW;
    DARK.store(if dark { 1.0f32 } else { 0.0 }.to_bits(), Ordering::Relaxed);
    for (slot, c) in CHANNELS.iter().zip(channels) {
        let value = if dark { 1.0 + c as f32 / 255.0 } else { channel_value(c, brightest) };
        slot.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// Stored color -> 0xRRGGBB, with unset (black) meaning the default.
fn to_rgb(stored: u32) -> u32 {
    let [r, g, b, _] = stored.to_le_bytes();
    let rgb = (r as u32) << 16 | (g as u32) << 8 | b as u32;
    if rgb == 0 { DEFAULT_COLOR } else { rgb }
}

/// One "<family> <min> <near> <far>" line (family as 4X or X; meters); None if malformed.
fn parse_threshold_line(line: &str) -> Option<(usize, [f32; 3])> {
    let mut parts = line.split_whitespace();
    let family: usize = parts.next()?.parse().ok()?;
    let family = if (40..50).contains(&family) { family - 40 } else { family };
    let values: Vec<f32> = parts.map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let [min, near, far] = values.try_into().ok()?;
    (family < FAMILIES && min <= near && near <= far).then_some((family, [min, near, far]))
}

/// Re-reads the thresholds file when it changes; families it doesn't list (or a missing file) get the defaults.
fn reload_thresholds(path: &Path, last_modified: &mut Option<SystemTime>) {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if modified == *last_modified {
        return;
    }
    *last_modified = modified;
    let mut table = [DEFAULT_THRESHOLDS; FAMILIES];
    for (family, values) in FAMILY_THRESHOLDS {
        table[family] = values;
    }
    let mut from_file = Vec::new();
    for line in std::fs::read_to_string(path).unwrap_or_default().lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match parse_threshold_line(line) {
            Some((family, values)) => {
                table[family] = values;
                from_file.push(family);
            }
            None => log(&format!("thrusters: ignored threshold line {line:?} (want \"<family> <min> <near> <far>\")")),
        }
    }
    for (slots, values) in THRESHOLDS.iter().zip(table) {
        for (slot, value) in slots.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }
    let describe = |family: usize| {
        let [min, near, far] = table[family];
        let source = if from_file.contains(&family) { "file" } else { "default" };
        format!("4{family}: {min}-{near}, stock above {far} ({source})")
    };
    let mut listed: Vec<usize> = FAMILY_THRESHOLDS.iter().map(|&(f, _)| f).chain(from_file.iter().copied()).collect();
    listed.sort_unstable();
    listed.dedup();
    let [min, near, far] = DEFAULT_THRESHOLDS;
    let others = format!("others: {min}-{near}, stock above {far}");
    let summary: Vec<String> = listed.into_iter().map(describe).chain([others]).collect();
    log(&format!("thrusters: tinted from min up to near, in m: {}", summary.join("; ")));
}

/// Logs and resets one family's camera distance histogram (nothing if its flames weren't evaluated).
fn log_family_distances(family: usize, start: Instant) {
    let counts: Vec<u32> = DISTANCES[family].iter().map(|c| c.swap(0, Ordering::Relaxed)).collect();
    let total: u64 = counts.iter().map(|&c| c as u64).sum();
    if total == 0 {
        return;
    }
    let percentile = |p: f64| {
        let target = (total as f64 * p).ceil().max(1.0) as u64;
        let mut seen = 0;
        counts.iter().position(|&c| {
            seen += c as u64;
            seen >= target
        })
    };
    let fmt = |i: Option<usize>| i.map_or("?".to_string(), |i| if i == DISTANCE_BUCKETS - 1 { format!("{i}+") } else { i.to_string() });
    let buckets: Vec<String> = counts.iter().enumerate().filter(|(_, c)| **c > 0).map(|(i, c)| format!("{i}:{c}")).collect();
    log(&format!(
        "distances 4{family} @{:.0}s: {total} reads, min {} p10 {} median {} p90 {} max {} m | {}",
        start.elapsed().as_secs_f32(),
        fmt(percentile(0.0)),
        fmt(percentile(0.1)),
        fmt(percentile(0.5)),
        fmt(percentile(0.9)),
        fmt(counts.iter().rposition(|&c| c > 0)),
        buckets.join(" ")
    ));
}

fn poll_loop(log_dir: PathBuf) {
    let start = Instant::now();
    let thresholds_path = log_dir.join(THRESHOLDS_FILE);
    let mut thresholds_modified = Some(SystemTime::UNIX_EPOCH); // forces the first load
    reload_thresholds(&thresholds_path, &mut thresholds_modified);
    // Without it the flames keep the default color, but threshold reloads and histograms still run.
    let game_data_man = find_game_data_man();
    match game_data_man {
        Some(addr) => log(&format!("thrusters: GameDataMan at {addr:#x}")),
        None => log("thrusters: GameDataMan pattern not found, flames stay at the default color"),
    }
    let mut last = None;
    for tick in 1u32.. {
        std::thread::sleep(POLL_INTERVAL);
        if tick % RELOAD_INTERVAL_TICKS == 0 {
            reload_thresholds(&thresholds_path, &mut thresholds_modified);
        }
        if tick % HISTOGRAM_INTERVAL_TICKS == 0 && DEBUG.load(Ordering::Relaxed) {
            (1..FAMILIES).for_each(|family| log_family_distances(family, start));
        }
        let Some(stored) = game_data_man.and_then(stored_color) else { continue };
        let rgb = to_rgb(stored);
        set_channels(rgb);
        if last != Some(stored) {
            last = Some(stored);
            log(&format!("thrusters: player's stored color {stored:08x} -> flames #{rgb:06X}"));
        }
    }
}

pub(crate) fn start(log_dir: PathBuf) {
    set_channels(DEFAULT_COLOR);
    if log_dir.join(DEBUG_FILE).exists() {
        DEBUG.store(true, Ordering::Relaxed);
        log("thrusters: debug on, logging camera distance histograms");
    }
    std::thread::spawn(move || poll_loop(log_dir));
}
