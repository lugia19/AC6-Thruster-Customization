# AC6-Thruster-Customization

Adds a THRUSTER color to the Paint screen in Armored Core VI (1.09.1). Your AC's thruster flames take that color:
boosting, quick boosting, assault boosting, all of it.

## Use

1. Grab the zip from [Releases](../../releases) (or build it, below), extract it anywhere, and launch
   `paint-menu.me3` with [me3](https://github.com/garyttierney/me3).
   It uses its own save, `PaintMenu.sl2`, which me3 copies from your real save on first launch.
2. In the garage, Paint: there's a new THRUSTER row under DEVICE. It works like DEVICE, one color for the whole AC.

The color is stored in the AC's paint data, so it's saved with the AC and carried by designs. Until you pick
one, it's white. Dark colors work too: the flames darken what's behind them instead of glowing.

## Known issues

- The Paint screen's pattern thumbnail can stop updating after going in and out of it a few times. That's an me3 bug
  with overridden menu files, not this mod: [me3#902](https://github.com/garyttierney/me3/issues/902).
- The THRUSTER label is in English in every language.

## How it works

- **Paint row.** The menu layout (`menu/02_coloring.gfx`) gets a 7th color row, patched in by `tools/gfx_place.py`.
  The DLL hooks the Paint screen so that row exists, edits as a single color, and reads and writes its color in a part
  of the AC's paint data the game never uses (section 7, color slot 0).
- **Flames.** Every player generator gets a new thruster color type, whose flames are wrapper effects built by
  `fxr/build_fxrs.mjs`. Enemy ACs use the same generators, so when a wrapper spawns, it asks the DLL whether it's on
  your AC. The DLL compares the flame's position with yours and with every other AC's: it's yours if it's close to
  you and closer to you than to anyone else, so an enemy AC's flames stay stock even when it's touching you. Yours
  play two recolored copies of the stock flame, one adding the color and one subtracting what the color lacks
  (that's what makes dark colors work). It decides once and sticks to it, since a flame it already started keeps
  playing even if it switches.
- **Color.** The DLL reads your color from the paint data and serves it to the flames as FXR external values, the
  same way as [AC6-FXR-DLL-POC](https://github.com/lugia19/AC6-FXR-DLL-POC). Without the DLL, everything plays stock.
  
  The DLL does all the color math, and the flames only multiply by what it serves. That matters: an FXR can only take
  one external value, run it through a curve and multiply it in, so anything that depends on the whole color (how
  bright it is overall, which decides how much the subtracting copy darkens) can't be done inside the FXR. The DLL
  works out the add, subtract and plain amounts once per color change, and the FXR just reads them.

## Build

```
python build.py --game "<...>/ARMORED CORE VI FIRES OF RUBICON/Game" --witchy "<...>/WitchyBND.exe"
```

Needs Python 3, node/npm, cargo (stable) and [WitchyBND](https://github.com/ividyon/WitchyBND) 3.x. It writes
`package/`: the mod folder, `paint_menu.dll` and `paint-menu.me3`.

WitchyBND 3.0.1 refuses to repack 1.09.1 regulations out of the box: raise the version in its
`Assets/Paramdex/AC6/Upgrader/version.txt` to `1_09_1_0042L`.
