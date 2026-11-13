//! Tells the flame wrappers (fxr/build_fxrs.mjs) whether they're on the player's AC: external value 900010 reads
//! 0 = not known yet (or no DLL), 1 = someone else's, 2 = the player's. A wrapper decides once, at spawn.
//!
//! The answer comes from the slot that drives the flame (AC6_tools\Research\thruster-ownership). Every character
//! updates its slots each frame as `update(slot, dt, ChrIns** owner)`; the flame-carrying ones keep their effects in a
//! CSFD4MultiSfxIns. After the original update, the hooks attach 900010 to the slot's wrapper flames: PLAYERS when the
//! owner is the player's ChrIns, SOMEONE_ELSES otherwise.
//!
//! - The two slot classes that carry the flames, with their update and where they keep their SFX object:
//!   ChrAcThrusterSfxSlot (our name; vtable 0x1438300f0): update 0x140937cc0, SFX object at + 0x60;
//!   CS::ChrMultiSfxSlot (vtable 0x14382e2b8; continuous flames, the startup flare): update 0x14092bcd0, + 0x70.
//! - SFX object (vtable 0x143b0fd90): effect handles from [+ 0x88] to [+ 0x90], 0x2E0 bytes each; handle + 8 = the
//!   effect instance (FXR ID at + 0).
//! - 0x141625930 `set_value(manager, handle, int id, float value)` packs one value into a block and attaches it to the
//!   effect (the effect manager uses it for 2000, the camera distance); it only uses the handle. The game then keeps
//!   900010 on the effect and serves it to the wrapper (an effect only takes the values its FXR header lists).
//! - The player's ChrIns: [[WorldChrMan 0x144e0f598] + 0xA5A0].
//!
//! A new flame shows up in its slot's handles a frame after the update that spawned it, so it's tagged on the next
//! update; the wrapper waits for a nonzero 900010 (up to 0.3 s, then plays stock). Flames spawned outside the slots
//! aren't tagged and play stock: one pair in the arena intro cutscene (9412021).

use std::sync::atomic::AtomicUsize;

use crate::{Hook, orig};

const EXT_PLAYERS_FLAME: i32 = 900010;
const SOMEONE_ELSES: f32 = 1.0;
const PLAYERS: f32 = 2.0;

// The wrappers are stock flame + WRAPPER_OFFSET (fxr/build_fxrs.mjs); the stock flames are the thruster flames
// (41xxxx-45xxxx) and the assault boost startup flare.
const WRAPPER_OFFSET: i32 = 9_000_000;
const STOCK_THRUSTER_FLAMES: std::ops::Range<i32> = 400_000..500_000;
const STOCK_STARTUP_FLARE: i32 = 1_003_010;

const SET_VALUE_ADDR: usize = 0x141625930;
const WORLD_CHR_MAN_ADDR: usize = 0x144e0f598;
const PLAYER_INS_OFFSET: usize = 0xA5A0;
const THRUSTER_SLOT_SFX_OFFSET: usize = 0x60;
const MULTI_SFX_SLOT_SFX_OFFSET: usize = 0x70;
const MULTI_SFX_INS_VTABLE: usize = 0x143b0fd90;
const HANDLES_BEGIN_OFFSET: usize = 0x88;
const HANDLES_END_OFFSET: usize = 0x90;
const HANDLE_SIZE: usize = 0x2e0;
// More handles than this means the vector isn't what we think it is.
const MAX_HANDLES: usize = 64;

static ORIG_THRUSTER_UPDATE: AtomicUsize = AtomicUsize::new(0);
static ORIG_MULTI_UPDATE: AtomicUsize = AtomicUsize::new(0);

type SlotUpdateFn = unsafe extern "system" fn(usize, f32, *const usize) -> u64;
type SetValueFn = unsafe extern "system" fn(usize, usize, i32, f32);

/// The two slot update hooks, for lib.rs's hook table.
pub(crate) fn hooks() -> [Hook; 2] {
    [
        // mov rax,rsp / push rbp / push rdi / push r13
        (0x140937cc0, [0x48, 0x8b, 0xc4, 0x55, 0x57, 0x41], thruster_update_impl as *const (), &ORIG_THRUSTER_UPDATE),
        // push rbp / push rsi / push rdi / push r12
        (0x14092bcd0, [0x40, 0x55, 0x56, 0x57, 0x41, 0x54], multi_update_impl as *const (), &ORIG_MULTI_UPDATE),
    ]
}

unsafe extern "system" fn thruster_update_impl(slot: usize, dt: f32, owner: *const usize) -> u64 {
    let result = unsafe { orig::<SlotUpdateFn>(&ORIG_THRUSTER_UPDATE)(slot, dt, owner) };
    unsafe { tag_flames(slot + THRUSTER_SLOT_SFX_OFFSET, owner) };
    result
}

unsafe extern "system" fn multi_update_impl(slot: usize, dt: f32, owner: *const usize) -> u64 {
    let result = unsafe { orig::<SlotUpdateFn>(&ORIG_MULTI_UPDATE)(slot, dt, owner) };
    unsafe { tag_flames(slot + MULTI_SFX_SLOT_SFX_OFFSET, owner) };
    result
}

fn is_wrapper(fxr_id: i32) -> bool {
    let stock = fxr_id - WRAPPER_OFFSET;
    STOCK_THRUSTER_FLAMES.contains(&stock) || stock == STOCK_STARTUP_FLARE
}

/// The player's ChrIns (none while loading).
fn player() -> Option<usize> {
    let world_chr_man = crate::read::<usize>(WORLD_CHR_MAN_ADDR).filter(|&p| p != 0)?;
    crate::read::<usize>(world_chr_man + PLAYER_INS_OFFSET).filter(|&p| p != 0)
}

/// Attaches 900010 to the wrapper flames of the SFX object at `[sfx_field]`, for the slot's `owner`.
///
/// Plain reads: the slot is being updated (alive), its SFX object belongs to it, and the handles are the ones the
/// original update just attached the slot's own values through.
unsafe fn tag_flames(sfx_field: usize, owner: *const usize) {
    if owner.is_null() {
        return;
    }
    let owner = unsafe { *owner };
    let sfx = unsafe { *(sfx_field as *const usize) };
    if owner == 0 || sfx == 0 || unsafe { *(sfx as *const usize) } != MULTI_SFX_INS_VTABLE {
        return;
    }
    let begin = unsafe { *((sfx + HANDLES_BEGIN_OFFSET) as *const usize) };
    let end = unsafe { *((sfx + HANDLES_END_OFFSET) as *const usize) };
    if end <= begin || end - begin > HANDLE_SIZE * MAX_HANDLES {
        return;
    }
    let set_value: SetValueFn = unsafe { std::mem::transmute(SET_VALUE_ADDR) };
    // Only read the player once a wrapper turns up (most slots carry none).
    let mut answer = None;
    for handle in (begin..end).step_by(HANDLE_SIZE) {
        let instance = unsafe { *((handle + 8) as *const usize) };
        if instance == 0 || !is_wrapper(unsafe { *(instance as *const i32) }) {
            continue;
        }
        let answer = *answer.get_or_insert_with(|| if Some(owner) == player() { PLAYERS } else { SOMEONE_ELSES });
        unsafe { set_value(0, handle, EXT_PLAYERS_FLAME, answer) };
    }
}
