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

# thrustersColorType N uses ThrustersSfxIdParam_PC column sfxIdN. Stock types 0-2 (blue, orange, Coral) get custom
# twins 3-5 (unused in vanilla): copies of their flames that switch between the stock and the tinted look by camera
# distance (see fxr/build_fxrs.mjs), so the player's AC, close to the camera, shows the color.
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
    """Raw .param file, fields located through WitchyBND's Paramdex defs, edited in place."""

    def __init__(self, path, def_path):
        self.path = path
        self.data = bytearray(open(path, "rb").read())
        self.fields, offset, bit_size, bits_used = {}, 0, None, 0
        for field in ET.parse(def_path).getroot().iter("Field"):
            typ, name, bits, count = re.match(r"(\w+)\s+(\w+)(?::(\d+))?(?:\[(\d+)\])?", field.get("Def")).groups()
            size = FIELD_SIZES[typ]
            if bits:  # bitfields share a storage unit until it's full
                if bit_size == size and bits_used + int(bits) <= size * 8:
                    bits_used += int(bits)
                    continue
                bit_size, bits_used = size, int(bits)
                offset += size
                continue
            bit_size = None
            self.fields[name] = (offset, typ)
            offset += size * (int(count) if count else 1)
        row_count = struct.unpack_from("<H", self.data, 0xA)[0]
        self.rows = dict(struct.unpack_from("<i4xQ", self.data, 0x40 + i * 0x18) for i in range(row_count))
        # AC6 paramdefs can be stale: a def whose size doesn't match the rows would put fields at wrong offsets.
        starts = sorted(self.rows.values())
        strides = {b - a for a, b in zip(starts, starts[1:])}
        assert strides <= {offset}, f"{os.path.basename(def_path)} describes {offset:#x}-byte rows, the param has {strides}"

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
    thrusters = Param(os.path.join(params, "ThrustersSfxIdParam_PC.param"), os.path.join(defs, "ThrustersSfxIdParam.xml"))
    stock_flames = sorted({thrusters.get(row, f"sfxId{t}") for row in thrusters.rows for t in STOCK_COLOR_TYPES} - {-1})

    # Custom flames: copies of every stock flame with a tinted look switched by camera distance (see fxr/build_fxrs.mjs).
    fxr_dir = os.path.join(ROOT, "fxr")
    shell = os.name == "nt"
    if not os.path.isdir(os.path.join(fxr_dir, "node_modules")):
        subprocess.run(["npm", "install"], cwd=fxr_dir, check=True, shell=shell)
    effect_dir = os.path.join(unpacked(effects), "effect")
    id_map_path = os.path.join(WORK, "id_map.json")
    subprocess.run(["node", "build_fxrs.mjs", effect_dir, id_map_path, *map(str, stock_flames)], cwd=fxr_dir,
                   check=True, shell=shell)
    twin_of = {source: int(new) for new, source in json.load(open(id_map_path)).items()}
    # The new FXRs use their sources' textures and models. WitchyBND would generate empty resource lists for them.
    reslists = os.path.join(unpacked(effects), "ResourceList")
    for source_id, new_id in twin_of.items():
        target = os.path.join(reslists, f"f{new_id:09d}.ffxreslist")
        assert not os.path.exists(target), f"twin {new_id} collides with an existing effect"
        shutil.copy(os.path.join(reslists, f"f{source_id:09d}.ffxreslist"), target)

    # Params: every thruster effect row gets its custom twins, and every player generator with a stock color type
    # switches to that type's twin.
    for row in thrusters.rows:
        for t in STOCK_COLOR_TYPES:
            assert thrusters.get(row, f"sfxId{t + CUSTOM_TYPE_SHIFT}") == -1, f"row {row}: column {t + CUSTOM_TYPE_SHIFT} in use"
            stock = thrusters.get(row, f"sfxId{t}")
            if stock != -1:
                thrusters.set(row, f"sfxId{t + CUSTOM_TYPE_SHIFT}", twin_of[stock])
    thrusters.save()
    generators = Param(os.path.join(params, "EquipParamGenerator.param"), os.path.join(defs, "EquipParamGenerator.xml"))
    switched = 0
    for row in generators.rows:
        color_type = generators.get(row, "thrustersColorType")
        if row < NPC_GENERATOR_IDS_START and color_type in STOCK_COLOR_TYPES:
            generators.set(row, "thrustersColorType", color_type + CUSTOM_TYPE_SHIFT)
            switched += 1
    assert switched, "no generator had a stock thruster color type"
    generators.save()
    print(f"{switched} player generators switched to their custom flame twins")

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
