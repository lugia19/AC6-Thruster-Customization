//! Feeds the custom generator flames (fxr/build_fxrs.mjs) the player's thruster color (lib.rs STORAGE_COLOR_OFFSET in
//! the paint block) through FXR external values, red, green, blue each: 900001-900003 what the additive copy of a flame
//! adds, 900004-900006 what the subtractive copy removes from the background (both copies play together; see
//! set_channels), 900007-900009 the plain color, for alpha-blended particles (smoke). Served as 1 + the amount (1..2),
//! so 0 keeps meaning "no DLL" (the flames' wrappers then play stock).
//!
//! Effects read external values through 0x142b56480, effectively `float *get(value_block, float *out, int id)`.
//!
//! The player's paint block: [[[GameDataMan]+8]+0x788] + 0x14C (verified in game: the Paint screen edits this block
//! in place; CE table AC6_TGA labels its first section "Head" at 0x108 + 0x44). A polling thread finds GameDataMan,
//! reads the chain with checked reads (it's null while loading) and caches the color for the hook.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use crate::{Hook, STORAGE_COLOR_OFFSET, log, orig, player_flames, read};

const EXT_FIRST_CHANNEL: i32 = 900001;
const CHANNEL_COUNT: usize = 9;
const POLL_INTERVAL: Duration = Duration::from_millis(200);
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
// Served values for 900001-900009 (add, remove, plain: red, green, blue each), as f32 bits (see set_channels).
static CHANNELS: [AtomicU32; CHANNEL_COUNT] = [const { AtomicU32::new(0) }; CHANNEL_COUNT];

type GetExtValueFn = unsafe extern "system" fn(*mut u8, *mut u32, i32) -> *mut u32;

/// The external value read hook, for lib.rs's hook table.
pub(crate) fn hook() -> Hook {
    // mov [rsp+18],r8d / mov [rsp+10],rdx
    (0x142b56480, [0x44, 0x89, 0x44, 0x24, 0x18, 0x48], get_ext_value_impl as *const (), &ORIG_GET_EXT_VALUE)
}

unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> usize;
}

/// Called for every external value read of every effect. Our IDs don't need the game's lookup at all (nothing else
/// provides them), so it's skipped for them: the color channels, and whether the flame is the player's
/// (player_flames.rs).
unsafe extern "system" fn get_ext_value_impl(block: *mut u8, out: *mut u32, id: i32) -> *mut u32 {
    let color = (EXT_FIRST_CHANNEL..EXT_FIRST_CHANNEL + CHANNEL_COUNT as i32).contains(&id);
    let players_flame = id == player_flames::EXT_PLAYERS_FLAME;
    if out.is_null() || !(color || players_flame) {
        return unsafe { orig::<GetExtValueFn>(&ORIG_GET_EXT_VALUE)(block, out, id) };
    }
    let value = if color {
        CHANNELS[(id - EXT_FIRST_CHANNEL) as usize].load(Ordering::Relaxed)
    } else {
        player_flames::served_value(block as usize).to_bits()
    };
    unsafe { *out = value };
    out
}

/// Whether `bytes` matches GAME_DATA_MAN_PATTERN (None = any byte).
fn matches_pattern(bytes: &[u8]) -> bool {
    GAME_DATA_MAN_PATTERN.iter().zip(bytes).all(|(expected, actual)| match expected {
        Some(byte) => byte == actual,
        None => true,
    })
}

/// Finds GameDataMan's static pointer: the game exe's code section is searched for GAME_DATA_MAN_PATTERN, whose
/// `mov rax,[rip+offset]` holds the pointer's address relative to the end of that instruction.
fn find_game_data_man() -> Option<usize> {
    // PE layout: the offset of the PE header is at 0x3c; in it, the section count at +6, the optional header's size at
    // +0x14 and the section headers right after the optional header (at +0x18 + its size), 0x28 bytes each: name at +0,
    // size at +8, address (relative to the module) at +12.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) };
    let pe_header = base + read::<u32>(base + 0x3c)? as usize;
    let section_count = read::<u16>(pe_header + 6)? as usize;
    let optional_header_size = read::<u16>(pe_header + 0x14)? as usize;
    let first_section = pe_header + 0x18 + optional_header_size;
    for i in 0..section_count {
        let section = first_section + i * 0x28;
        if read::<[u8; 8]>(section)? != *b".text\0\0\0" {
            continue;
        }
        let code_size = read::<u32>(section + 8)? as usize;
        let code_start = base + read::<u32>(section + 12)? as usize;
        let code = unsafe { std::slice::from_raw_parts(code_start as *const u8, code_size) };

        let hit = code.windows(GAME_DATA_MAN_PATTERN.len()).position(matches_pattern)?;
        // The mov is 7 bytes; its last 4 are the offset from the next instruction to the pointer.
        let mov_end = code_start + hit + 7;
        let offset = i32::from_le_bytes(code[hit + 3..hit + 7].try_into().ok()?);
        return Some(mov_end.wrapping_add_signed(offset as isize));
    }
    None
}

/// The stored thruster color (a u32 of RGBA bytes), if the player's paint chain resolves right now.
fn stored_color(game_data_man: usize) -> Option<u32> {
    let game_data = read::<usize>(game_data_man)?;
    let player = read::<usize>(game_data + PLAYER_DATA_OFFSET)?;
    let paint = read::<usize>(player + PAINT_DATA_OFFSET)?;
    if paint == 0 {
        return None;
    }
    read::<u32>(paint + PAINT_BLOCK_OFFSET + STORAGE_COLOR_OFFSET)
}

/// sRGB (what the color picker shows) to linear light.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// How bright a color counts as, from its brightest channel (0-1): 0 = dark (64/255 or less), 1 = bright (160/255 or
/// more), smoothly in between.
fn brightness(brightest: f32) -> f32 {
    let t = ((brightest * 255.0 - 64.0) / 96.0).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t) // smoothstep
}

/// Serves a 0xRRGGBB color: for each channel c (0-1),
/// - add (900001-900003): c for bright colors (what looked right in game); for dark ones its linear intensity, since the
///   flames multiply it into HDR colors up to ~50 (#050505 at 0.02 would still leave those above 1: white);
/// - remove (900004-900006): what the color lacks (1 - c), for dark colors only (additive flames can't get darker than
///   their background, so dark red darkens it in green and blue);
/// - plain (900007-900009): c.
///
/// Each is served as 1 + the value.
fn set_channels(rgb: u32) {
    let red = (rgb >> 16 & 0xFF) as f32 / 255.0;
    let green = (rgb >> 8 & 0xFF) as f32 / 255.0;
    let blue = (rgb & 0xFF) as f32 / 255.0;
    let bright = brightness(red.max(green).max(blue));
    for (i, c) in [red, green, blue].into_iter().enumerate() {
        let linear = srgb_to_linear(c);
        let add = linear + (c - linear) * bright;
        let remove = (1.0 - c) * (1.0 - bright);
        let plain = c;
        CHANNELS[i].store((1.0 + add).to_bits(), Ordering::Relaxed);
        CHANNELS[3 + i].store((1.0 + remove).to_bits(), Ordering::Relaxed);
        CHANNELS[6 + i].store((1.0 + plain).to_bits(), Ordering::Relaxed);
    }
}

/// Stored color -> 0xRRGGBB, with unset (black) meaning the default.
fn to_rgb(stored: u32) -> u32 {
    let [r, g, b, _] = stored.to_le_bytes();
    let rgb = (r as u32) << 16 | (g as u32) << 8 | b as u32;
    if rgb == 0 { DEFAULT_COLOR } else { rgb }
}

fn poll_loop() {
    let Some(game_data_man) = find_game_data_man() else {
        log("thrusters: GameDataMan pattern not found, flames stay at the default color");
        return;
    };
    log(&format!("thrusters: GameDataMan at {game_data_man:#x}"));
    let mut last = None;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let Some(stored) = stored_color(game_data_man) else { continue };
        if last != Some(stored) {
            last = Some(stored);
            let rgb = to_rgb(stored);
            set_channels(rgb);
            log(&format!("thrusters: player's stored color {stored:08x} -> flames #{rgb:06X}"));
        }
    }
}

pub(crate) fn start() {
    set_channels(DEFAULT_COLOR);
    std::thread::spawn(poll_loop);
}
