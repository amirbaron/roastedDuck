"""Hash regular binary streams on the supported CPython 3.10+ test runtimes."""
import hashlib


def sha256_stream(stream):
    implementation = getattr(hashlib, "file_digest", None)
    if implementation is not None:
        return implementation(stream, "sha256").hexdigest()
    digest = hashlib.sha256()
    while block := stream.read(256 * 1024):
        digest.update(block)
    return digest.hexdigest()
