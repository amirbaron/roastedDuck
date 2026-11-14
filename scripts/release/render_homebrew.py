#!/usr/bin/env python3
"""Render the tap formula from qualified native SDK archive checksums."""
import argparse
from pathlib import Path
import re


def render(version, sums):
    if not re.fullmatch(r'1\.5\.6(?:\.post[1-9][0-9]*)?', version):
        raise ValueError('Expected a DuckDB 1.5.6-family release version')
    def archive(platform):
        name = f'roastedduck-{version}-{platform}.tar.gz'
        digest = sums[name]
        if not re.fullmatch('[0-9a-f]{64}', digest):
            raise ValueError('Invalid SHA-256: ' + name)
        return f'https://github.com/amirbaron/roastedDuck/releases/download/v{version}/{name}', digest
    mac, arm, intel = (archive(p) for p in ('macos-arm64', 'linux-arm64', 'linux-amd64'))
    return f'''require "shellwords"

class Roastedduck < Formula
  desc "Analytical SQL engine in Rust with bundled compatible FTS and VSS"
  homepage "https://amirbaron.github.io/roastedDuck/"
  version "{version}"
  license "MIT"

  on_macos do
    depends_on arch: :arm64
    depends_on macos: :big_sur
    url "{mac[0]}"
    sha256 "{mac[1]}"
  end

  on_linux do
    on_arm do
      url "{arm[0]}"
      sha256 "{arm[1]}"
    end
    on_intel do
      url "{intel[0]}"
      sha256 "{intel[1]}"
    end
  end

  def install
    bin.install "bin/roastedDuck" => "roastedduck"
    bin.install_symlink "roastedduck" => "roastedDuck" unless (bin/"roastedDuck").exist?
    pkgshare.install "notices", "LICENSE", "THIRD_PARTY_NOTICES.md"
  end

  def caveats
    <<~EOS
      FTS and VSS are bundled. Start with LOAD fts; and LOAD vss;.
      HNSW indexes currently require an in-memory database.
      This preview targets DuckDB 1.5.6. It is independently maintained.
    EOS
  end

  test do
    sql = <<~SQL
      SET custom_extension_repository='http://127.0.0.1:1';
      SET extension_directory='#{{testpath}}/extensions';
      INSTALL fts;
      INSTALL vss;
      LOAD fts;
      LOAD vss;
      CREATE TABLE docs(id INTEGER, body VARCHAR);
      INSERT INTO docs VALUES (1,'red fox'),(2,'blue bird');
      PRAGMA create_fts_index('docs','id','body');
      SELECT CASE WHEN count(*)=1 THEN 'fts-ok' ELSE error('FTS failure') END FROM docs WHERE fts_main_docs.match_bm25(id,'fox') IS NOT NULL;
      CREATE TABLE vectors(id INTEGER,v FLOAT[3]);
      INSERT INTO vectors VALUES (1,[1,0,0]),(2,[0,1,0]);
      CREATE INDEX h ON vectors USING HNSW(v);
      SELECT CASE WHEN id=1 THEN 'vss-ok' ELSE error('VSS failure') END FROM (SELECT id FROM vectors ORDER BY array_distance(v,[1,0,0]::FLOAT[3]) LIMIT 1);
      EXPLAIN SELECT id FROM vectors ORDER BY array_distance(v,[1,0,0]::FLOAT[3]) LIMIT 1;
    SQL
    output = pipe_output("#{{bin}}/roastedduck".shellescape + " -batch -bail -init /dev/null :memory:", sql, 0)
    assert_match "fts-ok", output
    assert_match "vss-ok", output
    assert_match "HNSW_INDEX_SCAN", output
  end
end
'''


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--checksums', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    sums = {}
    for line in args.checksums.read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        name = name.lstrip('*')
        if name in sums:
            raise ValueError('Duplicate checksum filename: ' + name)
        sums[name] = digest
    formula = render(args.version, sums)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open('x') as output:
        output.write(formula)
