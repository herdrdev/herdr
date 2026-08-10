from __future__ import annotations

import io
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts.vendor_libghostty_vt import (
    apply_patch_series,
    ensure_dist_archive,
    load_patch_series,
    parse_archive_root,
    require_clean_checkout,
    vendor_libghostty_vt,
)


class VendorLibghosttyVtTests(unittest.TestCase):
    @staticmethod
    def make_vendor_fixture(root: Path, marker: str = "keep\n") -> tuple[Path, Path, Path]:
        archive = root / "libghostty-vt.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            data = b"upstream\n"
            info = tarfile.TarInfo("libghostty-vt-test/value.txt")
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))

        destination = root / "destination"
        destination.mkdir()
        (destination / "marker.txt").write_text(marker)
        patch_dir = root / "patches"
        patch_dir.mkdir()
        (patch_dir / "series").write_text("")
        return archive, destination, patch_dir

    def test_parse_archive_root_returns_single_top_level_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            archive = Path(temp_dir) / "libghostty-vt.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                data = b"hello"
                info = tarfile.TarInfo("libghostty-vt-1.0.0/README.md")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))

            self.assertEqual(parse_archive_root(archive), "libghostty-vt-1.0.0")

    def test_ensure_dist_archive_refuses_stale_archives_without_head_match(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            dist = repo / "zig-out" / "dist"
            dist.mkdir(parents=True)
            (dist / "libghostty-vt-1.3.2-main-+deadbeef0.tar.gz").write_bytes(b"stale")

            def git_output(command: list[str], **_kwargs: object) -> str:
                if command[1] == "status":
                    return ""
                return "0123456789abcdef\n"

            with (
                mock.patch("scripts.vendor_libghostty_vt.subprocess.run"),
                mock.patch(
                    "scripts.vendor_libghostty_vt.subprocess.check_output",
                    side_effect=git_output,
                ),
            ):
                with self.assertRaisesRegex(FileNotFoundError, "HEAD 012345678"):
                    ensure_dist_archive(repo)

    def test_require_clean_checkout_rejects_tracked_and_untracked_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            with mock.patch(
                "scripts.vendor_libghostty_vt.subprocess.check_output",
                return_value=" M src/terminal.zig\n?? local.patch\n",
            ):
                with self.assertRaisesRegex(ValueError, "refusing to vendor from dirty checkout"):
                    require_clean_checkout(repo)

    def test_ensure_dist_archive_rejects_checkout_dirtied_by_build(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir)
            with (
                mock.patch("scripts.vendor_libghostty_vt.subprocess.run"),
                mock.patch(
                    "scripts.vendor_libghostty_vt.subprocess.check_output",
                    side_effect=["", "0123456789abcdef\n", " M generated.txt\n"],
                ),
            ):
                with self.assertRaisesRegex(ValueError, "refusing to vendor from dirty checkout"):
                    ensure_dist_archive(repo)

    def test_patch_series_is_ordered_and_complete(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            patch_dir = Path(temp_dir)
            (patch_dir / "series").write_text("0002-second.patch\n0001-first.patch\n")
            (patch_dir / "0001-first.patch").write_text("first")
            (patch_dir / "0002-second.patch").write_text("second")

            self.assertEqual(
                [path.name for path in load_patch_series(patch_dir)],
                ["0002-second.patch", "0001-first.patch"],
            )

            (patch_dir / "0003-unlisted.patch").write_text("unlisted")
            with self.assertRaisesRegex(ValueError, "unlisted patch"):
                load_patch_series(patch_dir)

    def test_apply_patch_series_replays_patches_in_declared_order(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            project_root = Path(temp_dir)
            vendor = project_root / "vendor" / "libghostty-vt"
            patch_dir = project_root / "patches"
            vendor.mkdir(parents=True)
            patch_dir.mkdir()
            (vendor / "value.txt").write_text("zero\n")
            (patch_dir / "series").write_text("0001-one.patch\n0002-two.patch\n")
            (patch_dir / "0001-one.patch").write_text(
                "--- a/vendor/libghostty-vt/value.txt\n"
                "+++ b/vendor/libghostty-vt/value.txt\n"
                "@@ -1 +1 @@\n-zero\n+one\n"
            )
            (patch_dir / "0002-two.patch").write_text(
                "--- a/vendor/libghostty-vt/value.txt\n"
                "+++ b/vendor/libghostty-vt/value.txt\n"
                "@@ -1 +1 @@\n-one\n+two\n"
            )

            apply_patch_series(project_root, patch_dir)

            self.assertEqual((vendor / "value.txt").read_text(), "two\n")

    def test_failed_patch_replay_does_not_replace_existing_vendor_tree(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, destination, patch_dir = self.make_vendor_fixture(root)
            (patch_dir / "series").write_text("0001-broken.patch\n")
            (patch_dir / "0001-broken.patch").write_text("not a patch\n")

            with (
                mock.patch(
                    "scripts.vendor_libghostty_vt.ensure_dist_archive",
                    return_value=archive,
                ),
                mock.patch(
                    "scripts.vendor_libghostty_vt.git_head",
                    return_value="0123456789abcdef",
                ),
            ):
                with self.assertRaises(subprocess.CalledProcessError):
                    vendor_libghostty_vt(root, destination, patch_dir)

            self.assertEqual((destination / "marker.txt").read_text(), "keep\n")

    def test_failed_install_and_rollback_preserve_the_previous_vendor_backup(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, destination, patch_dir = self.make_vendor_fixture(root)
            original_rename = Path.rename

            def fail_install_and_rollback(path: Path, target: Path) -> Path:
                if path == destination:
                    return original_rename(path, target)
                if path.parent.name.startswith(f".{destination.name}.new-"):
                    raise OSError("install failed")
                if path.parent.name.startswith(f".{destination.name}.old-"):
                    raise OSError("rollback failed")
                return original_rename(path, target)

            with (
                mock.patch(
                    "scripts.vendor_libghostty_vt.ensure_dist_archive",
                    return_value=archive,
                ),
                mock.patch(
                    "scripts.vendor_libghostty_vt.git_head",
                    return_value="0123456789abcdef",
                ),
                mock.patch.object(Path, "rename", autospec=True, side_effect=fail_install_and_rollback),
            ):
                with self.assertRaisesRegex(RuntimeError, "backup preserved"):
                    vendor_libghostty_vt(root, destination, patch_dir)

            backups = list(root.glob(f".{destination.name}.old-*/{destination.name}/marker.txt"))
            self.assertEqual(len(backups), 1)
            self.assertEqual(backups[0].read_text(), "keep\n")

    def test_post_install_backup_cleanup_failure_keeps_vendor_and_returns_success(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, destination, patch_dir = self.make_vendor_fixture(root, "old\n")
            original_rmtree = shutil.rmtree

            def fail_backup_cleanup(path: Path, *args: object, **kwargs: object) -> None:
                path = Path(path)
                if path.name == destination.name and path.parent.name.startswith(
                    f".{destination.name}.old-"
                ):
                    raise OSError("cleanup failed")
                original_rmtree(path, *args, **kwargs)

            with (
                mock.patch(
                    "scripts.vendor_libghostty_vt.ensure_dist_archive",
                    return_value=archive,
                ),
                mock.patch(
                    "scripts.vendor_libghostty_vt.git_head",
                    return_value="0123456789abcdef",
                ),
                mock.patch(
                    "scripts.vendor_libghostty_vt.shutil.rmtree",
                    side_effect=fail_backup_cleanup,
                ),
            ):
                vendor_libghostty_vt(root, destination, patch_dir)

            self.assertEqual((destination / "value.txt").read_text(), "upstream\n")
            backups = list(root.glob(f".{destination.name}.old-*/{destination.name}/marker.txt"))
            self.assertEqual(len(backups), 1)
            self.assertEqual(backups[0].read_text(), "old\n")

    def test_vendored_tree_contains_required_upstream_files(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "libghostty-vt"
        required = [
            root / "build.zig",
            root / "build.zig.zon",
            root / "CMakeLists.txt",
            root / "dist" / "cmake" / "ghostty-vt-config.cmake.in",
            root / "include" / "ghostty" / "vt.h",
            root / "include" / "ghostty" / "vt" / "render.h",
            root / "src" / "lib_vt.zig",
        ]

        missing = [str(path.relative_to(root)) for path in required if not path.exists()]
        self.assertEqual(missing, [])

    def test_vendor_metadata_exists_and_points_at_vendored_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        metadata = project_root / "vendor" / "libghostty-vt.vendor.json"
        self.assertTrue(metadata.exists())
        text = metadata.read_text()
        self.assertIn('"source_commit"', text)
        self.assertIn('"dist_archive"', text)
        self.assertIn('"extracted_dir"', text)

    def test_local_vendor_patches_are_listed_in_patch_index(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        index = project_root / "vendor" / "libghostty-vt.patches.md"
        patch_dir = project_root / "vendor" / "patches" / "libghostty-vt"
        patches = sorted(patch_dir.glob("*.patch"))

        if not patches:
            return

        self.assertTrue(index.exists())
        text = index.read_text()
        missing = [
            str(path.relative_to(project_root))
            for path in patches
            if str(path.relative_to(project_root)) not in text
        ]
        self.assertEqual(missing, [])

    def test_patch_series_reconstructs_the_checked_in_vendor_tree(self) -> None:
        project_root = Path(__file__).resolve().parent.parent
        patch_dir = project_root / "vendor" / "patches" / "libghostty-vt"
        tracked = subprocess.check_output(
            ["git", "ls-files", "vendor/libghostty-vt"],
            cwd=project_root,
            text=True,
        ).splitlines()

        with tempfile.TemporaryDirectory() as temp_dir:
            replay_root = Path(temp_dir)
            for relative in tracked:
                source = project_root / relative
                target = replay_root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)

            for patch in reversed(load_patch_series(patch_dir)):
                subprocess.run(
                    ["git", "apply", "--reverse", str(patch)],
                    cwd=replay_root,
                    check=True,
                )
            apply_patch_series(replay_root, patch_dir)

            changed = [
                relative
                for relative in tracked
                if (project_root / relative).read_bytes()
                != (replay_root / relative).read_bytes()
            ]
            self.assertEqual(changed, [])

    def test_embedded_libghostty_logging_is_silenced(self) -> None:
        root = Path(__file__).resolve().parent.parent / "vendor" / "libghostty-vt"
        lib_vt = root / "src" / "lib_vt.zig"
        sys_zig = root / "src" / "terminal" / "c" / "sys.zig"
        lib_text = lib_vt.read_text()
        sys_text = sys_zig.read_text()
        self.assertIn('.logFn = @import("terminal/c/sys.zig").logFn', lib_text)
        self.assertIn("if (global.log == null) return;", sys_text)


if __name__ == "__main__":
    unittest.main()
