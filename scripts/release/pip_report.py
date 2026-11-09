"""Read SHA-256 from supported pip installation-report formats."""
import re


def archive_sha256(archive_info):
    """Require one consistent SHA-256 identity, including pip 23's legacy form."""
    candidates = []
    hashes = archive_info.get("hashes", {})
    if "sha256" in hashes:
        candidates.append(hashes["sha256"])
    legacy = archive_info.get("hash")
    if legacy is not None:
        algorithm, separator, digest = legacy.partition("=")
        assert separator and algorithm == "sha256", "Unsupported legacy archive digest"
        candidates.append(digest)
    assert candidates, "Installation report has no SHA-256 archive identity"
    assert all(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) for value in candidates), "Invalid SHA-256 archive identity"
    assert len(set(candidates)) == 1, "Conflicting installation-report archive digests"
    return candidates[0]
