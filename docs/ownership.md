# Whose flame is it? Effect ownership in AC6 (1.09.1)

paint-menu colors only the player's thruster flames, but enemy ACs use the same flame effects. This page explains how
the DLL tells them apart exactly, and how far the same chain goes for effects in general. Addresses are for the
1.09.1 exe.

## The chain

Nothing points from a running effect back to the character that spawned it: not the effect instance, not its node
in the effect manager. The link goes the other way. Every character drives its long-lived effects through **slots**,
small controller objects in a per-character list, and each SFX-carrying slot holds the handles of the effects it
spawned.

```
ChrIns (a character)
  + 0x40f0                  its slot system (CS::ChrSlotSys)
  + 0x4100                  first slot; slots form a singly linked list, next at slot + 0x10
slot                        CS::ChrSlotBase subclass; the class (vtable) says what it does
  + 0x60 / + 0x70           an SFX slot's SFX object (thruster slot / ChrMultiSfxSlot)
SFX object                  CS::CSFD4MultiSfxIns, vtable 0x143b0fd90
  [+ 0x88, + 0x90)          its effect handles, 0x2E0 bytes each
handle + 0x08               the effect instance: the running FXR (FXR ID at + 0)
```

So an effect belongs to the character whose slot list holds a slot whose SFX object holds a handle to it.

One slot spawns one effect (one FXR ID). `ChrMultiSfxSlot` spawns it once on every dummy poly with the slot's
dummy poly ID, and its SFX object holds one handle per copy: 2 for a pair of boost flames, up to 6 for some quick boost and Laser Lance effects.
A live capture (60 s of boosting, quick boosts and Laser Lance) never found an SFX object holding two different FXRs.
The thruster slots held one flame each, so a left/right pair there is two slots.

The class names come from Elden Ring, which runs on the same engine and still has its RTTI (class names), unlike
AC6. Each AC6 class was matched to ER's by comparing functions: byte patterns, field offsets, vtable layout and who
constructs what.

| AC6 vtable | Class | SFX object | Carries |
|---|---|---|---|
| `0x1438300f0` | `ChrAcThrusterSfxSlot`: **our name**, AC6-only (no ER equivalent; only ACs have it) | `+0x60` | quick boost, assault boost, boost start/stop flames; feeds them external values 40000-40002 |
| `0x14382e2b8` | `CS::ChrMultiSfxSlot` | `+0x70` | continuous boost flames, the assault boost startup flare, melee weapon effects, and NPCs' thrusters |
| `0x14382ead0` | `CS::ChrSingleSeSlot` | none | a character sound (Wwise event), not an effect |

Characters have other slot kinds too (in ER: footstep effects, bullet emitters, burning...); only these two SFX
slot classes carried effects in our tests.

## How paint-menu uses it

Each character updates its slots once per frame. The slot update is a virtual call, `update(slot, dt, ChrIns**
owner)`, and the owner is right there as an argument:

- `0x140937cc0`: the thruster slot's update;
- `0x14092bcd0`: `ChrMultiSfxSlot`'s update.

The DLL hooks both (`dll/src/player_flames.rs`). After the original update, it walks that slot's handles and, for
each paint-menu flame, attaches external value 900010 (2 = the player's, 1 = someone else's). It uses the same setter
the game uses for the camera distance value: `0x141625930`, `set_value(manager, handle, id, value)`, which only uses
the handle. Each flame effect waits for 900010 before choosing between the stock and the colored version.

Things to know:

- **Don't walk slot lists from inside a slot update.** The container update (`0x1407464e0`) detaches the whole list
  while it updates the slots and rebuilds it afterwards, so mid-update the list is empty or partial. From another
  thread, between updates, it's normally intact. paint-menu doesn't walk lists at all: the hook already has the slot.
- **A new effect shows up in its slot's handles one frame after the update that spawned it**, so it gets tagged on
  the next update (7 ms median in our tests). The flame effects wait up to 0.3 s for an answer, so that's fine.
- **Effects spawned outside the slots get no answer.** In the arena intro cutscene, one pair of flames (9412021) is
  spawned by the cutscene, not by the AC's slots, and plays stock. Every other intro flame went through the slots.

## Checking it

- **In code**: the chain was read in Ghidra (who calls the container update and with what, what each slot update
  does with its SFX object, how the SFX object attaches values to its handles) and matched against Elden Ring's
  named classes.
- **At runtime**: a research DLL recorded which owner each thruster slot was updated with and checked that the slot
  sat in that owner's list: 3,233 times yes, 0 times in another character's list. For thruster flames, the owner
  found through the chain agreed with "the AC nearest to the flame" on 628 of 631 flames, in a close-range AC fight.
- **Against the old rule**: paint-menu used to decide by position (the flame is the player's if it's near the player
  and nearer to them than to any other AC). Both rules ran side by side over arena fights: they agreed on every
  flame except a few at the arena intro, which is where the position rule was known to fail.

## Which effects have an owner

The same chain, applied to every effect over an arena fight, AC Test alone (one action at a time) and a full mission
(Watchpoint: MTs, helicopters, an enemy AC, the boss):

| Has an owner (goes through a slot) | No owner |
|---|---|
| thruster flames and generator exhaust, for ACs and NPCs alike | bullets and missiles: projectiles are separate objects |
| melee weapon effects: Laser Lance, pile bunker | hit effects (from the damage system) |
| pulse armor's effects on the AC (some of them) | map and asset effects |
| footsteps (some) | |

So effects a character carries over time go through its slots; effects that live on their own don't. In the
mission, non-AC enemies (including the boss) went through `ChrMultiSfxSlot` too, and never used the player
generator flames that paint-menu replaces.

**Ranged weapons don't follow this chain**: their bullets, missiles and impacts have no owner through the slots.
Bullets presumably keep their owner somewhere (for damage), but that's a different structure we haven't looked at.
