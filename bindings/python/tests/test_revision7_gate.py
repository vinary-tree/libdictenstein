"""An old native symbol set must remain importable without revision-8 lookup."""

import libdictenstein as LD
import pytest


def test_revision7_loader_gates_optional_backend_symbols() -> None:
    assert LD.abi_version() == 1
    assert LD.api_revision() == 7
    with pytest.raises(LD.NativeError) as pathmap:
        LD.PathMap(LD.UnitDomain.BYTE)
    assert pathmap.value.status == 6
    with pytest.raises(LD.NativeError) as suffix:
        LD.SuffixIndex()
    assert suffix.value.status == 6
