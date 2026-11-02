import hashlib
import importlib.util
import json
import os
import tempfile
import sys
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('package_toolchain_licenses.py')
spec = importlib.util.spec_from_file_location('toolchain_licenses', SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ToolchainLicenses(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.rust = self.root / 'rust'
        self.zig = self.root / 'zig'
        self.lib = self.zig / 'lib'
        sources = {'rust/share/doc/rust/COPYRIGHT-library.html': b'rust\r\noriginal',
                   'zig/LICENSE': b'zig\noriginal', 'zig/lib/libc/musl/COPYRIGHT': b'musl\noriginal'}
        for name, data in sources.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.responses = {
            ('rustc', '-vV'): 'rustc 1.96.1 (31fca3adb 2026-06-26)\nbinary: rustc\nrelease: 1.96.1',
            ('rustc', '--print', 'sysroot'): str(self.rust),
            ('zig', 'version'): '0.16.0',
            ('zig', 'env'): '.{\n .zig_exe = ' + json.dumps(str(self.zig / 'zig')) + ',\n .lib_dir = ' + json.dumps(str(self.lib)) + ',\n}',
        }

    def collect(self):
        with patch.dict(os.environ, {'ZIG_LIB_DIR': ''}), \
             patch.object(module, 'output', side_effect=lambda args: self.responses[tuple(args)]):
            return module.collect()

    def test_archive_preserves_bytes_versions_target_and_no_paths(self):
        files, rust, zig = self.collect()
        archive = self.root / 'notices.zip'
        module.write_archive(archive, 'x86_64-unknown-linux-musl', 'a' * 40, files, rust, zig)
        with zipfile.ZipFile(archive) as result:
            metadata = json.loads(result.read('origem.json'))
            self.assertEqual(metadata['alvo'], 'x86_64-unknown-linux-musl')
            self.assertEqual(metadata['fonte_commit'], 'a' * 40)
            self.assertEqual(metadata['rustc_vV'], rust)
            self.assertEqual(metadata['zig_version'], '0.16.0')
            self.assertNotIn(str(self.root), json.dumps(metadata))
            for name, data in files.items():
                self.assertEqual(result.read(name), data)
                self.assertEqual(metadata['arquivos'][name], hashlib.sha256(data).hexdigest())
        with self.assertRaises(FileExistsError):
            module.write_archive(archive, 'x86_64-unknown-linux-musl', 'a' * 40, files, rust, zig)

    def test_incorrect_versions_and_missing_text_stop_collection(self):
        for key, value in ((('rustc', '-vV'), 'rustc 1.96.2\nrelease: 1.96.2'),
                           (('zig', 'version'), '0.15.0')):
            with self.subTest(key=key):
                original = self.responses[key]
                self.responses[key] = value
                with self.assertRaises(ValueError):
                    self.collect()
                self.responses[key] = original
        (self.lib / 'libc/musl/COPYRIGHT').unlink()
        with self.assertRaisesRegex(ValueError, 'missing'):
            self.collect()

    def test_zig_environment_and_override(self):
        self.assertEqual(module.zon_string(self.responses[('zig', 'env')], 'lib_dir'), self.lib)
        with self.assertRaises(ValueError):
            module.zon_string('.{ .lib_dir = null, }', 'lib_dir')
        other = self.root / 'alternate'
        path = other / 'libc/musl/COPYRIGHT'
        path.parent.mkdir(parents=True)
        path.write_bytes(b'override')
        with patch.dict(os.environ, {'ZIG_LIB_DIR': str(other)}), \
             patch.object(module, 'output', side_effect=lambda args: self.responses[tuple(args)]):
            files, _, _ = module.collect()
        self.assertEqual(files['zig/musl/COPYRIGHT'], b'override')

    def test_cli_uses_build_tool_overrides(self):
        with patch.dict(os.environ, {'RUSTC': '/ferramentas/rustc', 'ZIG': '/ferramentas/zig'}), \
             patch.object(sys, 'argv', ['coletor', '--target', 'aarch64-apple-darwin', '--output', str(self.root / 'x.zip')]), \
             patch.object(module, 'collect', return_value=({}, 'rust', 'zig')) as coletar, \
             patch.object(module, 'output', return_value='a' * 40), \
             patch.object(module, 'write_archive'):
            module.main()
        coletar.assert_called_once_with('/ferramentas/rustc', '/ferramentas/zig')

    def test_zon_windows_paths_preserve_backslashes(self):
        for valor in (r'C:\Zig\lib', r'\\servidor\share\zig\lib'):
            texto = '.{\n .lib_dir = ' + json.dumps(valor) + ',\n}'
            self.assertEqual(module.decode_zon_string(texto, 'lib_dir'), valor)

    def test_rejects_unsafe_rust_metadata_and_invalid_commit(self):
        self.responses[('rustc', '-vV')] += '\nhost: /secret/user'
        with self.assertRaises(ValueError):
            self.collect()
        with self.assertRaises(ValueError):
            module.write_archive(self.root / 'x', 'x86_64-unknown-linux-musl', 'bad', {}, '', '')


if __name__ == '__main__':
    unittest.main()
