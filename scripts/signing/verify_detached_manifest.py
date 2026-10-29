"""Verify a codesign-detached SHA256 manifest with a pinned certificate and OpenSSL.

Restricted to a single SHA256 CodeDirectory for a plain manifest, without scatter
or alternate code directories. Format: Apple's xnu/osfmk/kern/cs_blobs.h. OpenSSL
verifies CMS; this script checks the signed page hashes and listed artifact hashes.
"""
import argparse
import hashlib
import hmac
from pathlib import Path
import struct
import subprocess
import tempfile


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify(manifest, signature, certificate, openssl, tmpdir, certificate_sha256):
    der = subprocess.check_output([openssl, "x509", "-in", str(certificate), "-outform", "DER"])
    require(hmac.compare_digest(hashlib.sha256(der).hexdigest(), certificate_sha256.lower()), "certificate fingerprint mismatch")
    payload = manifest.read_bytes()
    data = signature.read_bytes()
    require(len(data) >= 12, "truncated signature")
    magic, length, count = struct.unpack_from(">III", data)
    require(magic == 0xFADE0CC0 and length == len(data), "unsupported signature container")
    require(count <= 32 and 12 + count * 8 <= length, "invalid signature index")
    blobs = {}
    for index in range(count):
        kind, offset = struct.unpack_from(">II", data, 12 + index * 8)
        require(kind not in blobs and offset >= 12 + count * 8 and offset + 8 <= length, "invalid blob offset")
        blob_magic, blob_length = struct.unpack_from(">II", data, offset)
        require(blob_length >= 8 and offset + blob_length <= length, "truncated signature blob")
        blobs[kind] = (blob_magic, data[offset:offset + blob_length])
    require(0 in blobs and 0x10000 in blobs, "missing signed CodeDirectory")
    require(not any(0x1000 <= kind <= 0x1005 for kind in blobs), "alternate CodeDirectories unsupported")
    directory_magic, directory = blobs[0]
    cms_magic, cms = blobs[0x10000]
    require(directory_magic == 0xFADE0C02 and cms_magic == 0xFADE0B01, "invalid signature blob types")
    require(len(directory) >= 44, "truncated CodeDirectory")
    version, flags, hash_offset, ident_offset, special, slots, limit = struct.unpack_from(">7I", directory, 8)
    hash_size, hash_type, platform, page_power = struct.unpack_from("4B", directory, 36)
    require(0x20001 <= version <= 0x20600 and not flags & 2, "unsupported or ad-hoc signature")
    require(hash_size == 32 and hash_type == 2 and page_power <= 20, "SHA256 required")
    if version >= 0x20100:
        require(len(directory) >= 48 and struct.unpack_from(">I", directory, 44)[0] == 0, "scatter unsupported")
    if version >= 0x20300:
        require(len(directory) >= 64, "truncated extended CodeDirectory")
        extended = struct.unpack_from(">Q", directory, 56)[0]
        limit = extended or limit
    require(limit == len(payload), "manifest length mismatch")
    page = (1 << page_power) if page_power else max(1, limit)
    require(slots == (limit + page - 1) // page, "invalid page count")
    require(hash_offset >= special * hash_size and hash_offset + slots * hash_size <= len(directory), "invalid hash table")
    require(0 <= ident_offset < len(directory) and b"\0" in directory[ident_offset:], "invalid identifier")
    with tempfile.TemporaryDirectory(prefix="verify-linux-signature-", dir=tmpdir) as folder:
        folder = Path(folder)
        (folder / "directory").write_bytes(directory)
        (folder / "signature.cms").write_bytes(cms[8:])
        result = subprocess.run([openssl, "cms", "-verify", "-binary", "-inform", "DER",
                                 "-in", str(folder / "signature.cms"), "-content", str(folder / "directory"),
                                 "-CAfile", str(certificate), "-purpose", "any", "-partial_chain",
                                 "-out", str(folder / "verified")], capture_output=True, text=True)
        require(result.returncode == 0, "CMS verification failed: " + result.stderr.strip())
        require((folder / "verified").read_bytes() == directory, "CMS content mismatch")
    for index in range(slots):
        expected = directory[hash_offset + index * hash_size:hash_offset + (index + 1) * hash_size]
        actual = hashlib.sha256(payload[index * page:min((index + 1) * page, limit)]).digest()
        require(hmac.compare_digest(actual, expected), "manifest page hash mismatch")
    root = manifest.parent.resolve()
    entries = 0
    seen = set()
    for line in payload.decode("utf-8").splitlines():
        require(len(line) > 66 and line[64:66] == "  ", "invalid SHA256 manifest entry")
        expected, name = line[:64], line[66:]
        relative = Path(name)
        require(not relative.is_absolute() and ".." not in relative.parts and name not in seen, "unsafe artifact path")
        seen.add(name)
        artifact = (root / relative).resolve()
        require(artifact.is_relative_to(root) and artifact.is_file(), "artifact outside signed directory")
        with artifact.open("rb") as source:
            actual = hashlib.file_digest(source, "sha256").hexdigest()
        require(hmac.compare_digest(actual, expected), "artifact SHA256 mismatch: " + name)
        entries += 1
    require(entries > 0, "empty artifact manifest")
    return entries


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("signature", type=Path)
    parser.add_argument("certificate", type=Path)
    parser.add_argument("--openssl", default="openssl")
    parser.add_argument("--tmpdir", type=Path, required=True)
    parser.add_argument("--certificate-sha256", required=True, help="Trusted DER certificate fingerprint")
    args = parser.parse_args()
    print("Verified signed artifacts:", verify(args.manifest, args.signature, args.certificate, args.openssl, args.tmpdir, args.certificate_sha256))
