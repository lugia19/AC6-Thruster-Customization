"""Edits the named placements (PlaceObject2) of one DefineSprite in a GFX file by binary patch; everything else stays
byte-identical.

usage: gfx_place.py in.gfx out.gfx sprite_id OP [OP...]
  OP = add:<char_id>:<depth>:<name>:<tx>:<ty>     place a new instance (before the sprite's first ShowFrame)
       move:<name>:<new_name>:<tx>:<ty>          rename and/or move an existing named placement (twips)
"""
import struct
import sys


def rd_tag(d, pos):
    cl = struct.unpack_from('<H', d, pos)[0]
    t, l, h = cl >> 6, cl & 0x3f, 2
    if l == 0x3f:
        l, h = struct.unpack_from('<I', d, pos + 2)[0], 6
    return t, h, l


def tag(t, body):
    if len(body) < 0x3f:
        return struct.pack('<H', (t << 6) | len(body)) + body
    return struct.pack('<HI', (t << 6) | 0x3f, len(body)) + body


def matrix(tx, ty):
    n = max(v.bit_length() + 1 if v >= 0 else (~v).bit_length() + 1 for v in (tx, ty))
    bits = '00' + format(n, '05b') + ''.join(format(v & ((1 << n) - 1), '0%db' % n) for v in (tx, ty))
    bits += '0' * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def matrix_len(body, pos):
    """Byte length of the MATRIX record at body[pos]."""
    bits = ''.join(format(b, '08b') for b in body[pos:pos + 40])
    i = 0
    for _ in range(2):  # scale, rotate
        if bits[i] == '1':
            n = int(bits[i + 1:i + 6], 2)
            i += 6 + 2 * n
        else:
            i += 1
    n = int(bits[i:i + 5], 2)
    i += 5 + 2 * n
    return (i + 7) // 8


def place2(depth, cid, name, tx, ty):
    return tag(26, bytes([0x26]) + struct.pack('<HH', depth, cid) + matrix(tx, ty) + name.encode() + b'\0')


def parse_place2(body):
    """(depth, char_id, name) of a PlaceObject2 that has exactly a character, a matrix and a name, else None."""
    if body[0] != 0x26:
        return None
    depth, cid = struct.unpack_from('<HH', body, 1)
    p = 5 + matrix_len(body, 5)
    return depth, cid, body[p:body.index(b'\0', p)].decode()


def first_tag_pos(d):
    """Offset of the first tag: after the 8-byte header, the stage RECT (variable size) and frame rate/count."""
    nb = d[8] >> 3
    return 8 + (5 + 4 * nb + 7) // 8 + 4


def main():
    inp, out, sid = sys.argv[1], sys.argv[2], int(sys.argv[3])
    adds, moves = [], {}
    for op in sys.argv[4:]:
        kind, *a = op.split(':')
        if kind == 'add':
            adds.append(place2(int(a[1]), int(a[0]), a[2], int(a[3]), int(a[4])))
        elif kind == 'move':
            moves[a[0]] = (a[1], int(a[2]), int(a[3]))
        else:
            sys.exit(f'unknown op {op}')

    d = bytearray(open(inp, 'rb').read())
    pos = first_tag_pos(d)
    while True:
        t, h, l = rd_tag(d, pos)
        if t == 39 and struct.unpack_from('<H', d, pos + h)[0] == sid:
            break
        if t == 0:
            sys.exit('sprite not found')
        pos += h + l
    assert h == 6, 'sprite tag must use the long length form'
    body_start, body_end = pos + h + 4, pos + h + l

    new_body = bytearray()
    p, inserted = body_start, False
    while p < body_end:
        t2, h2, l2 = rd_tag(d, p)
        raw = bytes(d[p:p + h2 + l2])
        if t2 == 1 and not inserted:
            new_body += b''.join(adds)
            inserted = True
        placed = parse_place2(raw[h2:]) if t2 == 26 else None
        if placed and placed[2] in moves:
            depth, cid, name = placed
            new_name, tx, ty = moves.pop(name)
            raw = place2(depth, cid, new_name, tx, ty)
        new_body += raw
        p += h2 + l2
    if moves:
        sys.exit(f'placements not found: {list(moves)}')

    delta = len(new_body) - (body_end - body_start)
    d[body_start:body_end] = new_body
    struct.pack_into('<I', d, pos + 2, l + delta)
    struct.pack_into('<I', d, 4, struct.unpack_from('<I', d, 4)[0] + delta)
    open(out, 'wb').write(d)
    print(f'sprite {sid}: {len(adds)} added, size {delta:+d} bytes')


if __name__ == '__main__':
    main()
