import gc
import hashlib
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import a8_acceptance as a8


class OverlayWorkspaceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.overlay = self.root / "overlay"
        self.overlay.mkdir()
        (self.overlay / "fixture.txt").write_text("fixture")
        self.manifest = self.overlay / "CHECKSUMS.sha256"
        self.manifest.write_text(hashlib.sha256(b"fixture").hexdigest() + "  fixture.txt\n")

    def test_extra_file_rejected_before_copy(self):
        (self.overlay / "unlisted.txt").write_text("extra")
        target = self.root / "out"
        with self.assertRaises(a8.AcceptanceError):
            a8.apply_gonka_overlay(self.overlay, target)
        self.assertFalse(target.exists())

    def test_invalid_manifest_entries(self):
        valid = self.manifest.read_text()
        for text in ("", "broken", valid + valid, "0" * 64 + "  ../escape", "z" * 64 + "  fixture.txt"):
            with self.subTest(text=text):
                self.manifest.write_text(text)
                with self.assertRaises(a8.AcceptanceError):
                    a8.verify_gonka_overlay_integrity(self.overlay)

    def git(self, *args):
        return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout.strip()

    def repository(self):
        base = self.root / "base"
        self.git("init", str(base))
        self.git("-C", str(base), "config", "user.name", "Test")
        self.git("-C", str(base), "config", "user.email", "test@example.invalid")
        (base / "source.txt").write_text("source")
        self.git("-C", str(base), "add", "source.txt")
        self.git("-C", str(base), "commit", "-m", "base")
        return base, self.git("-C", str(base), "rev-parse", "HEAD")

    def test_real_git_kept_workspace_and_exact_parent(self):
        base, sha = self.repository()
        with patch.object(a8.tempfile, "mkdtemp", return_value=str(self.root / "kept")):
            workspace, cleanup = a8.prepare_temporary_gonka_workspace(
                base, self.overlay, a8.Runner(), base_sha=sha, keep_temp=True)
        gc.collect()
        self.assertIsNone(cleanup)
        self.assertTrue((workspace / "fixture.txt").exists())
        self.assertEqual(self.git("-C", str(workspace), "rev-parse", "HEAD^"), sha)
        self.assertEqual(self.git("-C", str(base), "status", "--porcelain"), "")

    def test_missing_pin_never_falls_back(self):
        base, _ = self.repository()
        target = self.root / "workspace"
        with self.assertRaises(a8.AcceptanceError):
            a8.prepare_temporary_gonka_workspace(base, self.overlay, a8.Runner(),
                target_dir=target, base_sha="0" * 40)
        self.assertFalse((target / "fixture.txt").exists())

    def test_clone_failure_never_copies_source(self):
        base, sha = self.repository()
        target = self.root / "occupied"
        target.mkdir()
        (target / "sentinel").write_text("keep")
        with self.assertRaises(a8.AcceptanceError):
            a8.prepare_temporary_gonka_workspace(base, self.overlay, a8.Runner(),
                target_dir=target, base_sha=sha)
        self.assertEqual((target / "sentinel").read_text(), "keep")
        self.assertFalse((target / "source.txt").exists())
