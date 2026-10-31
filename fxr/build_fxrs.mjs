// Builds the custom generator flames: a copy of every given stock thruster FXR (and every FXR they reference) at
// +ID_OFFSET. Each copy plays its stock look, or, while it's close to the camera (which in practice means it's on the
// player's AC), a whitened version tinted by three custom external values (R, G, B): the player's thruster color
// from the Paint screen. "Close" is the game's per-effect camera distance (external value 2000, meters) against
// thresholds the paint-menu DLL serves per flame family (stock ID 4Xxxxx -> family X: 41 continuous/falling/end smoke,
// 43, 44 assault boost, 45 quick boost; the camera sits closer during assault boost): from MIN up to NEAR the effect
// turns tinted, above FAR or below MIN it turns stock again (the NEAR-FAR gap stops flickering at the boundary).
// Without the DLL the thresholds read 0, so everything stays stock.
// Usage: node build_fxrs.mjs <unpacked sfxbnd_commoneffects effect dir> <id map json out> <stock FXR id...>
// Writes the new FXRs into the same dir, and a {new id: source id} map (the new FXRs need the source's resource
// lists).
import {
  ActionType,
  ConstantProperty,
  ExternalValue1Modifier,
  FXR,
  Game,
  Keyframe,
  LinearProperty,
  NodeWithConfigs,
  ParticleModifier,
  ProxyNode,
  State,
  StateCondition,
} from '@cccode/fxr'
import fs from 'node:fs'
import path from 'node:path'

// Custom external values, served by the DLL as channel + 1 (1 = channel off, 2 = channel full). 0 means nothing
// provides them (no DLL), which the tint curve maps to "no tint", so the flames stay white instead of black.
const EXT_RED = 900001
const EXT_GREEN = 900002
const EXT_BLUE = 900003
// The effect's distance from the camera in meters, provided by the game to almost every effect.
const EXT_CAMERA_DISTANCE = 2000
// Tinting thresholds in meters, served by the DLL (live-tunable) per flame family X: 9001X0 min, 9001X1 near,
// 9001X2 far (must match dll/src/thrusters.rs EXT_THRESHOLDS_BASE).
const thresholdIds = family => {
  const base = 900100 + family * 10
  return { min: base, near: base + 1, far: base + 2 }
}
// Stock thruster IDs are 4Xxxxx (X = family); shared child effects get their own ID's family.
const familyOf = stockId => Math.floor(stockId / 10000) % 10

const ID_OFFSET = 9000000

// Keeps brightness and alpha, drops the hue: the tint supplies it.
const whiten = ([r, g, b, a]) => {
  const m = Math.max(r, g, b)
  return [m, m, m, a]
}

// Multiplier for one channel: 0 (no DLL) -> 1, 1 -> 0, 2 -> 1, linear in between.
function channelTint(ext, channel) {
  const factor = v => [0, 1, 2, 3].map(i => (i === channel ? v : 1))
  return new ExternalValue1Modifier(ext, new LinearProperty(false, [
    new Keyframe(0, factor(1)),
    new Keyframe(1, factor(0)),
    new Keyframe(2, factor(1)),
  ]))
}

function addTint(action, key) {
  if ('field' in action.$data.props[key]) return 0 // fields can't carry modifiers
  if (Array.isArray(action[key])) {
    action[key] = new ConstantProperty(action[key])
  }
  action[key].modifiers.push(channelTint(EXT_RED, 0), channelTint(EXT_GREEN, 1), channelTint(EXT_BLUE, 2))
  return 1
}

// A particle's color is the product of several properties (the appearance's color1 x color2 x color3, the config's
// ParticleModifier color, ...), so the tint goes on exactly one factor: the ParticleModifier. Everything else that
// feeds into it is only whitened. Lights and lens flares aren't particles and get the tint on their own colors.
// Distortion colors stay as they are: they're neutral, and tinting them darkens the background into dark shapes.
function tintConfig(config) {
  let tinted = 0
  for (const action of config.walkActions()) {
    if (!action.$data || !('props' in action.$data) || action.type === ActionType.Distortion) continue
    for (const [key, meta] of Object.entries(action.$data.props)) {
      if (!('color' in meta)) continue
      action.recolorProperty(key, whiten)
      if (action.type === ActionType.PointLight || (action.type === ActionType.LensFlare && /^layer\dColor$/.test(key))) {
        tinted += addTint(action, key)
      }
    }
  }
  const appearance = config.appearance
  if (appearance?.$data?.isParticle && appearance.type !== ActionType.Distortion) {
    if (!config.particleModifier) {
      config.particleModifier = new ParticleModifier()
    }
    tinted += addTint(config.particleModifier, 'color')
  }
  return tinted
}

// Doubles the effect: states 0..k-1 keep the stock look, states k..2k-1 are the same states with tinted configs.
// Every node gets tinted clones of its configs, mapped in the second half; each state also switches to its twin in
// the other half when the camera distance crosses the family's thresholds. Conditions run in order and the first
// true one wins, so "below MIN" goes first: in a stock state it keeps the effect where it is (a jump to itself).
function addDistanceSwitch(fxr, family) {
  const { min, near, far } = thresholdIds(family)
  let tinted = 0
  for (const node of fxr.root.walk()) {
    if (!(node instanceof NodeWithConfigs)) continue
    const count = node.configs.length
    const clones = node.configs.map(config => config.clone())
    for (const clone of clones) tinted += tintConfig(clone)
    node.configs.push(...clones)
    node.stateConfigMap = [...node.stateConfigMap, ...node.stateConfigMap.map(c => (c < 0 ? -1 : c + count))]
  }
  const k = fxr.states.length
  const shifted = condition => {
    const copy = condition.clone()
    if (copy.state >= 0) copy.state += k
    return copy
  }
  const distance = `External(${EXT_CAMERA_DISTANCE})`
  const stock = fxr.states.map((state, i) => new State([
    ...state.conditions.map(c => c.clone()),
    StateCondition.from(`${distance} < External(${min}) then ${i}`),
    StateCondition.from(`${distance} < External(${near}) then ${k + i}`),
  ]))
  const tintedStates = fxr.states.map((state, i) => new State([
    ...state.conditions.map(shifted),
    StateCondition.from(`${distance} < External(${min}) then ${i}`),
    StateCondition.from(`${distance} > External(${far}) then ${i}`),
  ]))
  fxr.states = [...stock, ...tintedStates]
  return tinted
}

const [effectDir, idMapPath, ...stockIds] = process.argv.slice(2)
const file = id => path.join(effectDir, `f${String(id).padStart(9, '0')}.fxr`)

// The stock flames and everything they reference through proxies.
// A path, not fs.readFileSync: small files come back as views into Node's shared buffer pool, which the library
// reads from the pool's start.
const fxrs = new Map()
const pending = stockIds.map(Number)
while (pending.length) {
  const id = pending.pop()
  if (fxrs.has(id)) continue
  const fxr = await FXR.read(file(id), Game.ArmoredCore6)
  fxrs.set(id, fxr)
  for (const node of fxr.root.walk()) {
    if (node instanceof ProxyNode) pending.push(node.sfx)
  }
}

for (const [oldId, fxr] of fxrs) {
  fxr.id = oldId + ID_OFFSET
  // The effect dir is a fresh unpack of the game's effects, so an existing file here is a stock effect.
  if (fs.existsSync(file(fxr.id))) throw new Error(`twin ${fxr.id} of ${oldId} would overwrite an existing effect`)
  for (const node of fxr.root.walk()) {
    if (node instanceof ProxyNode) node.sfx += ID_OFFSET
  }
  const family = familyOf(oldId)
  const tinted = addDistanceSwitch(fxr, family)
  await fxr.saveAs(file(fxr.id), Game.ArmoredCore6)
  console.log(`${oldId} -> ${fxr.id}: family 4${family}, ${fxr.states.length} states, ${tinted} colors tinted`)
}
fs.writeFileSync(idMapPath, JSON.stringify(Object.fromEntries([...fxrs.keys()].map(id => [id + ID_OFFSET, id])), null, 1))
