"""Tests for plugins/__init__.py — namespace isolation for sibling
modules and startup-time collision detection (slopsmith#33).

The plugin loader used to insert each plugin directory onto `sys.path`,
which made bare `import sibling` fall through Python's per-name cache
in `sys.modules`. Two plugins shipping a same-named top-level module
(`extractor.py`, `util.py`, …) would step on each other. The loader
now exposes `context['load_sibling'](name)` that loads the sibling
under a namespaced module name `plugin_<plugin_id>.<name>` (with `.`
in plugin_id bijectively encoded — `_` -> `_5f_`, `.` -> `_2e_`),
plus a warning at startup so
existing colliding plugins are visible.
"""

import importlib
import json
import sys

import pytest


# Bare module names that this test module pre-populates into
# sys.modules to simulate the bare-import path. Saved/restored by
# the reset_plugin_state fixture so they don't leak to other test
# files. Codex / Copilot review on PR for slopsmith#33.
_BARE_NAMES_USED = ("util", "extractor")


@pytest.fixture()
def reset_plugin_state(monkeypatch):
    """Clear loader module-level state and restore on teardown.

    Saves and restores:
      * `plugins.LOADED_PLUGINS`
      * any `plugin_*` keys we add to `sys.modules`
      * the bare names this module simulates (`util`, `extractor`)
      * `sys.path` — `plugins.load_plugins()` mutates it
    Also unsets `SLOPSMITH_PLUGINS_DIR` for the test's duration
    (via monkeypatch) so a CI env that pre-sets it can't leak
    real user plugins into a tmp_path-driven test. Per-module
    locks are owned by the standard import system
    (`importlib._bootstrap._module_locks`) and are not our
    responsibility to reset.
    """
    monkeypatch.delenv("SLOPSMITH_PLUGINS_DIR", raising=False)
    plugins = importlib.import_module("plugins")
    saved_loaded = list(plugins.LOADED_PLUGINS)
    saved_modules = {k: v for k, v in sys.modules.items() if k.startswith("plugin_")}
    saved_bare = {k: sys.modules[k] for k in _BARE_NAMES_USED if k in sys.modules}
    saved_path = list(sys.path)
    plugins.LOADED_PLUGINS.clear()
    for k in list(sys.modules):
        if k.startswith("plugin_") or k in _BARE_NAMES_USED:
            del sys.modules[k]
    try:
        yield plugins
    finally:
        plugins.LOADED_PLUGINS.clear()
        plugins.LOADED_PLUGINS.extend(saved_loaded)
        for k in list(sys.modules):
            if k.startswith("plugin_") or k in _BARE_NAMES_USED:
                del sys.modules[k]
        sys.modules.update(saved_modules)
        sys.modules.update(saved_bare)
        sys.path[:] = saved_path


def _make_plugin(plugin_root, plugin_id, *, sibling_files=None, routes_body=None):
    """Create a minimal plugin directory under `plugin_root`.

    `sibling_files` is a dict of `{module_name: file_body}` written as
    `{module_name}.py` next to routes. `routes_body` is the contents of
    routes.py — defaults to a no-op `setup` so the plugin loads cleanly.
    """
    plugin_dir = plugin_root / plugin_id
    plugin_dir.mkdir(parents=True)
    (plugin_dir / "plugin.json").write_text(
        json.dumps({"id": plugin_id, "name": plugin_id, "routes": "routes.py"})
    )
    (plugin_dir / "routes.py").write_text(
        routes_body if routes_body is not None else "def setup(app, ctx):\n    pass\n"
    )
    for name, body in (sibling_files or {}).items():
        (plugin_dir / f"{name}.py").write_text(body)
    return plugin_dir


def _run_load_plugins(plugins, app, tmp_path, context=None):
    """Drive load_plugins against a tmp plugin root, restoring module
    state on the way out so each test is isolated."""
    saved_dir = plugins.PLUGINS_DIR
    plugins.PLUGINS_DIR = tmp_path
    try:
        plugins.load_plugins(app, context if context is not None else {})
    finally:
        plugins.PLUGINS_DIR = saved_dir


def test_load_sibling_returns_per_plugin_namespaced_modules(tmp_path, reset_plugin_state):
    """Two plugins shipping `extractor.py` with different exports must
    each see their OWN file via load_sibling — no cross-contamination."""
    plugins = reset_plugin_state
    _make_plugin(
        tmp_path, "alpha",
        sibling_files={"extractor": "MANIFEST_DIR = 'alpha-manifest'\n"},
        routes_body=(
            "def setup(app, ctx):\n"
            "    extractor = ctx['load_sibling']('extractor')\n"
            "    app.state.alpha_manifest = extractor.MANIFEST_DIR\n"
        ),
    )
    _make_plugin(
        tmp_path, "beta",
        sibling_files={"extractor": "BETA_VALUE = 42\n"},
        routes_body=(
            "def setup(app, ctx):\n"
            "    extractor = ctx['load_sibling']('extractor')\n"
            "    app.state.beta_value = extractor.BETA_VALUE\n"
        ),
    )
    fake_app = type("FakeApp", (), {})()
    fake_app.state = type("State", (), {})()
    _run_load_plugins(plugins, fake_app, tmp_path)
    assert fake_app.state.alpha_manifest == "alpha-manifest"
    assert fake_app.state.beta_value == 42
    # The two extractors are namespaced into distinct sys.modules
    # entries. `.` separates id and name to disambiguate when either
    # contains underscores.
    alpha_mod = sys.modules["plugin_alpha.extractor"]
    beta_mod = sys.modules["plugin_beta.extractor"]
    assert alpha_mod is not beta_mod
    assert getattr(alpha_mod, "MANIFEST_DIR", None) == "alpha-manifest"
    assert getattr(beta_mod, "BETA_VALUE", None) == 42
    # Negative cross-check: alpha's extractor must NOT carry beta's exports.
    assert not hasattr(alpha_mod, "BETA_VALUE")
    assert not hasattr(beta_mod, "MANIFEST_DIR")


def test_load_sibling_caches_repeat_calls(tmp_path, reset_plugin_state):
    """Two `load_sibling('util')` calls within the same plugin return
    the identical module object — no double exec_module."""
    plugins = reset_plugin_state
    _make_plugin(
        tmp_path, "cached",
        sibling_files={"util": "INSTANCE = object()\n"},
        routes_body=(
            "def setup(app, ctx):\n"
            "    a = ctx['load_sibling']('util')\n"
            "    b = ctx['load_sibling']('util')\n"
            "    app.state.same = a is b\n"
            "    app.state.instance = a.INSTANCE\n"
        ),
    )
    fake_app = type("FakeApp", (), {})()
    fake_app.state = type("State", (), {})()
    _run_load_plugins(plugins, fake_app, tmp_path)
    assert fake_app.state.same is True
    assert fake_app.state.instance is sys.modules["plugin_cached.util"].INSTANCE


def test_load_sibling_missing_module_raises_import_error(tmp_path, reset_plugin_state):
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "bare")
    with pytest.raises(ImportError):
        plugins._load_plugin_sibling("bare", plugin_dir, "does_not_exist")


def test_load_sibling_rejects_traversal_and_suffix(tmp_path, reset_plugin_state):
    """The helper takes a bare module name; reject anything that could
    traverse paths or carry a redundant .py suffix."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "p")
    # Reject:
    # - empty / non-string (bare module name required)
    # - path traversal (`/`, `\`, `../`)
    # - redundant `.py` suffix
    # - any `.` (used as separator in the cache key — would
    #   otherwise allow ambiguous keys)
    for bad in ("", "../etc", "sub/util", "util.py", "pkg.helper", 123, None):
        with pytest.raises((ValueError, TypeError)):
            plugins._load_plugin_sibling("p", plugin_dir, bad)


def test_collision_warning_fires_for_shared_module_name(tmp_path, reset_plugin_state, capsys):
    """Two plugins both shipping extractor.py must trigger the warning."""
    plugins = reset_plugin_state
    _make_plugin(tmp_path, "rs1extract", sibling_files={"extractor": "X = 1\n"})
    _make_plugin(tmp_path, "discextract", sibling_files={"extractor": "Y = 2\n"})
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    assert "Module-name collision warning" in out
    assert "'extractor' (module)" in out
    assert "rs1extract" in out
    assert "discextract" in out


def test_collision_warning_silent_when_names_unique(tmp_path, reset_plugin_state, capsys):
    plugins = reset_plugin_state
    _make_plugin(tmp_path, "alpha", sibling_files={"alpha_helper": "A = 1\n"})
    _make_plugin(tmp_path, "beta", sibling_files={"beta_helper": "B = 2\n"})
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    assert "Module-name collision warning" not in out


def test_collision_warning_excludes_routes_and_dunders(tmp_path, reset_plugin_state, capsys):
    """routes.py is already namespaced by the loader; __init__.py
    belongs to a plugin that opted into being a package and namespaces
    itself. Neither should trip the collision warning even when both
    plugins ship one."""
    plugins = reset_plugin_state
    p1 = _make_plugin(tmp_path, "one", sibling_files={"unique_one": "V = 1\n"})
    p2 = _make_plugin(tmp_path, "two", sibling_files={"unique_two": "V = 2\n"})
    (p1 / "__init__.py").write_text("")
    (p2 / "__init__.py").write_text("")
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    assert "Module-name collision warning" not in out


def test_collision_warning_dedupes_per_plugin(tmp_path, reset_plugin_state, capsys):
    """A single plugin shipping BOTH `extractor.py` and
    `extractor/__init__.py` is a supported intra-plugin layout
    (load_sibling deterministically prefers the package form,
    matching CPython's import precedence). The warning must NOT
    count it as a 2-plugin collision and emit a bogus message
    listing the same plugin id twice. Codex round 5."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "lonely")
    (plugin_dir / "extractor.py").write_text("FROM = 'file'\n")
    pkg_dir = plugin_dir / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("FROM = 'package'\n")
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    # Only one plugin is involved, so no cross-plugin warning fires.
    assert "Module-name collision warning" not in out


def test_collision_warning_still_fires_when_two_plugins_each_have_both_forms(
    tmp_path, reset_plugin_state, capsys
):
    """Two plugins each shipping both forms of `extractor` IS a
    real cross-plugin collision and must be reported. Codex round 5
    sanity check on the dedup logic."""
    plugins = reset_plugin_state
    for pid in ("alpha", "beta"):
        plugin_dir = _make_plugin(tmp_path, pid)
        (plugin_dir / "extractor.py").write_text(f"OWNER = '{pid}-file'\n")
        pkg_dir = plugin_dir / "extractor"
        pkg_dir.mkdir()
        (pkg_dir / "__init__.py").write_text(f"OWNER = '{pid}-package'\n")
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    warning_lines = [ln for ln in out.splitlines() if "Module-name collision warning" in ln]
    assert len(warning_lines) == 1
    warning = warning_lines[0]
    assert "alpha" in warning
    assert "beta" in warning
    # The warning text should list each plugin id ONCE, even though
    # both plugins ship two forms of `extractor`.
    assert warning.count("'alpha'") == 1
    assert warning.count("'beta'") == 1
    # Both forms reported in the kind label.
    assert "module/package" in warning


def test_load_sibling_does_not_alias_bare_imported_package(tmp_path, reset_plugin_state):
    """A bare-imported package keeps `__package__` and
    `__spec__.name` as the un-namespaced bare name, so lazy
    relative imports inside it would still resolve through the
    global cache. To avoid that, load_sibling does NOT reuse a
    bare-imported package — it re-executes under the namespaced
    spec instead. Two copies of the package coexist (one bare,
    one namespaced); module-level state diverges. This is
    documented as the trade-off; the alternative would silently
    leak submodule cross-loads. Codex round 5."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "pkgsafe")
    pkg_dir = plugin_dir / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("MARK = object()\n")
    (pkg_dir / "child.py").write_text("FROM = 'leaf'\n")
    # Pre-populate sys.modules['extractor'] as if a bare import had
    # already pulled in the package.
    spec = importlib.util.spec_from_file_location(
        "extractor",
        str(pkg_dir / "__init__.py"),
        submodule_search_locations=[str(pkg_dir)],
    )
    bare_pkg = importlib.util.module_from_spec(spec)
    sys.modules["extractor"] = bare_pkg
    spec.loader.exec_module(bare_pkg)
    bare_mark = bare_pkg.MARK
    # load_sibling re-executes under the namespaced spec rather
    # than aliasing the bare package.
    via_helper = plugins._load_plugin_sibling("pkgsafe", plugin_dir, "extractor")
    assert via_helper is not bare_pkg
    # Different MARK objects confirm the namespaced version was
    # actually re-executed.
    assert via_helper.MARK is not bare_mark
    # The namespaced submodule resolves through the namespaced
    # package, NOT through `extractor.child`.
    child = importlib.import_module("plugin_pkgsafe.extractor.child")
    assert child.FROM == "leaf"


def test_load_sibling_handles_dotted_plugin_id_via_escape(tmp_path, reset_plugin_state):
    """Plugins with reverse-DNS-style ids (`foo.bar`) must still be
    able to use load_sibling — the helper escapes `.` in the
    plugin_id portion of the cache key so the synthetic parent
    package is still well-formed. Spotted across codex review
    rounds on PR for slopsmith#33."""
    plugins = reset_plugin_state
    plugin_dir = tmp_path / "rdns"
    plugin_dir.mkdir()
    (plugin_dir / "util.py").write_text("VALUE = 'reverse-dns'\n")
    util = plugins._load_plugin_sibling("com.example.foo", plugin_dir, "util")
    assert util.VALUE == "reverse-dns"
    # The cache key uses the bijectively-encoded form so it doesn't
    # fight with Python's package resolution. `.` -> `_2e_`.
    assert "plugin_com_2e_example_2e_foo.util" in sys.modules
    assert sys.modules["plugin_com_2e_example_2e_foo.util"] is util


def test_load_sibling_rejects_empty_plugin_id(tmp_path, reset_plugin_state):
    """Empty / non-string plugin_id is still rejected — the helper
    needs SOMETHING to namespace under."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "valid_id", sibling_files={"util": "X = 1\n"})
    for bad in ("", None, 123):
        with pytest.raises((ValueError, TypeError)):
            plugins._load_plugin_sibling(bad, plugin_dir, "util")


def test_load_sibling_exposes_child_as_parent_attribute(tmp_path, reset_plugin_state):
    """After load_sibling caches a child, Python's package-style
    relative imports (`from . import sibling`, `from .. import
    sibling`) need to find the child as an ATTRIBUTE on the parent
    package — not just in sys.modules. The standard import
    machinery sets that attribute; load_sibling must mimic the
    behavior. Codex round 9."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "expose")
    (plugin_dir / "extractor.py").write_text("VAL = 'extr'\n")
    # Another sibling does `from . import extractor` — pure
    # attribute lookup on the synthetic parent.
    (plugin_dir / "consumer.py").write_text(
        "from . import extractor\n"
        "GOT = extractor.VAL\n"
    )
    # Load the consumer first; while it's executing, the
    # `from . import extractor` triggers extractor's import
    # through the parent package's __path__. After it loads,
    # extractor must be visible as an attribute on the parent.
    consumer = plugins._load_plugin_sibling("expose", plugin_dir, "consumer")
    assert consumer.GOT == "extr"
    parent = sys.modules["plugin_expose"]
    assert hasattr(parent, "extractor")
    assert parent.extractor is sys.modules["plugin_expose.extractor"]
    # And consumer is exposed on the parent the same way.
    assert hasattr(parent, "consumer")
    assert parent.consumer is consumer


def test_load_sibling_supports_relative_imports_between_siblings(tmp_path, reset_plugin_state):
    """A sibling loaded via load_sibling that does `from .shared
    import X` (relative import to another top-level sibling) must
    resolve. The synthetic parent's __path__ points at the plugin
    directory so the import machinery can find sibling files via
    the standard relative-import path. Codex round 7."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "rel")
    (plugin_dir / "shared.py").write_text("SHARED_VALUE = 'shared'\n")
    (plugin_dir / "extractor.py").write_text(
        "from .shared import SHARED_VALUE\n"
        "RE_EXPORT = SHARED_VALUE\n"
    )
    extractor = plugins._load_plugin_sibling("rel", plugin_dir, "extractor")
    assert extractor.RE_EXPORT == "shared"
    # The relatively-imported sibling is registered under the
    # namespaced key, NOT polluted into the global `shared` slot
    # (collision risk with other plugins' `shared.py`).
    assert "plugin_rel.shared" in sys.modules


def test_load_sibling_package_relative_import_to_outside_sibling(tmp_path, reset_plugin_state):
    """A package-form sibling whose __init__.py does
    `from ..shared import X` reaches the parent and finds another
    sibling. Verifies the package + parent-__path__ wiring works
    end-to-end. Codex round 7."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "pkgrel")
    (plugin_dir / "shared.py").write_text("VAL = 42\n")
    pkg_dir = plugin_dir / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text(
        "from ..shared import VAL\n"
        "VALUE = VAL\n"
    )
    extractor = plugins._load_plugin_sibling("pkgrel", plugin_dir, "extractor")
    assert extractor.VALUE == 42


def test_load_sibling_package_relative_import_works(tmp_path, reset_plugin_state):
    """A package-form sibling whose __init__.py uses `from .child
    import X` must load. Without registering the synthetic parent
    package `plugin_<id>` in sys.modules first, Python can't resolve
    the relative import. Codex round 3 caught this."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "relpkg")
    pkg_dir = plugin_dir / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "child.py").write_text("CHILD_VALUE = 99\n")
    (pkg_dir / "__init__.py").write_text(
        "from .child import CHILD_VALUE\n"
        "RE_EXPORT = CHILD_VALUE\n"
    )
    extractor = plugins._load_plugin_sibling("relpkg", plugin_dir, "extractor")
    assert extractor.RE_EXPORT == 99
    # Parent package was registered as a synthetic ModuleType.
    assert "plugin_relpkg" in sys.modules


def test_load_sibling_does_not_alias_bare_imported_file_module(tmp_path, reset_plugin_state):
    """Mixed migration: bare `import util` already cached this
    plugin's util.py under the global `util` name. load_sibling
    does NOT alias the bare module into the namespaced cache —
    it re-executes under the namespaced spec. The bare module's
    `__package__` / `__name__` / `__spec__` would otherwise stay
    set to the un-namespaced bare name, and any later relative
    import inside util.py (`from .shared import X` in a function
    body) would route through the bare global cache, undoing the
    isolation. Trade-off: module-level state in util splits across
    two copies until the plugin removes its bare imports. Spotted
    by codex review on PR for slopsmith#33 round 8."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "mixmig")
    util_path = plugin_dir / "util.py"
    util_path.write_text("MARK = object()\n")
    spec = importlib.util.spec_from_file_location("util", str(util_path))
    bare_mod = importlib.util.module_from_spec(spec)
    sys.modules["util"] = bare_mod
    spec.loader.exec_module(bare_mod)
    bare_mark = bare_mod.MARK
    via_helper = plugins._load_plugin_sibling("mixmig", plugin_dir, "util")
    # Different module objects — the helper re-executed instead
    # of aliasing.
    assert via_helper is not bare_mod
    assert via_helper.MARK is not bare_mark
    # Namespaced key has the namespaced object; bare key still has
    # the bare-imported object.
    assert sys.modules["plugin_mixmig.util"] is via_helper
    assert sys.modules["util"] is bare_mod
    # Critically, via_helper has the correct namespaced metadata so
    # later relative imports inside it would route through the
    # synthetic parent.
    assert via_helper.__name__ == "plugin_mixmig.util"
    assert via_helper.__package__ == "plugin_mixmig"


def test_safe_plugin_id_encoding_is_collision_free(reset_plugin_state):
    """Distinct plugin_ids must always map to distinct encoded
    forms. The previous `.` -> `_x2e_` (only when `.` was present)
    was not bijective: ids `foo.bar` and `foo_x2e_bar` both produced
    `foo_x2e_bar`. With the bijective `_` -> `_5f_`, `.` -> `_2e_`
    encoding (in that order), no two distinct plugin_ids map to the
    same output. Copilot review on PR #105 round 3."""
    plugins = reset_plugin_state
    samples = [
        "foo",
        "foo_bar",
        "foo.bar",
        "foo_2e_bar",
        "foo_5f_bar",
        "foo_5f_2e_5f_bar",
        "com.example.foo",
        "com_example_foo",
        "com_2e_example_2e_foo",
        "",  # empty edge — empty maps to empty, distinct from all others
        "_",
        ".",
        "._",
        "_.",
    ]
    encoded = [plugins._safe_plugin_id_for_module_name(s) for s in samples]
    # Bijective: distinct inputs -> distinct outputs.
    assert len(set(encoded)) == len(samples), dict(zip(samples, encoded))


def test_load_plugins_skips_non_string_id(tmp_path, reset_plugin_state, capsys):
    """A malformed manifest with a non-string id (e.g. number) is
    skipped with a clear message rather than crashing later inside
    `_safe_plugin_id_for_module_name`'s `.replace()` call. Copilot
    review on PR #105 round 3."""
    plugins = reset_plugin_state
    bad_dir = tmp_path / "bad"
    bad_dir.mkdir()
    (bad_dir / "plugin.json").write_text('{"id": 42, "name": "bad"}')
    _make_plugin(tmp_path, "good", sibling_files={"util": "X = 1\n"})
    fake_app = type("FakeApp", (), {})()
    _run_load_plugins(plugins, fake_app, tmp_path)
    out = capsys.readouterr().out
    assert "must be a string" in out
    assert "int" in out  # type name surfaced
    loaded_ids = {p["id"] for p in plugins.LOADED_PLUGINS}
    assert 42 not in loaded_ids
    assert "good" in loaded_ids


def test_load_plugins_warns_on_falsy_non_string_id(tmp_path, reset_plugin_state, capsys):
    """`{"id": 0}` and `{"id": []}` are falsy non-strings. The
    type-check must run BEFORE the falsy-empty check so the user
    gets the explicit "must be a string" warning instead of a
    silent skip. Copilot review on PR #105 round 4."""
    plugins = reset_plugin_state
    for i, bad_value in enumerate(("0", "[]", "false")):  # JSON literals
        bad_dir = tmp_path / f"bad{i}"
        bad_dir.mkdir()
        (bad_dir / "plugin.json").write_text(f'{{"id": {bad_value}, "name": "x"}}')
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    # Each malformed manifest produces a "must be a string" warning;
    # none are silently dropped.
    assert out.count("must be a string") == 3
    assert "int" in out  # for {"id": 0}
    assert "list" in out  # for {"id": []}
    assert "bool" in out  # for {"id": false}


def test_load_plugins_escapes_dotted_id_in_routes_module_name(tmp_path, reset_plugin_state):
    """A plugin with a reverse-DNS id like `com.example.foo` must
    have its routes module registered under a `.`-free name, or
    Python would treat the cache key as a dotted package path and
    set `__package__` to an unintended parent (relative imports in
    routes.py would then resolve against something else entirely).
    The same `.` -> `_2e_` encoding used by load_sibling now
    applies to routes too. Copilot review on PR #105 round 2."""
    plugins = reset_plugin_state
    plugin_dir = tmp_path / "rdns_routes"
    plugin_dir.mkdir()
    (plugin_dir / "plugin.json").write_text(
        json.dumps({"id": "com.example.foo", "name": "rdns", "routes": "routes.py"})
    )
    (plugin_dir / "routes.py").write_text(
        "def setup(app, ctx):\n    app.state.routes_loaded = True\n"
    )
    fake_app = type("FakeApp", (), {})()
    fake_app.state = type("State", (), {})()
    _run_load_plugins(plugins, fake_app, tmp_path)
    assert fake_app.state.routes_loaded is True
    # Routes module is registered under the escaped name and is a
    # single identifier-shaped key — NOT a dotted path that Python
    # would try to resolve as a real package.
    assert "plugin_com_2e_example_2e_foo_routes" in sys.modules
    routes_mod = sys.modules["plugin_com_2e_example_2e_foo_routes"]
    # __package__ is empty (top-level module), not a dotted parent.
    assert (routes_mod.__package__ or "") == ""


def test_load_sibling_parent_registration_is_atomic(tmp_path, reset_plugin_state):
    """Two threads loading DIFFERENT siblings for the same plugin
    must agree on the synthetic parent. If they each constructed a
    fresh ModuleType and assigned to sys.modules[parent_name]
    without coordination, the second assignment could replace the
    first — and child attributes already attached to the
    first parent would disappear, breaking `from . import sibling`.
    setdefault makes the registration atomic. Copilot round 2."""
    import threading
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "atomic")
    # Two slow siblings so the threads have time to overlap.
    (plugin_dir / "alpha.py").write_text("import time\ntime.sleep(0.05)\nVALUE = 'a'\n")
    (plugin_dir / "beta.py").write_text("import time\ntime.sleep(0.05)\nVALUE = 'b'\n")
    errors: list = []

    def worker(name):
        try:
            plugins._load_plugin_sibling("atomic", plugin_dir, name)
        except BaseException as e:  # pragma: no cover
            errors.append(e)

    threads = [
        threading.Thread(target=worker, args=("alpha",)),
        threading.Thread(target=worker, args=("beta",)),
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors
    parent = sys.modules["plugin_atomic"]
    # Both children are exposed as attributes on the SAME parent —
    # neither was lost to a parent-replacement race.
    assert hasattr(parent, "alpha")
    assert hasattr(parent, "beta")
    assert parent.alpha.VALUE == "a"
    assert parent.beta.VALUE == "b"


def test_load_sibling_concurrent_first_call_returns_fully_initialized(tmp_path, reset_plugin_state):
    """Two threads racing on the same first-time load_sibling call
    should both receive a fully-initialized module object — neither
    can observe the half-built module that's briefly registered in
    sys.modules between `module_from_spec` and the end of
    `exec_module`. The per-module lock added in round 8 enforces
    this."""
    import threading
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "racy")
    # The sibling's __init__ does meaningful work BEFORE setting
    # `READY = True`, so a partially-initialized module would lack
    # the attribute even though the module object exists in
    # sys.modules.
    (plugin_dir / "slow.py").write_text(
        "import time\n"
        "time.sleep(0.05)\n"
        "READY = True\n"
        "VALUE = 'done'\n"
    )
    results: list = []
    errors: list = []

    def worker():
        try:
            mod = plugins._load_plugin_sibling("racy", plugin_dir, "slow")
            results.append((mod, getattr(mod, "READY", None), getattr(mod, "VALUE", None)))
        except BaseException as e:  # pragma: no cover - bug path
            errors.append(e)

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors
    assert len(results) == 8
    # Every caller sees the same fully-initialized module.
    first_mod, _, _ = results[0]
    for mod, ready, value in results:
        assert mod is first_mod
        assert ready is True
        assert value == "done"


def test_load_sibling_does_not_reuse_other_plugins_bare_import(tmp_path, reset_plugin_state):
    """If sys.path has already cached a util.py from PLUGIN A under
    the bare name `util`, plugin B's load_sibling('util') must NOT
    return plugin A's module — it has to load plugin B's own copy
    under the namespaced key. This is the whole point of the
    isolation fix; the reuse path can't accidentally undo it."""
    plugins = reset_plugin_state
    plugin_a = _make_plugin(tmp_path, "plug_a")
    plugin_b = _make_plugin(tmp_path, "plug_b")
    (plugin_a / "util.py").write_text("OWNER = 'a'\n")
    (plugin_b / "util.py").write_text("OWNER = 'b'\n")
    # Simulate plugin A's bare import landing in sys.modules['util'].
    spec_a = importlib.util.spec_from_file_location("util", str(plugin_a / "util.py"))
    bare_a = importlib.util.module_from_spec(spec_a)
    sys.modules["util"] = bare_a
    spec_a.loader.exec_module(bare_a)
    assert bare_a.OWNER == "a"
    # Plugin B's load_sibling must give plugin B's util, NOT plugin A's.
    b_util = plugins._load_plugin_sibling("plug_b", plugin_b, "util")
    assert b_util is not bare_a
    assert b_util.OWNER == "b"


def test_load_sibling_loads_package_form(tmp_path, reset_plugin_state):
    """A plugin shipping a sibling as a package directory
    (`extractor/__init__.py`) should be loadable through
    load_sibling exactly like a single-file `.py` sibling. The
    collision-warning scanner directs maintainers of package-form
    plugins toward load_sibling, so the helper has to actually
    support them. Codex review on PR for slopsmith#33."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "pkgplugin")
    pkg_dir = plugin_dir / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("ROOT_VALUE = 7\n")
    (pkg_dir / "child.py").write_text("CHILD_VALUE = 8\n")
    extractor = plugins._load_plugin_sibling("pkgplugin", plugin_dir, "extractor")
    assert extractor.ROOT_VALUE == 7
    # Submodule lookup works because spec carried submodule_search_locations.
    child = importlib.import_module("plugin_pkgplugin.extractor.child")
    assert child.CHILD_VALUE == 8


def test_load_sibling_prefers_package_over_file_when_both_exist(tmp_path, reset_plugin_state):
    """If a plugin ships BOTH `extractor.py` and `extractor/__init__.py`
    in the same directory, the package form wins — matches CPython's
    own import-resolution precedence so bare `import extractor` and
    `load_sibling('extractor')` always run the same code path.
    Spotted by codex review on PR for slopsmith#33."""
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "both")
    (plugin_dir / "extractor.py").write_text("FROM = 'file'\n")
    pkg_dir = plugin_dir / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("FROM = 'package'\n")
    extractor = plugins._load_plugin_sibling("both", plugin_dir, "extractor")
    assert extractor.FROM == "package"


def test_load_sibling_missing_in_both_forms_raises_with_useful_message(tmp_path, reset_plugin_state):
    plugins = reset_plugin_state
    plugin_dir = _make_plugin(tmp_path, "empty")
    with pytest.raises(ImportError) as exc:
        plugins._load_plugin_sibling("empty", plugin_dir, "missing")
    msg = str(exc.value)
    # Error message should mention BOTH probed locations so a
    # confused author sees "I checked here AND here" not "I checked
    # only the .py form".
    assert "missing.py" in msg
    assert "missing" in msg and "__init__.py" in msg


def test_load_sibling_disambiguates_underscored_ids_and_names(tmp_path, reset_plugin_state):
    """`(plugin_id='a_b', name='c')` and `(plugin_id='a', name='b_c')`
    must NOT collide in sys.modules. The `.` separator + bijective
    `_` -> `_5f_` encoding of plugin_id make the cache key
    unambiguous (the old `_` separator collapsed both to
    `plugin_a_b_c`). Codex review on PR for slopsmith#33."""
    plugins = reset_plugin_state
    p1 = _make_plugin(tmp_path, "a_b", sibling_files={"c": "WHO = 'a_b/c'\n"})
    p2 = _make_plugin(tmp_path, "a", sibling_files={"b_c": "WHO = 'a/b_c'\n"})
    m1 = plugins._load_plugin_sibling("a_b", p1, "c")
    m2 = plugins._load_plugin_sibling("a", p2, "b_c")
    assert m1 is not m2
    assert m1.WHO == "a_b/c"
    assert m2.WHO == "a/b_c"
    # Both keys exist independently in sys.modules. plugin_id `a_b`
    # encodes to `a_5f_b` so the parent is `plugin_a_5f_b`. The
    # NAME portion is not encoded (it's only the plugin_id that
    # could be confused with the `.` separator). So:
    #   id='a_b', name='c'   -> plugin_a_5f_b.c
    #   id='a',   name='b_c' -> plugin_a.b_c
    # The old `_` separator collapsed both to `plugin_a_b_c`.
    assert "plugin_a_5f_b.c" in sys.modules
    assert "plugin_a.b_c" in sys.modules
    assert sys.modules["plugin_a_5f_b.c"] is m1
    assert sys.modules["plugin_a.b_c"] is m2


def test_collision_warning_detects_package_form(tmp_path, reset_plugin_state, capsys):
    """A plugin shipping `extractor/__init__.py` collides with another
    plugin's `extractor.py` the same way two `.py` files would. The
    scanner picks up packages too. Codex review on PR for slopsmith#33."""
    plugins = reset_plugin_state
    # Plugin one: extractor.py
    _make_plugin(tmp_path, "as_module", sibling_files={"extractor": "X = 1\n"})
    # Plugin two: extractor/ (package form)
    plugin_pkg = _make_plugin(tmp_path, "as_package")
    pkg_dir = plugin_pkg / "extractor"
    pkg_dir.mkdir()
    (pkg_dir / "__init__.py").write_text("Y = 2\n")
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    assert "Module-name collision warning" in out
    assert "extractor" in out
    assert "as_module" in out
    assert "as_package" in out
    # The mixed-form label should also appear so the maintainer knows
    # to look for both shapes.
    assert "module/package" in out


def test_collision_warning_detects_two_packages(tmp_path, reset_plugin_state, capsys):
    """Two plugins each shipping the SAME package directory form."""
    plugins = reset_plugin_state
    for pid in ("plug_a", "plug_b"):
        plugin_dir = _make_plugin(tmp_path, pid)
        pkg = plugin_dir / "shared_pkg"
        pkg.mkdir()
        (pkg / "__init__.py").write_text(f"# {pid}\n")
    _run_load_plugins(plugins, type("FakeApp", (), {})(), tmp_path)
    out = capsys.readouterr().out
    assert "Module-name collision warning" in out
    assert "shared_pkg" in out
    assert "plug_a" in out
    assert "plug_b" in out


def test_per_plugin_context_does_not_leak_load_sibling_across_plugins(tmp_path, reset_plugin_state):
    """Plugin A's `load_sibling` must close over plugin A's id+dir.
    If both plugins received the SAME closure (the bug we are
    preventing), plugin A calling `load_sibling('thing')` would
    load whatever the loop's last-iteration closure pointed at —
    typically the alphabetically-last plugin's directory."""
    plugins = reset_plugin_state
    _make_plugin(
        tmp_path, "aaa",
        sibling_files={"thing": "ORIGIN = 'aaa'\n"},
        routes_body=(
            "def setup(app, ctx):\n"
            "    app.state.aaa_origin = ctx['load_sibling']('thing').ORIGIN\n"
        ),
    )
    _make_plugin(
        tmp_path, "zzz",
        sibling_files={"thing": "ORIGIN = 'zzz'\n"},
        routes_body=(
            "def setup(app, ctx):\n"
            "    app.state.zzz_origin = ctx['load_sibling']('thing').ORIGIN\n"
        ),
    )
    fake_app = type("FakeApp", (), {})()
    fake_app.state = type("State", (), {})()
    _run_load_plugins(plugins, fake_app, tmp_path)
    assert fake_app.state.aaa_origin == "aaa"
    assert fake_app.state.zzz_origin == "zzz"
