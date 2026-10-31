"""Prints a GFX menu's display tree (instance names, symbols, text placeholders) from a JPEXS FFDec XML export, to find
the Item_N_M rows the game binds. Make the XML with strip_exporter.py, then ffdec-cli -swf2xml.

usage: gfxtree.py export.xml [max depth]
"""
import sys, xml.etree.ElementTree as ET
root=ET.parse(sys.argv[1]).getroot()
top=root.find('tags')
chars={}; cls={}
for t in top:
    ty=t.get('type')
    if ty=='SymbolClassTag':
        ids=[i.text for i in t.find('tags')]; names=[i.text for i in t.find('names')]
        cls=dict(zip(ids,names))
    for k in ('spriteId','shapeId','characterID','characterId','fontId'):
        if t.get(k) and ty.startswith('Define'): chars[t.get(k)]=t
def kind(cid):
    t=chars.get(cid); return (t.get('type').replace('Define','').replace('Tag','') if t is not None else '?')
def places(tags):
    out=[]; seen=set()
    for t in tags:
        if t.get('type','').startswith('PlaceObject') and t.get('characterId') and t.get('placeFlagHasCharacter','true')!='false':
            key=(t.get('characterId'),t.get('name'))
            if key in seen: continue
            seen.add(key); out.append(key)
    return out
maxd=int(sys.argv[2]) if len(sys.argv)>2 else 4
def walk(tags,d,stack):
    for cid,name in places(tags):
        t=chars.get(cid); k=kind(cid)
        extra=''
        if k=='EditText': extra=' text=%r'%(t.get('initialText') or '')[:40]
        print('  '*d+f'{name or "-"} [{k} {cid} {cls.get(cid,"")}]'+extra)
        if k=='Sprite' and d<maxd and cid not in stack:
            walk(t.find('subTags'),d+1,stack|{cid})
walk(top,0,set())
