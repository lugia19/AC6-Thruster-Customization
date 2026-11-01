// Builds the custom generator flames. For every given stock thruster FXR S:
// - a wrapper at S + WRAPPER_OFFSET (what the params point at): a small effect whose states enable either the stock S
//   itself or two custom copies of it, which play together;
// - the copies at S + ADD_OFFSET and S + REMOVE_OFFSET, plain copies of S (and of every FXR S references, remapped to
//   the same copy) with their own, untouched states, every color whitened to its brightness and tinted by custom
//   external values for the player's thruster color from the Paint screen:
//   - add: what the flame adds (lights and lens flares too), plus the alpha-blended particles (smoke), which can show
//     the color as is. The additive parts also fade out for the darkest colors: some keep the generator's hue whatever
//     their color properties say (textures, models);
//   - remove: the additive particles again, but subtracting what the color lacks from the background, so dark colors
//     darken it (additive flames can't get darker than what's behind them). Nothing for bright colors.
//   The DLL computes both amounts from the color (dll/src/thrusters.rs set_channels), so the look changes smoothly with
//   it: there's no switch between a bright and a dark version.
// The wrapper plays stock unless it spawns close to the camera (which in practice means on the player's AC): the
// game's per-effect camera distance (external value 2000, meters) against min/near thresholds the paint-menu DLL
// serves per family (stock ID ..Xxxxx -> family X: 41 continuous/falling/end smoke, 43, 44 assault boost, 45 quick
// boost, 0 the assault boost startup flare). Spawned from MIN up to NEAR, it plays the custom copies for its whole life
// (see wrapper()). Without the DLL every threshold reads 0, so everything stays stock.
// Usage: node build_fxrs.mjs <unpacked sfxbnd_commoneffects effect dir> <id map json out> <stock FXR id...>
// Writes the new FXRs into the same dir, and a map {twins: {stock id: wrapper id}, resources: {new id: source id}}
// (each new FXR needs its source's resource list; wrappers only hold proxies).
import {
  ActionType,
  BasicNode,
  BlendMode,
  ConstantProperty,
  ExternalValue1Modifier,
  FXR,
  Game,
  Keyframe,
  LinearProperty,
  ParticleModifier,
  ProxyNode,
  State,
} from '@cccode/fxr'
import fs from 'node:fs'
import path from 'node:path'

// Custom external values, red, green, blue each, served by the DLL as 1 + a 0..1 amount (1 = none, 2 = full). 0
// means nothing provides them (no DLL).
const EXT_ADD = 900001 // what the additive parts add
const EXT_REMOVE = 900004 // what the subtractive copy removes
const EXT_PLAIN = 900007 // the plain color, for alpha-blended particles
const EXT_ADD_OPACITY = 900010 // fades the additive parts out for the darkest colors (their textures keep a hue)
// The effect's distance from the camera in meters, provided by the game to almost every effect.
const EXT_CAMERA_DISTANCE = 2000
// The effect termination signal: the wrapper ends when the spawner says so, like the stock flames.
const EXT_TERMINATE = 0
// Tinting thresholds in meters, served by the DLL (live-tunable) per family X: 9001X0 min, 9001X1 near (must match
// dll/src/thrusters.rs EXT_THRESHOLDS_BASE).
const thresholdIds = family => {
  const base = 900100 + family * 10
  return { min: base, near: base + 1 }
}
// Stock thruster IDs are 4Xxxxx (X = family).
const familyOf = stockId => Math.floor(stockId / 10000) % 10

const WRAPPER_OFFSET = 9000000
const ADD_OFFSET = 9100000
const REMOVE_OFFSET = 9200000

// Keeps brightness and alpha, drops the hue: the tint supplies it.
const whiten = ([r, g, b, a]) => {
  const m = Math.max(r, g, b)
  return [m, m, m, a]
}

// Multiplier for some channels of a color by one served amount: 1 -> 0, 2 -> 1, linear in between; 0 (no DLL) ->
// `withoutDll`.
function channelTint(ext, channels, withoutDll) {
  const factor = v => [0, 1, 2, 3].map(i => (channels.includes(i) ? v : 1))
  const keyframes = [new Keyframe(0, factor(withoutDll)), new Keyframe(1, factor(0)), new Keyframe(2, factor(1))]
  return new ExternalValue1Modifier(ext, new LinearProperty(false, keyframes))
}

// Multiplies a color property by the served amounts starting at `ext` (red, green, blue), and optionally by the add
// opacity (on `opacityChannels`).
function addTint(action, key, ext, { withoutDll = 1, opacityChannels = null } = {}) {
  if ('field' in action.$data.props[key]) return 0 // fields can't carry modifiers
  if (Array.isArray(action[key])) {
    action[key] = new ConstantProperty(action[key])
  }
  action[key].modifiers.push(...[0, 1, 2].map(c => channelTint(ext + c, [c], withoutDll)))
  if (opacityChannels) action[key].modifiers.push(channelTint(EXT_ADD_OPACITY, opacityChannels, 1))
  return 1
}

// Debug (build.py --debug-parts, which sets PARTS_TSV): every part of the custom copies gets its own external value
// ID, 901000 + n, and a multiplier on it that the DLL sets to 0 for the IDs listed in paint_menu_hide.txt (it serves 2
// for those; anything else reads 0 or 1 and leaves the part alone). PARTS_TSV lists which ID is which.
const PARTS_TSV = process.env.PARTS_TSV
const EXT_PARTS_BASE = 901000
const partLines = []
const hideCurve = value => new LinearProperty(false, [new Keyframe(0, value(1)), new Keyframe(1, value(1)), new Keyframe(2, value(0))])
function toggle(fxr, copy, action, keys, what) {
  if (!PARTS_TSV) return
  const id = EXT_PARTS_BASE + partLines.length
  partLines.push([id, copy, fxr.id, ActionType[action.type] ?? action.type, keys.join(','), what].join('\t'))
  for (const key of keys) {
    if (!(key in action.$data.props) || 'field' in action.$data.props[key]) continue
    let prop = action[key]
    const vector = Array.isArray(prop) || Array.isArray(prop?.value)
    if (typeof prop === 'number' || Array.isArray(prop)) prop = action[key] = new ConstantProperty(prop)
    prop.modifiers.push(new ExternalValue1Modifier(id, hideCurve(v => (vector ? [v, v, v, v] : v))))
  }
}
const describe = action => {
  const out = []
  for (const k of Object.keys(action.$data?.props ?? {})) {
    if (!/^(texture|model|normalMap)$/.test(k)) continue
    const v = action[k]
    out.push(`${k}=${typeof v === 'number' ? v : v?.value}`)
  }
  return out.join(' ')
}

// Blend modes that add light: Add, and Unk0 / Unk7 (the library documents both as behaving like Add).
const ADDITIVE = new Set([BlendMode.Add, BlendMode.Unk0, BlendMode.Unk7])
const blendModeOf = appearance => {
  const mode = appearance.blendMode
  return typeof mode === 'number' ? mode : mode?.value
}
const isLightColor = (action, key) =>
  action.type === ActionType.PointLight || (action.type === ActionType.LensFlare && /^layer\dColor$/.test(key))

// A particle's color is the product of several properties (the appearance's color1 x color2 x color3, the config's
// ParticleModifier color, ...), so the tint goes on exactly one factor: the ParticleModifier. Everything else that
// feeds into it is only whitened. Lights and lens flares aren't particles and get the tint on their own colors.
// Distortion colors stay as they are: they're neutral, and tinting them darkens the background into dark shapes.
// `remove`: the subtractive copy. Additive particles subtract (whitened color x remove amounts) from the background,
// without the extra bloom glow. Everything the add copy already draws once is hidden: alpha-blended particles
// (subtracting them would darken their whole quad: dark squares), lights, lens flares and distortion.
function recolor(fxr, remove) {
  const copy = remove ? 'remove' : 'add'
  let tinted = 0
  for (const action of fxr.root.walkActions()) {
    if (!action.$data || !('props' in action.$data) || action.type === ActionType.Distortion) continue
    const lightKeys = []
    for (const [key, meta] of Object.entries(action.$data.props)) {
      if (!('color' in meta)) continue
      if (remove && isLightColor(action, key)) {
        action.recolorProperty(key, () => [0, 0, 0, 0])
        continue
      }
      action.recolorProperty(key, whiten)
      if (isLightColor(action, key)) {
        tinted += addTint(action, key, EXT_ADD, { opacityChannels: [0, 1, 2] })
        lightKeys.push(key)
      }
    }
    if (lightKeys.length) toggle(fxr, copy, action, lightKeys, 'light')
  }
  for (const config of fxr.root.walkConfigs()) {
    const appearance = config.appearance
    if (!appearance?.$data?.isParticle) continue
    if (appearance.type === ActionType.Distortion) {
      if (remove) {
        for (const key of ['sizeX', 'sizeY', 'sizeZ']) if (key in appearance.$data.props) appearance[key] = 0
      } else {
        toggle(fxr, copy, appearance, ['sizeX', 'sizeY', 'sizeZ'], `distortion ${describe(appearance)}`)
      }
      continue
    }
    if (!config.particleModifier) {
      config.particleModifier = new ParticleModifier()
    }
    // Model particles ignore the ParticleModifier's color (found with fxr-hunter: tinting it left them in their stock
    // color), so they take the tint on their own color2 (plain white in the stock flames) instead.
    const [target, key] = appearance.type === ActionType.Model ? [appearance, 'color2'] : [config.particleModifier, 'color']
    const additive = ADDITIVE.has(blendModeOf(appearance))
    const what = `${additive ? 'additive' : 'alpha-blended'} ${describe(appearance)}`
    if (!remove) {
      tinted += additive
        // RGB too, not just alpha: Unk0 / Unk7 may add without looking at alpha
        ? addTint(target, key, EXT_ADD, { opacityChannels: [0, 1, 2, 3] })
        : addTint(target, key, EXT_PLAIN)
      toggle(fxr, copy, target, [key], `${ActionType[appearance.type]} ${what}`)
    } else if (additive) {
      appearance.blendMode = BlendMode.Subtract
      if ('bloom' in appearance.$data.props) appearance.bloom = false
      tinted += addTint(target, key, EXT_REMOVE, { withoutDll: 0 })
      toggle(fxr, copy, target, [key], `${ActionType[appearance.type]} ${what}`)
    } else if (target === appearance) {
      appearance.color2 = [1, 1, 1, 0]
    } else {
      config.particleModifier.color = [1, 1, 1, 0]
    }
  }
  return tinted
}

// The wrapper for stock flame `stockId`: a child node per copy (a proxy to it); STOCK enables the stock flame, CUSTOM
// both custom copies. The choice is made once and kept (latched): a proxy's effect keeps playing after its node is
// disabled (found with fxr-hunter: switching from stock to custom left the stock flame running under the custom one),
// so a wrapper can only ever enable one of them. State conditions are checked in order, first match wins:
// - DECIDING (shows nothing): at or past NEAR -> STOCK; from MIN -> CUSTOM; closer than MIN for 0.1 s -> STOCK (this
//   also covers a distance that isn't set yet on the first update). Without the DLL every threshold reads 0: STOCK.
// - STOCK, CUSTOM: only end with the effect.
function wrapper(stockId) {
  const { min, near } = thresholdIds(familyOf(stockId))
  const distance = `External(${EXT_CAMERA_DISTANCE})`
  const terminate = `External(${EXT_TERMINATE}) >= 1 then terminate`
  const STOCK = 1, CUSTOM = 2
  const states = [
    State.from([
      terminate,
      `${distance} >= External(${near}) then ${STOCK}`,
      `${distance} >= External(${min}) then ${CUSTOM}`,
      `StateTime > 0.1 then ${STOCK}`,
    ]),
    State.from([terminate]),
    State.from([terminate]),
  ]
  const child = (sfx, state) => new BasicNode([], [new ProxyNode(sfx)]).mapStates(...states.map((_, s) => (s === state ? 0 : -1)))
  return new FXR(stockId + WRAPPER_OFFSET, states, [
    child(stockId, STOCK),
    child(stockId + ADD_OFFSET, CUSTOM),
    child(stockId + REMOVE_OFFSET, CUSTOM),
  ])
}

const [effectDir, idMapPath, ...stockArgs] = process.argv.slice(2)
const stockIds = stockArgs.map(Number)
const file = id => path.join(effectDir, `f${String(id).padStart(9, '0')}.fxr`)

// Every stock flame and everything it references through proxies: each gets both copies.
// A path, not fs.readFileSync: small files come back as views into Node's shared buffer pool, which the library
// reads from the pool's start.
const sources = new Set()
const pending = [...stockIds]
while (pending.length) {
  const id = pending.pop()
  if (sources.has(id)) continue
  sources.add(id)
  const fxr = await FXR.read(file(id), Game.ArmoredCore6)
  for (const node of fxr.root.walk()) {
    if (node instanceof ProxyNode) pending.push(node.sfx)
  }
}

// The effect dir is a fresh unpack of the game's effects, so an existing file at a new ID is a stock effect.
async function save(fxr) {
  if (fs.existsSync(file(fxr.id))) throw new Error(`${fxr.id} would overwrite an existing effect`)
  await fxr.saveAs(file(fxr.id), Game.ArmoredCore6)
}

const resources = {}
for (const sourceId of sources) {
  for (const [offset, remove] of [[ADD_OFFSET, false], [REMOVE_OFFSET, true]]) {
    const fxr = await FXR.read(file(sourceId), Game.ArmoredCore6)
    fxr.id = sourceId + offset
    for (const node of fxr.root.walk()) {
      if (node instanceof ProxyNode) node.sfx += offset
    }
    const tinted = recolor(fxr, remove)
    await save(fxr)
    resources[fxr.id] = sourceId
    console.log(`${sourceId} -> ${fxr.id}: ${remove ? 'remove' : 'add'}, ${tinted} colors tinted`)
  }
}

const twins = {}
for (const stockId of stockIds) {
  const fxr = wrapper(stockId)
  await save(fxr)
  twins[stockId] = fxr.id
  console.log(`${stockId} -> ${fxr.id}: wrapper, family ${familyOf(stockId)}`)
}
fs.writeFileSync(idMapPath, JSON.stringify({ twins, resources }, null, 1))
if (PARTS_TSV) {
  fs.writeFileSync(PARTS_TSV, ['id\tcopy\tfxr\taction\tproperties\twhat', ...partLines].join('\n') + '\n')
  console.log(`${partLines.length} parts listed in ${PARTS_TSV}: IDs ${EXT_PARTS_BASE}-${EXT_PARTS_BASE + partLines.length - 1}`)
}
