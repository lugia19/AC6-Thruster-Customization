"""Builds the mod from an installed copy of Armored Core VI 1.09.1.

    python build.py --game "<...>/ARMORED CORE VI FIRES OF RUBICON/Game" --witchy "<...>/WitchyBND.exe"

Needs node/npm, cargo (stable) and WitchyBND 3.x. Writes package/: the mod folder (regulation, custom flames, Paint
menu layout, the row's label in every language), the DLL and the me3 profile.
"""
import argparse
import atexit
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(ROOT, "build")
PACKAGE = os.path.join(ROOT, "package")

# thrustersColorType N uses ThrustersSfxIdParam_PC column sfxIdN. Stock types 0-2 (orange, blue, Coral) get custom
# types 3-5 (unused in vanilla), whose flames are wrappers that play the stock flame or custom copies tinted with the
# player's color, depending on how close to the camera they spawn (see fxr/build_fxrs.mjs).
STOCK_COLOR_TYPES = (0, 1, 2)
CUSTOM_TYPE_SHIFT = 3
# Player generators are 650xxxxx; the NPC versions (659xxxxx) are only used by special ACs and stay untouched.
NPC_GENERATOR_IDS_START = 65900000

# Paint screen layout (02_coloring.gfx, sprite 268 = ColorAndMaterialList): a 7th color row (symbol 244, like the
# other colors) below DEVICE, with pattern and weathering moved down a row and renamed so the rows stay in data order.
COLOR_LIST_SPRITE = 268
COLOR_LIST_EDITS = [
    "move:Item_8_0:Item_9_0:2760:15500",
    "move:Item_7_0:Item_8_0:2760:13440",
    "add:244:12:Item_7_0:2760:12000",
]

# The row's label: color rows use text 210111 + slot, in menu.msgbnd.dcx's menu text FMG. English in every language for
# now.
COLOR_LABEL_BASE = 210111
EXTRA_SLOT = 9  # dll/src/lib.rs EXTRA_SLOT
LABEL_FMG = "FNR_メニューテキスト.fmg"
LABEL_ID = COLOR_LABEL_BASE + EXTRA_SLOT
LABEL_AFTER_ID = 210116  # DEVICE
LABEL_TEXT = "THRUSTER"

FIELD_SIZES = {'s8': 1, 'u8': 1, 'dummy8': 1, 's16': 2, 'u16': 2, 's32': 4, 'u32': 4, 'f32': 4, 'angle32': 4,
               'b32': 4, 'f64': 8, 'fixstr': 1, 'fixstrW': 2}
FIELD_FORMATS = {'s8': '<b', 's32': '<i'}


class Param:
    """Raw .param file, fields located through WitchyBND's Paramdex defs (or `layout`, a list of (type, name) for
    params without one), edited in place."""

    def __init__(self, path, def_path=None, layout=None):
        self.path = path
        self.data = bytearray(open(path, "rb").read())

        # Field definitions like "s32 sfxId0", "u8 flag:1" (a bitfield) or "dummy8 pad[4]" (an array).
        if layout:
            definitions = [f"{typ} {name}" for typ, name in layout]
        else:
            definitions = [field.get("Def") for field in ET.parse(def_path).getroot().iter("Field")]

        # Lay the fields out in order. Consecutive bitfields of the same type share one storage unit until it's full.
        self.fields = {}  # name -> (offset in the row, type)
        offset = 0
        bitfield_unit_size = None  # size of the storage unit the current run of bitfields is filling
        bits_used = 0
        for definition in definitions:
            typ, name, bits, count = re.match(r"(\w+)\s+(\w+)(?::(\d+))?(?:\[(\d+)\])?", definition).groups()
            size = FIELD_SIZES[typ]
            if bits:
                fits_in_current_unit = bitfield_unit_size == size and bits_used + int(bits) <= size * 8
                if fits_in_current_unit:
                    bits_used += int(bits)
                else:
                    bitfield_unit_size = size
                    bits_used = int(bits)
                    offset += size
                continue
            bitfield_unit_size = None
            self.fields[name] = (offset, typ)
            offset += size * (int(count) if count else 1)
        row_size = offset

        # The row table: row_count entries of 0x18 bytes from 0x40, each an s32 row ID, 4 bytes of padding and the u64
        # offset of the row's data.
        row_count = struct.unpack_from("<H", self.data, 0xA)[0]
        self.rows = {}  # row ID -> data offset
        for i in range(row_count):
            row_id, data_offset = struct.unpack_from("<i4xQ", self.data, 0x40 + i * 0x18)
            self.rows[row_id] = data_offset

        # AC6 paramdefs can be stale: a def whose size doesn't match the rows would put fields at wrong offsets.
        starts = sorted(self.rows.values())
        strides = {b - a for a, b in zip(starts, starts[1:])}
        assert strides <= {row_size}, f"{os.path.basename(path)}: the layout has {row_size:#x}-byte rows, the param {strides}"

    def get(self, row, field):
        offset, typ = self.fields[field]
        return struct.unpack_from(FIELD_FORMATS[typ], self.data, self.rows[row] + offset)[0]

    def set(self, row, field, value):
        offset, typ = self.fields[field]
        struct.pack_into(FIELD_FORMATS[typ], self.data, self.rows[row] + offset, value)

    def save(self):
        open(self.path, "wb").write(self.data)


def unpacked(path):
    # WitchyBND's folder naming, next to the file: regulation.bin -> regulation-bin, x.ffxbnd.dcx -> x-ffxbnd-dcx-wffxbnd
    name = os.path.basename(path)
    folder = name.replace(".", "-") + ("-wffxbnd" if name.endswith(".ffxbnd.dcx") else "")
    return os.path.join(os.path.dirname(path), folder)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--game", required=True, help="the game's Game folder (with regulation.bin, sfx/ and menu/)")
    parser.add_argument("--witchy", required=True, help="WitchyBND.exe (3.x)")
    args = parser.parse_args()
    defs = os.path.join(os.path.dirname(args.witchy), "Assets", "Paramdex", "AC6", "Defs")

    # The DLL doesn't depend on anything below: build it meanwhile.
    cargo = subprocess.Popen(["cargo", "+stable", "build", "--release"], cwd=os.path.join(ROOT, "dll"))
    # If a later step fails, don't leave it compiling (and holding the target dir) after the build exits.
    atexit.register(lambda: cargo.poll() is None and cargo.kill())

    def witchy(inputs, outputs):
        """One WitchyBND run over all inputs (each launch is slow). It exits 0 even when it does nothing, so check
        that every expected output was (re)written."""
        before = {out: os.path.getmtime(out) if os.path.exists(out) else None for out in outputs}
        subprocess.run([args.witchy, "-s", *inputs], check=True)
        stale = [out for out in outputs if not os.path.exists(out) or os.path.getmtime(out) == before[out]]
        assert not stale, f"WitchyBND didn't produce {stale}"

    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    regulation = shutil.copy(os.path.join(args.game, "regulation.bin"), WORK)
    effects = shutil.copy(os.path.join(args.game, "sfx", "sfxbnd_commoneffects.ffxbnd.dcx"), WORK)
    msg_root = os.path.join(args.game, "msg")
    menu_texts = {}  # language -> copy of its menu.msgbnd.dcx
    for lang in sorted(os.listdir(msg_root)):
        source = os.path.join(msg_root, lang, "menu.msgbnd.dcx")
        if os.path.isfile(source):
            os.makedirs(os.path.join(WORK, "msg", lang))
            menu_texts[lang] = shutil.copy(source, os.path.join(WORK, "msg", lang))
    assert menu_texts, "no msg/*/menu.msgbnd.dcx found"
    archives = [regulation, effects, *menu_texts.values()]
    witchy(archives, [unpacked(a) for a in archives])

    params = unpacked(regulation)
    # The generator-dependent effect tables: row = an effect the game spawns, column t (sfxId<t>) = what it uses for
    # generator color type t, -1 = the row's own effect (e.g. row 1003010, the assault boost startup flare, has no
    # columns: one flare for every type). Thrusters: the flames, which get custom wrappers. Replace (no paramdef, 8 s32
    # columns): everything else that depends on the generator (assault armor, pulse armor...), which stays stock.
    thrusters = Param(os.path.join(params, "ThrustersSfxIdParam_PC.param"), os.path.join(defs, "ThrustersSfxIdParam.xml"))
    replace = Param(os.path.join(params, "GeneratorTypeSfxIdRplace_PC.param"),
                    layout=[("s32", f"sfxId{t}") for t in range(8)])

    def effect_for(table, row, t):
        sfx = table.get(row, f"sfxId{t}")
        return sfx if sfx != -1 or row == 0 else row

    effect_dir = os.path.join(unpacked(effects), "effect")
    stock_flames = sorted({effect_for(thrusters, row, t) for row in thrusters.rows for t in STOCK_COLOR_TYPES} - {-1})

    # Custom flames: a wrapper per stock flame, plus its two custom copies (see fxr/build_fxrs.mjs).
    fxr_dir = os.path.join(ROOT, "fxr")
    shell = os.name == "nt"
    if not os.path.isdir(os.path.join(fxr_dir, "node_modules")):
        subprocess.run(["npm", "install"], cwd=fxr_dir, check=True, shell=shell)
    id_map_path = os.path.join(WORK, "id_map.json")
    subprocess.run(["node", "build_fxrs.mjs", effect_dir, id_map_path, *map(str, stock_flames)], cwd=fxr_dir,
                   check=True, shell=shell)
    id_map = json.load(open(id_map_path))
    wrapper_of = {int(stock): wrapper for stock, wrapper in id_map["wrappers"].items()}
    # The copies use their sources' textures and models. WitchyBND would generate empty resource lists for them (fine
    # for the wrappers, which only hold proxies).
    reslists = os.path.join(unpacked(effects), "ResourceList")
    for new_id, source_id in id_map["resources"].items():
        target = os.path.join(reslists, f"f{int(new_id):09d}.ffxreslist")
        assert not os.path.exists(target), f"{new_id} collides with an existing effect"
        shutil.copy(os.path.join(reslists, f"f{source_id:09d}.ffxreslist"), target)

    # Params: each custom type t+3 uses what stock type t uses, as its wrapper in the thruster table and unchanged in
    # the replace table (or a Coral generator would fall back to the non-Coral versions), and every player generator
    # with a stock color type switches to its custom type.
    for table, mapping in [(thrusters, wrapper_of), (replace, {})]:
        for row in table.rows:
            for t in STOCK_COLOR_TYPES:
                column = f"sfxId{t + CUSTOM_TYPE_SHIFT}"
                assert table.get(row, column) == -1, f"{os.path.basename(table.path)} row {row}: {column} in use"
                stock = effect_for(table, row, t)
                if stock != -1:
                    table.set(row, column, mapping.get(stock, stock))
        table.save()
    generators = Param(os.path.join(params, "EquipParamGenerator.param"), os.path.join(defs, "EquipParamGenerator.xml"))
    switched = 0
    for row in generators.rows:
        color_type = generators.get(row, "thrustersColorType")
        if row < NPC_GENERATOR_IDS_START and color_type in STOCK_COLOR_TYPES:
            generators.set(row, "thrustersColorType", color_type + CUSTOM_TYPE_SHIFT)
            switched += 1
    assert switched, "no generator had a stock thruster color type"
    generators.save()
    print(f"{switched} player generators switched to their custom flames")

    # The row's label, in every language's menu text (FMG -> XML, insert after DEVICE, XML -> FMG).
    label_fmgs = {}
    for lang, archive in menu_texts.items():
        fmg = os.path.join(unpacked(archive), LABEL_FMG)
        if os.path.isfile(fmg):
            label_fmgs[lang] = fmg
        else:  # some languages (araae) keep their menu text elsewhere
            # (no file name in the message: Japanese breaks redirected output on cp1252 consoles)
            print(f"{lang}: menu.msgbnd.dcx has no menu text FMG, label left out")
    assert label_fmgs, "no language's menu.msgbnd.dcx has the menu text FMG"
    witchy(label_fmgs.values(), [fmg + ".xml" for fmg in label_fmgs.values()])
    for lang, fmg in label_fmgs.items():
        xml_path = fmg + ".xml"
        text = open(xml_path, encoding="utf-8-sig").read()
        anchor = re.search(rf'<text id="{LABEL_AFTER_ID}">.*?</text>\r?\n', text)
        assert anchor and f'id="{LABEL_ID}"' not in text, f"unexpected menu text in {lang}"
        text = text[:anchor.end()] + f'<text id="{LABEL_ID}">{LABEL_TEXT}</text>\n' + text[anchor.end():]
        open(xml_path, "w", encoding="utf-8-sig").write(text)
    witchy([fmg + ".xml" for fmg in label_fmgs.values()], label_fmgs.values())
    for fmg in label_fmgs.values():
        os.remove(fmg + ".xml")  # or it would be packed into the archive too

    label_archives = [menu_texts[lang] for lang in label_fmgs]
    repacked = [regulation, effects, *label_archives]
    witchy([unpacked(a) for a in repacked], repacked)

    # Paint screen layout.
    menu_out = os.path.join(WORK, "02_coloring.gfx")
    subprocess.run([sys.executable, os.path.join(ROOT, "tools", "gfx_place.py"), os.path.join(args.game, "menu", "02_coloring.gfx"),
                    menu_out, str(COLOR_LIST_SPRITE), *COLOR_LIST_EDITS], check=True)

    assert cargo.wait() == 0, "cargo build failed"

    shutil.rmtree(PACKAGE, ignore_errors=True)
    for sub in ("sfx", "menu"):
        os.makedirs(os.path.join(PACKAGE, "mod", sub))
    shutil.copy(regulation, os.path.join(PACKAGE, "mod"))
    shutil.copy(effects, os.path.join(PACKAGE, "mod", "sfx"))
    shutil.copy(menu_out, os.path.join(PACKAGE, "mod", "menu"))
    for lang in label_fmgs:
        os.makedirs(os.path.join(PACKAGE, "mod", "msg", lang))
        shutil.copy(menu_texts[lang], os.path.join(PACKAGE, "mod", "msg", lang))
    shutil.copy(os.path.join(ROOT, "dll", "target", "release", "paint_menu.dll"), PACKAGE)
    shutil.copy(os.path.join(ROOT, "paint-menu.me3"), PACKAGE)
    print(f"Built {PACKAGE}")


if __name__ == "__main__":
    main()
