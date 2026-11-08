# Preview packaging

The first preview repackages an already qualified native candidate. It does
not silently rebuild the engine or treat packaging checks as a full corpus run.

```sh
python3 scripts/release/collect_notices.py --out /external/notices
python3 scripts/release/package_preview.py \
  --payload /external/verified-candidate/payload \
  --dependency-notices /external/notices \
  --out /external/new-release-directory
python3 scripts/release/check_wheels.py /external/new-release-directory
python3 scripts/release/verify_downloads.py /external/new-release-directory
```

The packager requires the pinned candidate manifest, verifies its input hashes
and symlinks, preserves native bytes, and produces separate platform archives.
It deliberately refuses to overwrite a nonempty output directory. The frozen
candidate is a maintainer input, not a dependency needed to build from source.

Python distributions change from `duckdb==1.5.6` to `roastedduck==0.1.0a1`.
The version helper, metadata, license additions, and RECORD are updated; all
native entries are checked byte-for-byte. Install each target wheel on its
matching native environment, then run `smoke_python.py`. Run the native CLI
from an extracted archive and test selected Go/Swift consumers before publishing.

`verify_downloads.py` verifies a complete release directory or a downloaded
subset against `SHA256SUMS`. Preserve release assets after publishing; fix
problems in a new version with new hashes.

The current historical data in `website/docs/evidence/` is a retained measurement
snapshot. A future engine rebuild needs new affected qualification results,
provenance, and signatures as appropriate. Editing a status document does not
qualify a new binary.
