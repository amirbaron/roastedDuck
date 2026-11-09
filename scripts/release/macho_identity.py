"""Hash file-backed Mach-O sections separately from loader/signature metadata."""
import hashlib
from pathlib import Path
import struct


def section_hashes(path):
    data = Path(path).read_bytes()
    base = 0
    magic = struct.unpack_from(">I", data)[0]
    if magic in (0xCAFEBABE, 0xCAFEBABF):
        assert struct.unpack_from(">I", data, 4)[0] == 1, "Only a single selected architecture is accepted"
        base = struct.unpack_from(">I" if magic == 0xCAFEBABE else ">Q", data, 16)[0]
    assert data[base:base + 4] == b"\xcf\xfa\xed\xfe"
    count = struct.unpack_from("<I", data, base + 16)[0]
    offset = base + 32
    sections = {}
    for _ in range(count):
        command, length = struct.unpack_from("<II", data, offset)
        assert length >= 8 and offset + length <= len(data)
        if command == 0x19:
            for index in range(struct.unpack_from("<I", data, offset + 64)[0]):
                entry = offset + 72 + index * 80
                name = data[entry:entry + 16].split(b"\0")[0].decode()
                segment = data[entry + 16:entry + 32].split(b"\0")[0].decode()
                size = struct.unpack_from("<Q", data, entry + 40)[0]
                position = struct.unpack_from("<I", data, entry + 48)[0]
                if position and size:
                    assert base + position + size <= len(data)
                    sections[segment + "/" + name] = hashlib.sha256(data[base + position:base + position + size]).hexdigest()
        offset += length
    assert "__TEXT/__text" in sections
    return sections
