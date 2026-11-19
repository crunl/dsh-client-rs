import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/runtime/package-runtime.py"


def load_package_runtime():
    spec = importlib.util.spec_from_file_location("package_runtime", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {SCRIPT}")
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


module = load_package_runtime()


class ExclusionTests(unittest.TestCase):
    def setUp(self):
        self.target = ("darwin", "arm64")

    def test_package_manager_metadata(self):
        for path in (
            "node_modules/.bin/node",
            "node_modules/a/node_modules/.pnpm/x",
            "node_modules/.modules.yaml",
            "node_modules/.pnpm-workspace-state-v1.json",
        ):
            with self.subTest(path=path):
                self.assertEqual(
                    module.exclusion_reason(path, self.target, "darwin-arm64"),
                    "package-manager metadata",
                )

    def test_source_maps_and_declarations(self):
        cases = {
            "node_modules/a/lib/index.js.map": "source map",
            "node_modules/a/lib/index.mjs.map": "source map",
            "node_modules/a/lib/index.cjs.map": "source map",
            "node_modules/a/lib/index.ts.map": "source map",
            "node_modules/a/lib/index.css.map": "source map",
            "node_modules/a/lib/index.d.ts": "TypeScript declaration",
            "node_modules/a/lib/index.d.mts": "TypeScript declaration",
            "node_modules/a/lib/index.d.cts": "TypeScript declaration",
            "node_modules/a/lib/tsconfig.tsbuildinfo": "TypeScript build cache",
        }
        for path, reason in cases.items():
            with self.subTest(path=path):
                self.assertEqual(
                    module.exclusion_reason(path, self.target, "darwin-arm64"), reason
                )

    def test_libreoffice_keeps_selected_engine(self):
        self.assertEqual(
            module.exclusion_reason(
                "node_modules/@deepseek-ai/libreoffice-kit-darwin-arm64/assets/x",
                self.target,
                "darwin-arm64",
            ),
            None,
        )
        for engine in ("wasm", "darwin-x64", "linux-x64-glibc"):
            with self.subTest(engine=engine):
                self.assertEqual(
                    module.exclusion_reason(
                        f"node_modules/@deepseek-ai/libreoffice-kit-{engine}/assets/x",
                        self.target,
                        "darwin-arm64",
                    ),
                    "LibreOffice other platform",
                )

    def test_fs_ext_outputs(self):
        cases = {
            "node_modules/fs-ext/build/Release/obj/a.o": "fs-ext compiler output",
            "node_modules/fs-ext/build/Debug/fs_ext.pdb": "fs-ext compiler output",
            "node_modules/fs-ext/build/Release/fs_ext.node": None,
            "node_modules/fs-ext/build/binding.sln": "fs-ext build configuration",
            "node_modules/fs-ext/build/config.gypi": "fs-ext build configuration",
            "node_modules/fs-ext/build/fs_ext.vcxproj": "fs-ext build configuration",
        }
        for path, reason in cases.items():
            with self.subTest(path=path):
                self.assertEqual(
                    module.exclusion_reason(path, self.target, "darwin-arm64"), reason
                )

    def test_domino_test_and_dev_tooling(self):
        cases = {
            "node_modules/@mixmark-io/domino/test/a.js": "Domino test fixtures",
            "node_modules/@mixmark-io/domino/test": "Domino test fixtures",
            "node_modules/@mixmark-io/domino/.yarn/a": "Domino development tooling",
            "node_modules/@mixmark-io/domino/lib/domino.js": None,
        }
        for path, reason in cases.items():
            with self.subTest(path=path):
                self.assertEqual(
                    module.exclusion_reason(path, self.target, "darwin-arm64"), reason
                )

    def test_node_pty_platform_rules(self):
        cases = {
            "node_modules/node-pty/third_party/conpty/x": "node-pty Windows ConPTY sources",
            "node_modules/node-pty/prebuilds/win32-x64/conpty.node": "node-pty other platform",
            "node_modules/node-pty/prebuilds/darwin-arm64/pty.node": None,
        }
        for path, reason in cases.items():
            with self.subTest(path=path):
                self.assertEqual(
                    module.exclusion_reason(path, self.target, "darwin-arm64"), reason
                )
        self.assertEqual(
            module.exclusion_reason(
                "node_modules/node-pty/third_party/conpty/x", ("win32", "x64"), "win32-x64"
            ),
            None,
        )

    def test_koffi_import_library_only(self):
        self.assertEqual(
            module.exclusion_reason(
                "node_modules/@koromix/koffi-win32-x64/win32_x64/koffi.lib",
                self.target,
                "darwin-arm64",
            ),
            "Koffi import library",
        )
        self.assertEqual(
            module.exclusion_reason(
                "node_modules/@koromix/koffi-win32-x64/win32_x64/koffi.node",
                self.target,
                "darwin-arm64",
            ),
            None,
        )

    def test_unknown_resources_and_runtime_code_are_retained(self):
        for path in (
            "package.json",
            "node_modules/@deepseek-ai/dsh/lib/bin.js",
            "node_modules/some-package/data/model.json",
            "node_modules/some-package/assets/image.wasm",
            "node_modules/some-package/LICENSE",
        ):
            with self.subTest(path=path):
                self.assertEqual(
                    module.exclusion_reason(path, self.target, "darwin-arm64"), None
                )

    def test_nested_node_modules_uses_innermost_package(self):
        self.assertEqual(
            module.exclusion_reason(
                "node_modules/outer/node_modules/node-pty/prebuilds/win32-x64/pty.node",
                self.target,
                "darwin-arm64",
            ),
            "node-pty other platform",
        )


class ProfileTests(unittest.TestCase):
    def test_target_parts(self):
        self.assertEqual(module.target_parts("darwin-arm64"), ("darwin", "arm64"))
        self.assertEqual(module.target_parts("windows-x64"), ("win32", "x64"))

    def test_derived_ids_distinguish_profiles(self):
        full = module.derived_runtime_id("0.2.2-rc.5", "24.20.0", "darwin-arm64", "full")
        code = module.derived_runtime_id("0.2.2-rc.5", "24.20.0", "darwin-arm64", "code")
        self.assertEqual(full, "dsh-0.2.2-rc.5+node-24.20.0+darwin-arm64")
        self.assertEqual(code, f"{full}+code")

    def test_validate_runtime_id_rejects_path_parts(self):
        valid = module.derived_runtime_id("0.2.2-rc.5", "24.20.0", "darwin-arm64", "full")
        self.assertEqual(module.validate_runtime_id(valid), valid)
        for bad in ("", "../escape", "a/b", "a\\b", "a..b"):
            with self.subTest(bad=bad), self.assertRaises(SystemExit):
                module.validate_runtime_id(bad)

    def test_office_engine_prefers_installed_native(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            native = root / "node_modules/@deepseek-ai/libreoffice-kit-darwin-arm64"
            native.mkdir(parents=True)
            self.assertEqual(module.office_engine(root, "darwin-arm64"), "darwin-arm64")
            native.rmdir()
            self.assertEqual(module.office_engine(root, "darwin-arm64"), "wasm")

    def test_ignore_callback_counts_reasons(self):
        from collections import Counter

        counts: Counter[str] = Counter()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            prebuilds = root / "node_modules/node-pty/prebuilds"
            prebuilds.mkdir(parents=True)
            ignored = module.make_exclusion_ignore(
                root, ("darwin", "arm64"), "darwin-arm64", counts
            )(str(prebuilds), ["win32-x64", "darwin-arm64"])
            self.assertEqual(ignored, {"win32-x64"})
            self.assertEqual(counts["node-pty other platform"], 1)


if __name__ == "__main__":
    unittest.main()
