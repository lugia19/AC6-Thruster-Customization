//! Tells the flame wrappers (fxr/build_fxrs.mjs) whether they're on the player's AC: external value 900010 reads
//! 0 = not known yet (or no DLL), 1 = someone else's, 2 = the player's. A wrapper decides once, at spawn.
//!
//! A flame is the player's when it's within PLAYERS_FLAME_RADIUS of the player's core and closer to it than to any
//! other AC's (so an enemy AC's flames stay its own even when it's touching the player). Only ACs spawn these flames:
//! other enemies take theirs from ThrustersSfxIdParam_NPC, which the mod doesn't change.
//!
//! The effect manager calls 0x141625930 `set_value(manager, handle, int id, float value)` every frame for every effect
//! it tracks, with id 2000 and the effect's camera distance (0x14162ab10). The hook takes that call for wrapper flames
//! to decide whether the flame is the player's (see is_players_flame) and stores the answer under the effect instance's
//! own value blocks (instance + 0x48 / + 0x50), which is what the wrapper's reads come with (thrusters.rs's getter).
//!
//! Coordinates (found with AC6_tools\fxr-owner-probe):
//! - an effect's position: 0x142b4fed0 `get_transform(handle, out)` gives its 3x4 world matrix, translation in floats
//!   3, 7, 11;
//! - the player's feet: [[WorldChrMan] + 0xA5A0] + 0x150, in the map's local frame;
//! - effects use a frame shifted from the local one by whole 256 m map tiles (0 in the AC Test, (-256, 0, -5376) in
//!   another map). The camera is in the effect frame (cached by the manager at + 0x210) and normally well within 128 m
//!   of the player, so rounding (camera - feet) to 256 m gives the shift exactly. Cutscene cameras can be farther
//!   away and round to the wrong tile, so the shift is kept (see MapShift) and only changes on a map load or after
//!   a new value has held for a while;
//! - every character: WorldChrMan holds [+0xa558] (int) character sets at [+0xa568], 0x18 bytes apart; a set has
//!   (int) slots at +0x10 and its characters at [+0x18] + slot * 0x10 (null when empty). ACs, the player's and
//!   enemies', all have the AC class's vtable (AC_VTABLE); every other character has a different one. The roster is
//!   walked every ROSTER_REFRESH by a thread (it's ~650 slots), and the hook only reads the few ACs' feet.

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{Hook, orig};

pub(crate) const EXT_PLAYERS_FLAME: i32 = 900010;
const NOT_KNOWN: f32 = 0.0;
const SOMEONE_ELSES: f32 = 1.0;
const PLAYERS: f32 = 2.0;

const CAMERA_DISTANCE_ID: i32 = 2000;
// The wrappers: stock thruster flames (41xxxx-45xxxx) + 9,000,000, and the assault boost startup flare's.
const WRAPPER_IDS: std::ops::Range<i32> = 9_400_000..9_500_000;
const STARTUP_FLARE_WRAPPER: i32 = 10_003_010;

const GET_TRANSFORM_ADDR: usize = 0x142b4fed0;
const WORLD_CHR_MAN_ADDR: usize = 0x144e0f598;
const PLAYER_INS_OFFSET: usize = 0xA5A0;
const PLAYER_FEET_OFFSET: usize = 0x150;
const CHR_SET_COUNT_OFFSET: usize = 0xa558;
const CHR_SETS_OFFSET: usize = 0xa568;
const CHR_SET_STRIDE: usize = 0x18;
const SET_SLOT_COUNT_OFFSET: usize = 0x10;
const SET_SLOTS_OFFSET: usize = 0x18;
const SLOT_STRIDE: usize = 0x10;
const AC_VTABLE: usize = 0x1438a9858;
const ROSTER_REFRESH: Duration = Duration::from_millis(200);
const MANAGER_CAMERA_OFFSET: usize = 0x210;
const INSTANCE_BLOCK_OFFSETS: [usize; 2] = [0x48, 0x50];
const MAP_TILE: f32 = 256.0;
// A new map shift is taken right away when the player's local position jumps this far (a load or teleport), otherwise
// only once it has held this long.
const MAP_LOAD_JUMP: f32 = 500.0;
const NEW_SHIFT_HOLD: Duration = Duration::from_secs(5);

// The player's flames spawn within ~7 m of a point 5 m above the AC's feet (all boost types, biped and tetrapod
// threads); NPCs' only come that close when they're nearly inside the player's AC.
const CORE_HEIGHT: f32 = 5.0;
const PLAYERS_FLAME_RADIUS: f32 = 10.0;
// The manager refreshes a live effect's answer every frame; older ones are a dead effect's, whose block address may
// have been reused.
const MAX_AGE: Duration = Duration::from_millis(200);

static ORIG_SET_VALUE: AtomicUsize = AtomicUsize::new(0);
// The other ACs in the world (their characters), from the last roster walk.
static OTHER_ACS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static MAP_SHIFT: Mutex<MapShift> = Mutex::new(MapShift { current: None, last_feet: None, candidate: None });
// Value block address -> (answer, when it was measured).
static ANSWERS: OnceLock<Mutex<HashMap<usize, (f32, Instant)>>> = OnceLock::new();

type SetValueFn = unsafe extern "system" fn(usize, usize, i32, f32);
type GetTransformFn = unsafe extern "system" fn(usize, *mut [f32; 12]) -> *mut [f32; 12];

/// The camera distance hook, for lib.rs's hook table.
pub(crate) fn hook() -> Hook {
    // mov [rsp+20],rbp / push rsi
    (0x141625930, [0x48, 0x89, 0x6c, 0x24, 0x20, 0x56], set_value_impl as *const (), &ORIG_SET_VALUE)
}

unsafe extern "system" fn set_value_impl(manager: usize, handle: usize, id: i32, value: f32) {
    if id == CAMERA_DISTANCE_ID && handle != 0 {
        record_answer(manager, handle);
    }
    unsafe { orig::<SetValueFn>(&ORIG_SET_VALUE)(manager, handle, id, value) }
}

fn read_triple(address: usize) -> Option<[f32; 3]> {
    Some([crate::read::<f32>(address)?, crate::read::<f32>(address + 4)?, crate::read::<f32>(address + 8)?])
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The shift from the map's local frame to the effect frame, kept between frames.
struct MapShift {
    current: Option<[f32; 3]>,
    last_feet: Option<[f32; 3]>,
    // A different shift the camera suggests, and since when.
    candidate: Option<([f32; 3], Instant)>,
}

impl MapShift {
    /// Takes this frame's suggestion (camera - feet rounded to tiles) and returns the shift to use.
    fn update(&mut self, suggested: [f32; 3], feet: [f32; 3]) -> [f32; 3] {
        let jumped = self.last_feet.is_some_and(|last| distance(last, feet) > MAP_LOAD_JUMP);
        self.last_feet = Some(feet);
        match self.current {
            Some(current) if current == suggested => {
                self.candidate = None;
            }
            Some(_) if !jumped => {
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
                // First value, or a load / teleport.
                self.current = Some(suggested);
                self.candidate = None;
            }
        }
        self.current.unwrap_or(suggested)
    }
}

fn player() -> Option<usize> {
    let world_chr_man = crate::read::<usize>(WORLD_CHR_MAN_ADDR).filter(|&p| p != 0)?;
    crate::read::<usize>(world_chr_man + PLAYER_INS_OFFSET).filter(|&p| p != 0)
}

/// A point CORE_HEIGHT above a character's feet, in the effect frame.
fn character_core(character: usize, map_shift: [f32; 3]) -> Option<[f32; 3]> {
    let feet = read_triple(character + PLAYER_FEET_OFFSET)?;
    Some([feet[0] + map_shift[0], feet[1] + map_shift[1] + CORE_HEIGHT, feet[2] + map_shift[2]])
}

/// The player's core, and the map shift (see the module docs).
fn player_core_and_shift(manager: usize) -> Option<([f32; 3], [f32; 3])> {
    let feet = read_triple(player()? + PLAYER_FEET_OFFSET)?;
    let camera = read_triple(manager + MANAGER_CAMERA_OFFSET)?;
    let suggested: [f32; 3] = std::array::from_fn(|i| ((camera[i] - feet[i]) / MAP_TILE).round() * MAP_TILE);
    let map_shift = MAP_SHIFT.lock().ok()?.update(suggested, feet);
    Some(([feet[0] + map_shift[0], feet[1] + map_shift[1] + CORE_HEIGHT, feet[2] + map_shift[2]], map_shift))
}

/// Every AC in the world other than the player, from WorldChrMan's character sets (see the module docs).
fn other_acs() -> Vec<usize> {
    let mut acs = Vec::new();
    let Some(world_chr_man) = crate::read::<usize>(WORLD_CHR_MAN_ADDR).filter(|&p| p != 0) else { return acs };
    let player = player();
    let set_count = crate::read::<i32>(world_chr_man + CHR_SET_COUNT_OFFSET).unwrap_or(0).max(0) as usize;
    let Some(sets) = crate::read::<usize>(world_chr_man + CHR_SETS_OFFSET) else { return acs };
    for set_index in 0..set_count {
        let Some(set) = crate::read::<usize>(sets + set_index * CHR_SET_STRIDE).filter(|&p| p != 0) else { continue };
        let slot_count = crate::read::<i32>(set + SET_SLOT_COUNT_OFFSET).unwrap_or(0).max(0) as usize;
        let Some(slots) = crate::read::<usize>(set + SET_SLOTS_OFFSET) else { continue };
        for slot in 0..slot_count {
            let Some(character) = crate::read::<usize>(slots + slot * SLOT_STRIDE).filter(|&p| p != 0) else { continue };
            let is_ac = crate::read::<usize>(character) == Some(AC_VTABLE);
            if is_ac && Some(character) != player {
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
    let other_acs = OTHER_ACS.lock().map(|acs| acs.clone()).unwrap_or_default();
    other_acs
        .into_iter()
        .filter_map(|ac| character_core(ac, map_shift))
        .all(|other_core| distance(flame, other_core) > to_player)
}

fn record_answer(manager: usize, handle: usize) {
    let Some(instance) = crate::read::<usize>(handle + 8).filter(|&p| p != 0) else { return };
    let Some(fxr_id) = crate::read::<i32>(instance) else { return };
    if !WRAPPER_IDS.contains(&fxr_id) && fxr_id != STARTUP_FLARE_WRAPPER {
        return;
    }
    let get_transform: GetTransformFn = unsafe { std::mem::transmute(GET_TRANSFORM_ADDR) };
    let mut matrix = [0f32; 12];
    unsafe { get_transform(handle, &mut matrix) };
    let flame = [matrix[3], matrix[7], matrix[11]];
    // A brand new effect can still be at the origin; wait for its real position.
    let placed = flame.iter().any(|v| v.abs() > 0.001);
    let Some((player_core, map_shift)) = player_core_and_shift(manager).filter(|_| placed) else { return };
    let answer = if is_players_flame(flame, player_core, map_shift) { PLAYERS } else { SOMEONE_ELSES };

    let Ok(mut answers) = ANSWERS.get_or_init(Default::default).lock() else { return };
    for offset in INSTANCE_BLOCK_OFFSETS {
        if let Some(block) = crate::read::<usize>(instance + offset).filter(|&b| b != 0) {
            answers.insert(block, (answer, Instant::now()));
        }
    }
    // Drop the dead effects' answers now and then, so the map stays small.
    if answers.len() > 4096 {
        answers.retain(|_, (_, measured)| measured.elapsed() < MAX_AGE);
    }
}

/// External value 900010 for an effect reading through `block`.
pub(crate) fn served_value(block: usize) -> f32 {
    let Some(Ok(answers)) = ANSWERS.get().map(|a| a.lock()) else { return NOT_KNOWN };
    match answers.get(&block) {
        Some(&(answer, measured)) if measured.elapsed() < MAX_AGE => answer,
        _ => NOT_KNOWN,
    }
}
