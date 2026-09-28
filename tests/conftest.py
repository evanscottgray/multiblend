import pytest

import mb


def pytest_addoption(parser):
    parser.addoption(
        "--update-golden",
        action="store_true",
        help="regenerate tests/golden/*.npz from the binary under test (use the C++ reference build)",
    )


def pytest_configure(config):
    config.addinivalue_line(
        "markers",
        "reference_bug(reason, strict=True): documents a defect in the C++ reference. Expected "
        "to fail against the reference build; must pass against any other implementation. "
        "Use strict=False when the reference failure is nondeterministic (UB, races).",
    )


def pytest_collection_modifyitems(config, items):
    if not mb.IS_REFERENCE:
        return
    for item in items:
        marker = item.get_closest_marker("reference_bug")
        if not marker:
            # The reference's thread pool occasionally crashes or returns early
            # and corrupts output (FINDINGS.md: threadpool races). Rerun other
            # tests so those races don't masquerade as regressions.
            item.add_marker(pytest.mark.flaky(reruns=2))
        else:
            reason = marker.kwargs.get("reason") or (marker.args[0] if marker.args else "")
            strict = marker.kwargs.get("strict", True)
            item.add_marker(pytest.mark.xfail(reason=f"reference bug: {reason}", strict=strict))


@pytest.fixture(scope="session", autouse=True)
def _check_binary():
    if not mb.BIN.exists():
        pytest.exit(f"multiblend binary not found at {mb.BIN} (run `make` or set MULTIBLEND_BIN)", returncode=2)


@pytest.fixture
def work(tmp_path, monkeypatch):
    """Per-test working directory; multiblend is run with cwd set here."""
    monkeypatch.chdir(tmp_path)
    return tmp_path


@pytest.fixture
def update_golden(request):
    return request.config.getoption("--update-golden")
