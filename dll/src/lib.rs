//! Adds a DEVICE-style color row to the Paint screen's per-part list (Armored Core VI 1.09.1), stored in a paint
//! section the menus never use.
//!
//! How the Paint rows work: 02_coloring.gfx's ColorAndMaterialList shows one Item_N_0 clip per entry of a row-data
//! vector the screen fills (clips past the vector's size are hidden). The six color rows come from looping over the
//! color slot list from 0x14045d300 (a static {0,1,2,3,4,5}): label = text 210111 + slot, the slot is stored as the
//! row's kind (+0x98; color set = 6, pattern = 7, weathering = 8), and slots 0x14045de80 calls "not normal" (5 and
//! 0xFF) get the DEVICE row type (color only). Confirming a row asks 0x140f40d10 for the slots its editor edits: kind
//! k -> {k}, color set -> all six, anything else -> none (the editor then opens with nothing to edit).
//!
//! Hooks:
//! - 0x14045d300 slot list: from the row builder (return 0x140f41cf5), the list gets an extra slot (EXTRA_SLOT). Other
//!   callers walk the same list for data, so only that call gets it.
//! - 0x14045de80 is-normal-slot: the extra slot (only ever in that injected list) is a DEVICE-type row.
//! - 0x140f40d10 kind -> edited slots: the extra slot's kind edits {EXTRA_SLOT}.
//! - 0x140e0d030 set color on the current part category: the extra slot's color goes to the storage slot below.
//! - 0x140e0c960 get color (out, slot, part category), called every frame for each swatch: the extra slot reads the
//!   storage slot.
//! - 0x14045afc0 paint color writer: drops writes to the extra slot, whatever path they came from (paint sections only
//!   have six colors; slot 9's address is the pattern bytes). Other slots pass through.
//! - 0x142b56480 FXR external value read: the flames side, in thrusters.rs.
//! - 0x141625930 an effect's per-frame camera distance: whether a flame is the player's, in player_flames.rs.
//!
//! Storage: the paint data (edit context +0x40) is 14 sections of 0x44 bytes, saved and carried by designs as one
//! 0x3b8 block. The part categories (0x14045d960) only ever write sections 0-5, 8 and 11-13, so the menus never touch
//! sections 6, 7, 9 and 10; the color lives in STORAGE_SECTION's color slot STORAGE_SLOT. One color per AC, whichever
//! part is selected.
//!
//! The menu needs a 7th color clip and the row's label text (build.py). Logs to Game\debug-logs\paint_menu.log.

mod player_flames;
mod thrusters;

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use retour::RawDetour;

// Return address of the Paint row builder's call to the slot list.
const ROW_BUILDER_SLOT_LIST_RET: usize = 0x140f41cf5;
// Copy-constructs the list's current row into a buffer and returns it; 0x140c82e00 destroys the copy. A row object is
// 0x308 bytes (the row vector's element stride; 0x140d407a0 -> copy ctor 0x140c81610, or a default ctor writing up to
// +0x300 for a bad index), so RowCopy's 0x320 has slack. The game's own caller (0x140f40d10) passes a 0x310 buffer.
const GET_ROW_ADDR: usize = 0x140d40830;
const DESTROY_ROW_ADDR: usize = 0x140c82e00;
const ROW_KIND_OFFSET: usize = 0x98;

// Not 0-8: 0-5 are the real color slots, 6-8 the color set / pattern / weathering row kinds. build.py's label ID
// (210111 + slot) depends on it.
const EXTRA_SLOT: u8 = 9;

static SLOTS: [u8; 7] = [0, 1, 2, 3, 4, 5, EXTRA_SLOT];
static EXTRA_SLOT_LIST: [u8; 1] = [EXTRA_SLOT];
// Where the extra row's color is kept in the paint data (colors are u32s of RGBA bytes).
const STORAGE_SECTION: u8 = 9;
const STORAGE_SLOT: u8 = 0;
static STORAGE_SECTIONS: [u8; 1] = [STORAGE_SECTION];
/// Offset of the stored color from the start of a paint block (section size 0x44, colors at +8).
pub(crate) const STORAGE_COLOR_OFFSET: usize = STORAGE_SECTION as usize * 0x44 + 8 + STORAGE_SLOT as usize * 4;
const CTX_PAINT_OFFSET: usize = 0x40;

static ORIG_SLOT_LIST: AtomicUsize = AtomicUsize::new(0);
static ORIG_IS_NORMAL_SLOT: AtomicUsize = AtomicUsize::new(0);
static ORIG_EDITED_SLOTS: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_COLOR: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_COLOR: AtomicUsize = AtomicUsize::new(0);
static ORIG_WRITE_COLORS: AtomicUsize = AtomicUsize::new(0);
static LOG: OnceLock<Mutex<File>> = OnceLock::new();

pub(crate) fn log(msg: &str) {
    if let Some(mut f) = LOG.get().and_then(|l| l.lock().ok()) {
        let _ = writeln!(f, "{msg}");
    }
}

unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn ReadProcessMemory(process: isize, address: usize, buffer: *mut u8, size: usize, read: *mut usize) -> i32;
}

/// Reads memory that may not be mapped (pointer chains while loading, effects being freed) without faulting.
pub(crate) fn read<T: Copy + Default>(addr: usize) -> Option<T> {
    let mut v = T::default();
    let mut n = 0;
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr, &mut v as *mut T as *mut u8, size_of::<T>(), &mut n) };
    (ok != 0 && n == size_of::<T>()).then_some(v)
}

/// A hooked function's original (its trampoline), typed as `F` (an `unsafe extern "system" fn`).
pub(crate) unsafe fn orig<F: Copy>(trampoline: &AtomicUsize) -> F {
    unsafe { std::mem::transmute_copy(&trampoline.load(Ordering::Relaxed)) }
}

type SlotRange = [*const u8; 2];
type WriteColorsFn = unsafe extern "system" fn(usize, *const SlotRange, u8, u32);

fn range(slots: &'static [u8]) -> SlotRange {
    let r = slots.as_ptr_range();
    [r.start, r.end]
}

// Passes the return address in rdx (the slot list takes one argument) and tail-jumps to the impl.
#[unsafe(naked)]
unsafe extern "system" fn slot_list_entry() {
    core::arch::naked_asm!("mov rdx, [rsp]", "jmp {}", sym slot_list_impl);
}

unsafe extern "system" fn slot_list_impl(out: *mut SlotRange, caller: usize) -> *mut SlotRange {
    let result = unsafe { orig::<unsafe extern "system" fn(*mut SlotRange) -> *mut SlotRange>(&ORIG_SLOT_LIST)(out) };
    if caller == ROW_BUILDER_SLOT_LIST_RET {
        unsafe { *out = range(&SLOTS) };
    }
    result
}

unsafe extern "system" fn is_normal_slot_impl(slot: *const u8) -> bool {
    unsafe { *slot != EXTRA_SLOT && orig::<unsafe extern "system" fn(*const u8) -> bool>(&ORIG_IS_NORMAL_SLOT)(slot) }
}

#[repr(C, align(16))]
struct RowCopy([u8; 0x320]);

/// The kind of the list's current row, read the way 0x140f40d10 does it.
fn current_row_kind(list: usize) -> i32 {
    let get_row: unsafe extern "system" fn(usize, *mut RowCopy) -> *const u8 = unsafe { std::mem::transmute(GET_ROW_ADDR) };
    let destroy_row: unsafe extern "system" fn(*mut RowCopy) = unsafe { std::mem::transmute(DESTROY_ROW_ADDR) };
    let mut copy = RowCopy([0; 0x320]);
    unsafe {
        let row = get_row(list, &mut copy);
        let kind = *(row.add(ROW_KIND_OFFSET) as *const i32);
        destroy_row(&mut copy);
        kind
    }
}

unsafe extern "system" fn edited_slots_impl(list: *const usize, out: *mut SlotRange) -> *mut SlotRange {
    type EditedSlotsFn = unsafe extern "system" fn(*const usize, *mut SlotRange) -> *mut SlotRange;
    let result = unsafe { orig::<EditedSlotsFn>(&ORIG_EDITED_SLOTS)(list, out) };
    // Unknown kinds (pattern, weathering, ours) get an empty range; only then check whether it's ours.
    let [start, end] = unsafe { *out };
    let no_slots = start == end;
    if no_slots && current_row_kind(unsafe { *list }) == EXTRA_SLOT as i32 {
        unsafe { *out = range(&EXTRA_SLOT_LIST) };
    }
    result
}

fn ctx_paint(ctx: usize) -> usize {
    unsafe { *((ctx + CTX_PAINT_OFFSET) as *const usize) }
}

unsafe extern "system" fn set_color_impl(ctx: usize, slot: u8, color: u32) {
    if slot != EXTRA_SLOT {
        return unsafe { orig::<unsafe extern "system" fn(usize, u8, u32)>(&ORIG_SET_COLOR)(ctx, slot, color) };
    }
    // The game's own writer (its trampoline, so past our guard), aimed at the storage section only.
    let paint = ctx_paint(ctx);
    unsafe { orig::<WriteColorsFn>(&ORIG_WRITE_COLORS)(paint, &range(&STORAGE_SECTIONS), STORAGE_SLOT, color) };
}

unsafe extern "system" fn get_color_impl(ctx: usize, out: *mut u32, slot: u8, category: u8) -> *mut u32 {
    if slot != EXTRA_SLOT {
        type GetColorFn = unsafe extern "system" fn(usize, *mut u32, u8, u8) -> *mut u32;
        return unsafe { orig::<GetColorFn>(&ORIG_GET_COLOR)(ctx, out, slot, category) };
    }
    unsafe { *out = *((ctx_paint(ctx) + STORAGE_COLOR_OFFSET) as *const u32) };
    out
}

unsafe extern "system" fn write_colors_impl(paint: usize, parts: *const SlotRange, slot: u8, color: u32) {
    // Only ever ours: vanilla writes (whatever their slot) pass through untouched.
    if slot == EXTRA_SLOT {
        log(&format!("blocked a paint color write to the extra slot (color {color:08x})"));
        return;
    }
    unsafe { orig::<WriteColorsFn>(&ORIG_WRITE_COLORS)(paint, parts, slot, color) }
}

/// A hook: target address, its first bytes (checked before hooking), detour, trampoline slot.
pub(crate) type Hook = (usize, [u8; 6], *const (), &'static AtomicUsize);

fn install() -> Result<(), String> {
    let hooks: [Hook; 8] = [
        // push rbx / sub rsp,30
        (0x14045d300, [0x40, 0x53, 0x48, 0x83, 0xec, 0x30], slot_list_entry as *const (), &ORIG_SLOT_LIST),
        // movzx eax,byte [rcx] / cmp al,5
        (0x14045de80, [0x0f, 0xb6, 0x01, 0x3c, 0x05, 0x74], is_normal_slot_impl as *const (), &ORIG_IS_NORMAL_SLOT),
        // push rdi / sub rsp,350
        (0x140f40d10, [0x40, 0x57, 0x48, 0x81, 0xec, 0x50], edited_slots_impl as *const (), &ORIG_EDITED_SLOTS),
        // mov [rsp+18],r8d
        (0x140e0d030, [0x44, 0x89, 0x44, 0x24, 0x18, 0x88], set_color_impl as *const (), &ORIG_SET_COLOR),
        // mov [rsp+8],rbx / mov [rsp+18],r8b
        (0x140e0c960, [0x48, 0x89, 0x5c, 0x24, 0x08, 0x44], get_color_impl as *const (), &ORIG_GET_COLOR),
        // mov [rsp+8],rbx
        (0x14045afc0, [0x48, 0x89, 0x5c, 0x24, 0x08, 0x48], write_colors_impl as *const (), &ORIG_WRITE_COLORS),
        thrusters::hook(),
        player_flames::hook(),
    ];
    for &(addr, bytes, _, _) in &hooks {
        let actual = unsafe { std::slice::from_raw_parts(addr as *const u8, bytes.len()) };
        // A jmp means another DLL hooked it first; the detours chain fine.
        if actual != bytes && actual[0] != 0xe9 {
            return Err(format!("prologue at {addr:#x} doesn't match (not 1.09.1?)"));
        }
    }
    let mut detours = Vec::new();
    for (target, _, detour, trampoline) in hooks {
        let d = unsafe { RawDetour::new(target as *const (), detour) }.map_err(|e| e.to_string())?;
        trampoline.store(d.trampoline() as *const () as usize, Ordering::Relaxed);
        detours.push(d);
    }
    for d in &detours {
        unsafe { d.enable() }.map_err(|e| e.to_string())?;
    }
    // Hooks stay installed for the life of the process.
    std::mem::forget(detours);
    Ok(())
}

fn init() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("debug-logs")))
        .unwrap_or_else(|| "debug-logs".into());
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(f) = File::create(dir.join("paint_menu.log")) {
        let _ = LOG.set(Mutex::new(f));
    }
    match install() {
        Ok(()) => {
            log("hooks installed");
            thrusters::start();
            player_flames::start();
        }
        Err(e) => log(&format!("not hooked: {e}")),
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(_module: *mut u8, reason: u32, _reserved: *mut u8) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        init();
    }
    1
}
