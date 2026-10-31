"""Turns a FromSoft GFX file into a plain SWF that JPEXS FFDec can open: FFDec fails on the GFX ExporterInfo tag
(1000), so it's dropped and the header becomes FWS. For inspection only (the game needs the GFX).

usage: strip_exporter.py in.gfx out.swf
"""
import struct
import sys

from gfx_place import first_tag_pos, rd_tag

d = bytearray(open(sys.argv[1], 'rb').read())
pos = first_tag_pos(d)
t, h, l = rd_tag(d, pos)
assert t == 1000, 'expected the ExporterInfo tag first'
del d[pos:pos + h + l]
d[0:3] = b'FWS'
struct.pack_into('<I', d, 4, len(d))
open(sys.argv[2], 'wb').write(d)
