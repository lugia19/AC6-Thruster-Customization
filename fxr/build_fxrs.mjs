// Builds the custom generator flames. For every given stock thruster FXR S:
// - a wrapper at S + WRAPPER_OFFSET (what the params point at), which plays either the stock S or two custom copies of
//   it together (see wrapper());
// - the copies at S + ADD_OFFSET and S + REMOVE_OFFSET: plain copies of S (and of every FXR S references, remapped to
//   the same copy), every color whitened to its brightness and tinted by external values the paint-menu DLL serves
//   for the player's thruster color (dll/src/thrusters.rs set_channels):
//   - add: the flame with the color added (lights and lens flares too); alpha-blended particles (smoke) get the plain
//     color, which they can show as is;
//   - remove: the additive particles again, subtracting what the color lacks from the background, so dark colors
//     darken it (additive flames can't get darker than what's behind them). Nothing for bright colors.
// Usage: node build_fxrs.mjs <unpacked sfxbnd_commoneffects effect dir> <id map json out> <stock FXR id...>
// Writes the new FXRs into the same dir, and a map {wrappers: {stock id: wrapper id}, resources: {new id: source id}}
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

// External values the DLL serves, red, green, blue each, as 1 + a 0..1 amount; 0 means no DLL.
const EXT_ADD = 900001 // what the add copy adds
const EXT_REMOVE = 900004 // what the remove copy subtracts
const EXT_PLAIN = 900007 // the plain color, for alpha-blended particles
// Whether the flame is on the player's AC (dll/src/player_flames.rs): 0 not known yet (or no DLL), 1 someone else's,
// 2 the player's.
const EXT_PLAYERS_FLAME = 900010
// Game value: the spawner's termination signal.
const EXT_TERMINATE = 0

const WRAPPER_OFFSET = 9000000
const ADD_OFFSET = 9100000
const REMOVE_OFFSET = 9200000

// Blend modes that add light: Add, and Unk0 / Unk7 (the library documents both as behaving like Add).
const ADDITIVE = new Set([BlendMode.Add, BlendMode.Unk0, BlendMode.Unk7])

// Keeps brightness and alpha, drops the hue: the tint supplies it.
const whiten = ([r, g, b, a]) => {
  const m = Math.max(r, g, b)
  return [m, m, m, a]
}

// A color multiplier that scales only one channel (0 red, 1 green, 2 blue) by `amount`.
function scaleChannel(channel, amount) {
  const multiplier = [1, 1, 1, 1]
  multiplier[channel] = amount
  return multiplier
}

// Multiplies a color property's red, green and blue by the amounts the DLL serves at `ext`, `ext + 1` and `ext + 2`.
// The DLL serves 1 + amount, so the curve maps 1 -> 0 and 2 -> 1; 0 (no DLL) maps to `withoutDll`.
function addTint(action, key, ext, withoutDll = 1) {
  if ('field' in action.$data.props[key]) return // fields can't carry modifiers
  if (Array.isArray(action[key])) {
    action[key] = new ConstantProperty(action[key])
  }
  for (const channel of [0, 1, 2]) {
    const curve = new LinearProperty(false, [
      new Keyframe(0, scaleChannel(channel, withoutDll)),
      new Keyframe(1, scaleChannel(channel, 0)),
      new Keyframe(2, scaleChannel(channel, 1)),
    ])
    action[key].modifiers.push(new ExternalValue1Modifier(ext + channel, curve))
  }
}

// Lights and lens flare layers: they can only add light, and get the tint on their own colors.
function isLightColor(action, key) {
  if (action.type === ActionType.PointLight) return true
  return action.type === ActionType.LensFlare && /^layer\dColor$/.test(key)
}

// An appearance's blend mode is either a number or a property holding one.
function blendModeOf(appearance) {
  const mode = appearance.blendMode
  return typeof mode === 'number' ? mode : mode?.value
}

// A particle's color is the product of several properties (the appearance's colors, the config's ParticleModifier
// color...), so the tint goes on exactly one factor: the ParticleModifier, or color2 for Model particles, which ignore
// the ParticleModifier's color (found with fxr-hunter). Everything else is only whitened. Lights and lens flares get
// the tint on their own colors. Distortion stays as it is: tinting it darkens the background into dark shapes.
// `remove`: additive particles subtract from the background instead, without bloom. Everything the add copy already
// draws is hidden: alpha-blended particles (subtracting them would darken their whole quad), lights, lens flares and
// distortion.
function recolor(fxr, remove) {
  // Every color property: whitened, and tinted for lights (hidden in the remove copy).
  for (const action of fxr.root.walkActions()) {
    if (!action.$data || !('props' in action.$data) || action.type === ActionType.Distortion) continue
    for (const [key, meta] of Object.entries(action.$data.props)) {
      if (!('color' in meta)) continue
      if (!isLightColor(action, key)) {
        action.recolorProperty(key, whiten)
      } else if (remove) {
        action.recolorProperty(key, () => [0, 0, 0, 0])
      } else {
        action.recolorProperty(key, whiten)
        addTint(action, key, EXT_ADD)
      }
    }
  }
  // Every particle: the tint on one factor (see above).
  for (const config of fxr.root.walkConfigs()) {
    const appearance = config.appearance
    if (!appearance?.$data?.isParticle) continue
    if (appearance.type === ActionType.Distortion) {
      if (remove) {
        for (const key of ['sizeX', 'sizeY', 'sizeZ']) {
          if (key in appearance.$data.props) appearance[key] = 0
        }
      }
      continue
    }
    if (!config.particleModifier) {
      config.particleModifier = new ParticleModifier()
    }
    let target = config.particleModifier
    let key = 'color'
    if (appearance.type === ActionType.Model) {
      target = appearance
      key = 'color2'
    }
    const additive = ADDITIVE.has(blendModeOf(appearance))
    if (!remove) {
      addTint(target, key, additive ? EXT_ADD : EXT_PLAIN)
    } else if (additive) {
      appearance.blendMode = BlendMode.Subtract
      if ('bloom' in appearance.$data.props) appearance.bloom = false
      addTint(target, key, EXT_REMOVE, 0)
    } else {
      target[key] = [1, 1, 1, 0]
    }
  }
}

// The wrapper for stock flame `stockId`: a child node per effect it can play (a proxy to it); STOCK enables the stock
// flame, CUSTOM both copies. The choice is made once, at spawn, and kept: a proxy's effect keeps playing after its node
// is disabled (switching from stock to custom left the stock flame running under the custom one), so a wrapper may
// only ever enable one of them. Conditions are checked in order, first match wins:
// - DECIDING (shows nothing): no DLL (its color values read 0) -> STOCK; the player's flame -> CUSTOM; someone else's
//   -> STOCK; still not known after 0.3 s -> STOCK (the DLL answers on the owner's next slot update, a frame after
//   the spawn; the wait only lasts while there's no answer, e.g. a flame a cutscene spawned).
// - STOCK, CUSTOM: only end with the effect.
function wrapper(stockId) {
  const playersFlame = `External(${EXT_PLAYERS_FLAME})`
  const terminate = `External(${EXT_TERMINATE}) >= 1 then terminate`
  const STOCK = 1, CUSTOM = 2
  const states = [
    State.from([
      terminate,
      `External(${EXT_ADD}) < 1 then ${STOCK}`,
      `${playersFlame} >= 2 then ${CUSTOM}`,
      `${playersFlame} >= 1 then ${STOCK}`,
      `StateTime > 0.3 then ${STOCK}`,
    ]),
    State.from([terminate]),
    State.from([terminate]),
  ]
  // A node holding a proxy to `sfx`, enabled (config 0) only in `state`, disabled (-1) in the others.
  const child = (sfx, state) => {
    const configPerState = states.map((_, s) => (s === state ? 0 : -1))
    return new BasicNode([], [new ProxyNode(sfx)]).mapStates(...configPerState)
  }
  return new FXR(stockId + WRAPPER_OFFSET, states, [
    child(stockId, STOCK),
    child(stockId + ADD_OFFSET, CUSTOM),
    child(stockId + REMOVE_OFFSET, CUSTOM),
  ])
}

const [effectDir, idMapPath, ...stockArgs] = process.argv.slice(2)
const stockIds = stockArgs.map(Number)
const file = id => path.join(effectDir, `f${String(id).padStart(9, '0')}.fxr`)
// A path, not fs.readFileSync: small files come back as views into Node's shared buffer pool, which the library
// reads from the pool's start.
const read = id => FXR.read(file(id), Game.ArmoredCore6)
// The effect dir is a fresh unpack of the game's effects, so an existing file at a new ID is a stock effect.
async function save(fxr) {
  if (fs.existsSync(file(fxr.id))) throw new Error(`${fxr.id} would overwrite an existing effect`)
  await fxr.saveAs(file(fxr.id), Game.ArmoredCore6)
}

// Every stock flame and everything it references through proxies gets both copies.
const sources = new Set()
const pending = [...stockIds]
while (pending.length) {
  const id = pending.pop()
  if (sources.has(id)) continue
  sources.add(id)
  for (const node of (await read(id)).root.walk()) {
    if (node instanceof ProxyNode) pending.push(node.sfx)
  }
}

const resources = {}
for (const sourceId of sources) {
  for (const [offset, remove] of [[ADD_OFFSET, false], [REMOVE_OFFSET, true]]) {
    const fxr = await read(sourceId)
    fxr.id = sourceId + offset
    for (const node of fxr.root.walk()) {
      if (node instanceof ProxyNode) node.sfx += offset
    }
    recolor(fxr, remove)
    await save(fxr)
    resources[fxr.id] = sourceId
  }
}

const wrappers = {}
for (const stockId of stockIds) {
  const fxr = wrapper(stockId)
  await save(fxr)
  wrappers[stockId] = fxr.id
}
fs.writeFileSync(idMapPath, JSON.stringify({ wrappers, resources }, null, 1))
console.log(`${stockIds.length} wrappers, ${sources.size * 2} copies (${sources.size} sources)`)
