//! Tells the flame wrappers (fxr/build_fxrs.mjs) whether they're on the player's AC: external value 900010 reads
//! 0 = not known yet (or no DLL), 1 = someone else's, 2 = the player's. A wrapper decides once, at spawn.
//!
//! A flame is the player's when it's within PLAYERS_FLAME_RADIUS of the player's core and closer to it than to any
//! other AC's (so an enemy AC's flames stay its own even when it's touching the player). Only ACs spawn these flames:
//! other enemies take theirs from ThrustersSfxIdParam_NPC, which the mod doesn't change.
//!
//! The effect manager calls 0x141625930 `set_value(manager, handle, int id, float value)` every frame for every effect
//! it tracks, with id 2000 and the effect's camera distance (0x14162ab10); set_value packs the value into a block and
//! attaches it to the effect. The hook takes that call for wrapper flames, decides whether the flame is the player's,
//! and attaches the answer the same way, as 900010. The game then keeps it on the effect and serves it to the wrapper
//! itself (an effect only takes the values its FXR header lists, and the wrapper's lists the ones it reads).
//!
//! Coordinates (found with AC6_tools\fxr-owner-probe; AC6_tools\Research\Characters and positions):
//! - an effect's position: 0x142b4fed0 `get_transform(handle, out)` gives its 3x4 world matrix, translation in floats
//!   3, 7, 11;
//! - a character's feet: ChrIns + 0x150, in the map's local frame; the player's ChrIns is [WorldChrMan] + 0xA5A0;
//! - effects use a frame shifted from the local one by whole 256 m map tiles (0 in the AC Test, (-256, 0, -5376) in
//!   another map). The camera is in the effect frame (cached by the manager at + 0x210) and normally well within 128 m
//!   of the player, so rounding (camera - feet) to 256 m gives the shift exactly. Cutscene cameras can be farther
//!   away and round to the wrong tile, so the shift is kept (see MapShift) and only changes on a map load (the
//!   player's ChrIns changes) or after a new value has held for a while;
//! - every character: WorldChrMan holds [+0xa558] (int) character sets at [+0xa568], 0x18 bytes apart; a set has
//!   (int) slots at +0x10 and its characters at [+0x18] + slot * 0x10 (null when empty). ACs, the player's and
//!   enemies', all have the AC class's vtable (AC_VTABLE); every other character has a different one. The roster is
//!   walked every ROSTER_REFRESH by a thread (it's ~650 slots), keeping the live ones (HP above 0, at
//!   [[ChrIns + 0x320] + 0] + 0x278, as in the AC6_TGA CE table); the hook only reads the few ACs' feet.

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use crate::{Hook, orig};

const EXT_PLAYERS_FLAME: i32 = 900010;
const SOMEONE_ELSES: f32 = 1.0;
const PLAYERS: f32 = 2.0;

const CAMERA_DISTANCE_ID: i32 = 2000;
// The wrappers are stock flame + WRAPPER_OFFSET (fxr/build_fxrs.mjs); the stock flames are the thruster flames
// (41xxxx-45xxxx) and the assault boost startup flare.
const WRAPPER_OFFSET: i32 = 9_000_000;
const STOCK_THRUSTER_FLAMES: std::ops::Range<i32> = 400_000..500_000;
const STOCK_STARTUP_FLARE: i32 = 1_003_010;

const GET_TRANSFORM_ADDR: usize = 0x142b4fed0;
const WORLD_CHR_MAN_ADDR: usize = 0x144e0f598;
const PLAYER_INS_OFFSET: usize = 0xA5A0;
const CHR_FEET_OFFSET: usize = 0x150;
const CHR_SET_COUNT_OFFSET: usize = 0xa558;
const CHR_SETS_OFFSET: usize = 0xa568;
const CHR_SET_STRIDE: usize = 0x18;
const SET_SLOT_COUNT_OFFSET: usize = 0x10;
const SET_SLOTS_OFFSET: usize = 0x18;
const SLOT_STRIDE: usize = 0x10;
const AC_VTABLE: usize = 0x1438a9858;
const CHR_MODULES_OFFSET: usize = 0x320;
const DATA_MODULE_HP_OFFSET: usize = 0x278;
const ROSTER_REFRESH: Duration = Duration::from_millis(200);
const MANAGER_CAMERA_OFFSET: usize = 0x210;
const MAP_TILE: f32 = 256.0;
// A new map shift is taken right away on a load (a new player ChrIns), otherwise only once it has held this long.
const NEW_SHIFT_HOLD: Duration = Duration::from_secs(2);

// The player's flames spawn within ~7 m of a point 5 m above the AC's feet (all boost types, biped and tetrapod
// threads); NPCs' only come that close when they're nearly inside the player's AC.
const CORE_HEIGHT: f32 = 5.0;
const PLAYERS_FLAME_RADIUS: f32 = 10.0;

static ORIG_SET_VALUE: AtomicUsize = AtomicUsize::new(0);
// The other ACs in the world (their characters), from the last roster walk.
static OTHER_ACS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static MAP_SHIFT: Mutex<MapShift> = Mutex::new(MapShift { current: None, player: 0, candidate: None });

type SetValueFn = unsafe extern "system" fn(usize, usize, i32, f32);
type GetTransformFn = unsafe extern "system" fn(usize, *mut [f32; 12]) -> *mut [f32; 12];

/// The camera distance hook, for lib.rs's hook table.
pub(crate) fn hook() -> Hook {
    // mov [rsp+20],rbp / push rsi
    (0x141625930, [0x48, 0x89, 0x6c, 0x24, 0x20, 0x56], set_value_impl as *const (), &ORIG_SET_VALUE)
}

unsafe extern "system" fn set_value_impl(manager: usize, handle: usize, id: i32, value: f32) {
    let set_value = unsafe { orig::<SetValueFn>(&ORIG_SET_VALUE) };
    unsafe { set_value(manager, handle, id, value) };
    if id == CAMERA_DISTANCE_ID && handle != 0 {
        if let Some(answer) = players_flame_answer(manager, handle) {
            unsafe { set_value(manager, handle, EXT_PLAYERS_FLAME, answer) };
        }
    }
}

fn is_wrapper(fxr_id: i32) -> bool {
    let stock = fxr_id - WRAPPER_OFFSET;
    STOCK_THRUSTER_FLAMES.contains(&stock) || stock == STOCK_STARTUP_FLARE
}

fn read_triple(address: usize) -> Option<[f32; 3]> {
    crate::read::<[f32; 3]>(address)
}

/// A pointer read that treats null as missing.
fn read_pointer(address: usize) -> Option<usize> {
    crate::read::<usize>(address).filter(|&p| p != 0)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The shift from the map's local frame to the effect frame, kept between frames.
struct MapShift {
    current: Option<[f32; 3]>,
    // The player's ChrIns the current shift was taken for; a new one means a load.
    player: usize,
    // A different shift the camera suggests, and since when.
    candidate: Option<([f32; 3], Instant)>,
}

impl MapShift {
    /// Takes this frame's suggestion (camera - feet rounded to tiles) and returns the shift to use.
    fn update(&mut self, suggested: [f32; 3], player: usize) -> [f32; 3] {
        let loaded = player != self.player;
        self.player = player;
        match self.current {
            Some(current) if current == suggested => {
                self.candidate = None;
            }
            Some(_) if !loaded => {
                // Different, with no load: keep the current shift unless this one has held for a while.
                let since = match self.candidate {
                    Some((candidate, since)) if candidate == suggested => since,
                    _ => Instant::now(),
                };
                self.candidate = Some((suggested, since));
                if since.elapsed() >= NEW_SHIFT_HOLD {
                    self.current = Some(suggested);
                    self.candidate = None;
                }
            }
            _ => {
                // First value, or a load.
                self.current = Some(suggested);
                self.candidate = None;
            }
        }
        self.current.unwrap_or(suggested)
    }
}

fn world_chr_man() -> Option<usize> {
    read_pointer(WORLD_CHR_MAN_ADDR)
}

fn player(world_chr_man: usize) -> Option<usize> {
    read_pointer(world_chr_man + PLAYER_INS_OFFSET)
}

/// A point CORE_HEIGHT above feet (map-local), in the effect frame.
fn core(feet: [f32; 3], map_shift: [f32; 3]) -> [f32; 3] {
    [feet[0] + map_shift[0], feet[1] + map_shift[1] + CORE_HEIGHT, feet[2] + map_shift[2]]
}

/// The player's core, and the map shift (see the module docs).
fn player_core_and_shift(manager: usize) -> Option<([f32; 3], [f32; 3])> {
    let player = player(world_chr_man()?)?;
    let feet = read_triple(player + CHR_FEET_OFFSET)?;
    let camera = read_triple(manager + MANAGER_CAMERA_OFFSET)?;
    let suggested: [f32; 3] = std::array::from_fn(|i| ((camera[i] - feet[i]) / MAP_TILE).round() * MAP_TILE);
    let map_shift = MAP_SHIFT.lock().ok()?.update(suggested, player);
    Some((core(feet, map_shift), map_shift))
}

fn is_alive(character: usize) -> bool {
    let Some(data_module) = read_pointer(character + CHR_MODULES_OFFSET).and_then(read_pointer) else { return false };
    crate::read::<i32>(data_module + DATA_MODULE_HP_OFFSET).is_some_and(|hp| hp > 0)
}

/// Every live AC in the world other than the player, from WorldChrMan's character sets (see the module docs). Empty
/// while the player can't be read (loading), so the player never ends up compared against itself.
fn other_acs() -> Vec<usize> {
    let mut acs = Vec::new();
    let Some(world_chr_man) = world_chr_man() else { return acs };
    let Some(player) = player(world_chr_man) else { return acs };
    let set_count = crate::read::<i32>(world_chr_man + CHR_SET_COUNT_OFFSET).unwrap_or(0).max(0) as usize;
    let Some(sets) = read_pointer(world_chr_man + CHR_SETS_OFFSET) else { return acs };
    for set_index in 0..set_count {
        let Some(set) = read_pointer(sets + set_index * CHR_SET_STRIDE) else { continue };
        let slot_count = crate::read::<i32>(set + SET_SLOT_COUNT_OFFSET).unwrap_or(0).max(0) as usize;
        let Some(slots) = read_pointer(set + SET_SLOTS_OFFSET) else { continue };
        for slot in 0..slot_count {
            let Some(character) = read_pointer(slots + slot * SLOT_STRIDE) else { continue };
            let is_ac = crate::read::<usize>(character) == Some(AC_VTABLE);
            if is_ac && character != player && is_alive(character) {
                acs.push(character);
            }
        }
    }
    acs
}

fn roster_loop() {
    loop {
        let acs = other_acs();
        if let Ok(mut other) = OTHER_ACS.lock() {
            *other = acs;
        }
        std::thread::sleep(ROSTER_REFRESH);
    }
}

pub(crate) fn start() {
    std::thread::spawn(roster_loop);
}

/// Whether a flame at `flame` is the player's: near the player's core, and no other AC's core is closer.
fn is_players_flame(flame: [f32; 3], player_core: [f32; 3], map_shift: [f32; 3]) -> bool {
    let to_player = distance(flame, player_core);
    if to_player >= PLAYERS_FLAME_RADIUS {
        return false;
    }
    let Ok(other_acs) = OTHER_ACS.lock() else { return true };
    other_acs
        .iter()
        .filter_map(|&ac| read_triple(ac + CHR_FEET_OFFSET))
        .all(|feet| distance(flame, core(feet, map_shift)) > to_player)
}

/// 900010 for the effect at `handle` if it's a wrapper flame with a position: PLAYERS or SOMEONE_ELSES.
fn players_flame_answer(manager: usize, handle: usize) -> Option<f32> {
    // Plain reads: this runs for every effect every frame, and both are safe: the manager has just taken this
    // handle's transform, and the original set_value has just attached through handle + 8 to the instance.
    let instance = unsafe { *((handle + 8) as *const usize) };
    if instance == 0 || !is_wrapper(unsafe { *(instance as *const i32) }) {
        return None;
    }
    let get_transform: GetTransformFn = unsafe { std::mem::transmute(GET_TRANSFORM_ADDR) };
    let mut matrix = [0f32; 12];
    unsafe { get_transform(handle, &mut matrix) };
    let flame = [matrix[3], matrix[7], matrix[11]];
    // A brand new effect can still be at the origin; wait for its real position.
    let placed = flame.iter().any(|v| v.abs() > 0.001);
    if !placed {
        return None;
    }
    let (player_core, map_shift) = player_core_and_shift(manager)?;
    Some(if is_players_flame(flame, player_core, map_shift) { PLAYERS } else { SOMEONE_ELSES })
}
