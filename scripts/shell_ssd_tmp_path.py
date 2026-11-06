"""Keep shell-test temporary files on SSD through a whitespace-free workspace alias.

The upstream tests interpolate tmp_path into unquoted dot commands. Delegate the
original pytest fixture, including retention/cleanup, and return only its lexical
workspace alias. Test bodies and assertions are unchanged.
"""
import os
from pathlib import Path

import pytest
from _pytest.tmpdir import tmp_path as original_tmp_path


@pytest.fixture
def tmp_path(request, tmp_path_factory):
    alias = Path(os.environ['SHELL_TEST_TMP_ALIAS']).absolute()
    base = tmp_path_factory.getbasetemp()
    assert alias.resolve() == base
    assert not any(character.isspace() for character in str(alias))
    fixture = original_tmp_path.__wrapped__(request, tmp_path_factory)
    physical = next(fixture)
    try:
        yield alias / physical.relative_to(base)
    finally:
        next(fixture, None)
