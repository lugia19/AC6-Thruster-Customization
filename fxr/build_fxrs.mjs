// Builds the custom generator flames. For every given stock thruster FXR S:
// - a wrapper at S + WRAPPER_OFFSET (what the params point at): a small effect whose states enable one of three child
//   proxies: the stock S itself, a tinted variant, or a dark variant;
// - variants at S + TINTED_OFFSET and S + DARK_OFFSET, plain copies of S (and of every FXR S references, remapped to
//   the same variant) with their own, untouched states:
//   - tinted: every color whitened to its brightness, tinted by three custom external values (R, G, B), the player's
//     thruster color from the Paint screen;
//   - dark: the same, but the particles subtract from the background (inverted tint), so dark colors darken instead of
//     vanishing (additive flames can't get darker than what's behind them).
// The wrapper plays stock unless it's close to the camera (which in practice means it's on the player's AC): the
// game's per-effect camera distance (external value 2000, meters) against min/near/far thresholds the paint-menu DLL
// serves per flame family (stock ID 4Xxxxx -> family X: 41 continuous/falling/end smoke, 43, 44 assault boost, 45
// quick boost; the camera sits closer during assault boost). From MIN up to NEAR it switches to tinted, or to dark
// while the DLL's dark-mode value (900006) is 1; above FAR or below MIN it goes back to stock (the NEAR-FAR gap stops
// flickering at the boundary). Without the DLL every threshold reads 0, so everything stays stock.
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

// Custom external values, served by the DLL as 1 + a 0..1 tint factor (1 = channel off, 2 = channel full). 0 means
// nothing provides them (no DLL), which the tint curves map to "no tint".
const EXT_RED = 900001
const EXT_GREEN = 900002
const EXT_BLUE = 900003
// 1 while the player's color is dark enough for the dark variant (DLL), else 0.
const EXT_DARK = 900006
// The effect's distance from the camera in meters, provided by the game to almost every effect.
const EXT_CAMERA_DISTANCE = 2000
// The effect termination signal: the wrapper ends when the spawner says so, like the stock flames.
const EXT_TERMINATE = 0
// Tinting thresholds in meters, served by the DLL (live-tunable) per flame family X: 9001X0 min, 9001X1 near,
// 9001X2 far (must match dll/src/thrusters.rs EXT_THRESHOLDS_BASE).
const thresholdIds = family => {
  const base = 900100 + family * 10
  return { min: base, near: base + 1, far: base + 2 }
}
// Stock thruster IDs are 4Xxxxx (X = family).
const familyOf = stockId => Math.floor(stockId / 10000) % 10

const WRAPPER_OFFSET = 9000000
const TINTED_OFFSET = 9100000
const DARK_OFFSET = 9200000

// Keeps brightness and alpha, drops the hue: the tint supplies it.
const whiten = ([r, g, b, a]) => {
  const m = Math.max(r, g, b)
  return [m, m, m, a]
}

// Multiplier for one channel. Tinted: 0 (no DLL) -> 1, 1 -> 0, 2 -> 1, linear in between. Inverted (what the dark
// variant subtracts): 1 -> 1, 2 -> 0, so a dark channel removes a lot of that channel from the background.
function channelTint(ext, channel, inverted) {
  const factor = v => [0, 1, 2, 3].map(i => (i === channel ? v : 1))
  const keyframes = inverted
    ? [new Keyframe(0, factor(1)), new Keyframe(1, factor(1)), new Keyframe(2, factor(0))]
    : [new Keyframe(0, factor(1)), new Keyframe(1, factor(0)), new Keyframe(2, factor(1))]
  return new ExternalValue1Modifier(ext, new LinearProperty(false, keyframes))
}

function addTint(action, key, inverted = false) {
  if ('field' in action.$data.props[key]) return 0 // fields can't carry modifiers
  if (Array.isArray(action[key])) {
    action[key] = new ConstantProperty(action[key])
  }
  action[key].modifiers.push(
    channelTint(EXT_RED, 0, inverted),
    channelTint(EXT_GREEN, 1, inverted),
    channelTint(EXT_BLUE, 2, inverted),
  )
  return 1
}

// A particle's color is the product of several properties (the appearance's color1 x color2 x color3, the config's
// ParticleModifier color, ...), so the tint goes on exactly one factor: the ParticleModifier. Everything else that
// feeds into it is only whitened. Lights and lens flares aren't particles and get the tint on their own colors.
// Distortion colors stay as they are: they're neutral, and tinting them darkens the background into dark shapes.
// Dark: additive particles subtract (whitened color x inverted tint) from the background instead, without the extra
// bloom glow. Alpha-blended ones (Normal, e.g. smoke) can already show dark colors, so they keep their blend mode and
// the normal tint (subtracting them would darken their whole quad: dark squares). Lights and lens flares can only add
// light, so they keep the normal (dim) tint too.
// Blend modes that add light: Add, and Unk0 / Unk7 (the library documents both as behaving like Add).
const ADDITIVE = new Set([BlendMode.Add, BlendMode.Unk0, BlendMode.Unk7])
const blendModeOf = appearance => {
  const mode = appearance.blendMode
  return typeof mode === 'number' ? mode : mode?.value
}

function recolor(fxr, dark) {
  let tinted = 0
  for (const action of fxr.root.walkActions()) {
    if (!action.$data || !('props' in action.$data) || action.type === ActionType.Distortion) continue
    for (const [key, meta] of Object.entries(action.$data.props)) {
      if (!('color' in meta)) continue
      action.recolorProperty(key, whiten)
      if (action.type === ActionType.PointLight || (action.type === ActionType.LensFlare && /^layer\dColor$/.test(key))) {
        tinted += addTint(action, key)
      }
    }
  }
  for (const config of fxr.root.walkConfigs()) {
    const appearance = config.appearance
    if (!appearance?.$data?.isParticle || appearance.type === ActionType.Distortion) continue
    const subtract = dark && ADDITIVE.has(blendModeOf(appearance))
    if (subtract) {
      appearance.blendMode = BlendMode.Subtract
      if ('bloom' in appearance.$data.props) appearance.bloom = false
    }
    if (!config.particleModifier) {
      config.particleModifier = new ParticleModifier()
    }
    tinted += addTint(config.particleModifier, 'color', subtract)
  }
  return tinted
}

// The wrapper for stock flame `stockId`: one child node per variant (a proxy to it), each enabled in one state.
function wrapper(stockId) {
  const { min, near, far } = thresholdIds(familyOf(stockId))
  const distance = `External(${EXT_CAMERA_DISTANCE})`
  const terminate = `External(${EXT_TERMINATE}) >= 1 then terminate`
  const STOCK = 0, TINTED = 1, DARK = 2
  const states = [
    State.from([terminate, `${distance} < External(${min}) then ${STOCK}`, `${distance} < External(${near}) then ${TINTED}`]),
    State.from([
      terminate,
      `${distance} < External(${min}) then ${STOCK}`,
      `${distance} > External(${far}) then ${STOCK}`,
      `External(${EXT_DARK}) >= 1 then ${DARK}`,
    ]),
    State.from([
      terminate,
      `${distance} < External(${min}) then ${STOCK}`,
      `${distance} > External(${far}) then ${STOCK}`,
      `External(${EXT_DARK}) < 1 then ${TINTED}`,
    ]),
  ]
  const child = (sfx, state) => new BasicNode([], [new ProxyNode(sfx)]).mapStates(...[0, 1, 2].map(s => (s === state ? 0 : -1)))
  return new FXR(stockId + WRAPPER_OFFSET, states, [
    child(stockId, STOCK),
    child(stockId + TINTED_OFFSET, TINTED),
    child(stockId + DARK_OFFSET, DARK),
  ])
}

const [effectDir, idMapPath, ...stockArgs] = process.argv.slice(2)
const stockIds = stockArgs.map(Number)
const file = id => path.join(effectDir, `f${String(id).padStart(9, '0')}.fxr`)

// Every stock flame and everything it references through proxies: each gets a tinted and a dark variant.
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
  for (const [offset, dark] of [[TINTED_OFFSET, false], [DARK_OFFSET, true]]) {
    const fxr = await FXR.read(file(sourceId), Game.ArmoredCore6)
    fxr.id = sourceId + offset
    for (const node of fxr.root.walk()) {
      if (node instanceof ProxyNode) node.sfx += offset
    }
    const tinted = recolor(fxr, dark)
    await save(fxr)
    resources[fxr.id] = sourceId
    console.log(`${sourceId} -> ${fxr.id}: ${dark ? 'dark' : 'tinted'}, ${tinted} colors tinted`)
  }
}

const twins = {}
for (const stockId of stockIds) {
  const fxr = wrapper(stockId)
  await save(fxr)
  twins[stockId] = fxr.id
  console.log(`${stockId} -> ${fxr.id}: wrapper, family 4${familyOf(stockId)}`)
}
fs.writeFileSync(idMapPath, JSON.stringify({ twins, resources }, null, 1))
