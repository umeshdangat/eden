#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

from eden.integration.lib import edenclient
from eden.integration.snapshot import verify as verify_mod
from eden.integration.snapshot.snapshot import HgSnapshot, snapshot_class


@snapshot_class(
    "basic",
    "A simple directory structure with a mix of loaded, materialized, "
    "and unloaded files.",
)
class BasicSnapshot(HgSnapshot):
    def populate_backing_repo(self) -> None:
        repo = self.backing_repo
        repo.write_file("README.md", "project docs")
        repo.write_file(".gitignore", "ignored.txt\n")

        repo.write_file("main/loaded_dir/loaded_file.c", "loaded")
        repo.write_file("main/loaded_dir/not_loaded_file.c", "not loaded")
        repo.write_file("main/loaded_dir/not_loaded_exe.sh", "not loaded", mode=0o755)

        repo.write_file(
            "main/materialized_subdir/script.sh", "original script contents", mode=0o755
        )
        repo.write_file("main/materialized_subdir/test.c", "original test contents")
        repo.write_file("main/materialized_subdir/unmodified.txt", "original contents")
        repo.write_file("main/mode_changes/normal_to_exe.txt", "will change mode")
        repo.write_file(
            "main/mode_changes/exe_to_normal.txt", "will change mode", mode=0o755
        )
        repo.write_file("main/mode_changes/normal_to_readonly.txt", "will be readonly")

        repo.write_file("never_accessed/foo/bar/baz.txt", "baz\n")
        repo.write_file("never_accessed/foo/bar/xyz.txt", "xyz\n")
        repo.write_file("never_accessed/foo/file.txt", "data\n")
        repo.commit("Initial commit.")

    def populate_checkout(self) -> None:
        # Load the main/loaded_dir directory and the main/loaded_dir/lib.c file
        # This currently allocates inode numbers for everything in main/loaded_dir/ and
        # causes main/loaded_dir/ to be tracked in the overlay
        self.list_dir("main/loaded_dir")
        self.read_file("main/loaded_dir/loaded_file.c")

        # Modify some files in main/materialized_subdir to force them to be materialized
        self.write_file(
            "main/materialized_subdir/script.sh", b"new script contents", 0o755
        )
        self.write_file("main/materialized_subdir/test.c", b"new test contents")

        # Test materializing some files by changing their mode
        self.chmod("main/mode_changes/normal_to_exe.txt", 0o755)
        self.chmod("main/mode_changes/exe_to_normal.txt", 0o644)
        self.chmod("main/mode_changes/normal_to_readonly.txt", 0o400)

        # Create a new top-level directory with some new files
        self.write_file("untracked/new/normal.txt", b"new src contents")
        self.write_file("untracked/new/normal2.txt", b"extra src contents")
        self.write_file("untracked/new/readonly.txt", b"new readonly contents", 0o400)
        self.write_file("untracked/executable.exe", b"do stuff", mode=0o755)
        self.make_socket("untracked/everybody.sock", mode=0o666)
        self.make_socket("untracked/owner_only.sock", mode=0o600)

        # Create some untracked files in an existing tracked directory
        self.write_file("main/untracked.txt", b"new new untracked file")
        self.write_file("main/ignored.txt", b"new ignored file")
        self.write_file("main/untracked_dir/foo.txt", b"foobar")

    def verify_snapshot_data(
        self, verifier: verify_mod.SnapshotVerifier, eden: edenclient.EdenFS
    ) -> None:
        # Confirm that `hg status` reports the correct information
        self.verify_hg_status(verifier)

        # Confirm that the files look like what we expect
        File = verify_mod.ExpectedFile
        Socket = verify_mod.ExpectedSocket
        Symlink = verify_mod.ExpectedSymlink
        expected_files = [
            # TODO: These symlink permissions should ideally be 0o777
            Symlink(".eden/root", bytes(self.checkout_path), 0o770),
            Symlink(
                ".eden/client",
                bytes(self.eden_state_dir / "clients" / "checkout"),
                0o770,
            ),
            Symlink(".eden/socket", bytes(self.eden_state_dir / "socket"), 0o770),
            File("README.md", b"project docs", 0o644),
            File(".gitignore", b"ignored.txt\n", 0o644),
            File("main/loaded_dir/loaded_file.c", b"loaded", 0o644),
            File("main/loaded_dir/not_loaded_file.c", b"not loaded", 0o644),
            File("main/loaded_dir/not_loaded_exe.sh", b"not loaded", 0o755),
            File("main/materialized_subdir/script.sh", b"new script contents", 0o755),
            File("main/materialized_subdir/test.c", b"new test contents", 0o644),
            File(
                "main/materialized_subdir/unmodified.txt", b"original contents", 0o644
            ),
            File("main/mode_changes/normal_to_exe.txt", b"will change mode", 0o755),
            File("main/mode_changes/exe_to_normal.txt", b"will change mode", 0o644),
            File(
                "main/mode_changes/normal_to_readonly.txt", b"will be readonly", 0o400
            ),
            File("main/untracked.txt", b"new new untracked file", 0o644),
            File("main/ignored.txt", b"new ignored file", 0o644),
            File("main/untracked_dir/foo.txt", b"foobar", 0o644),
            File("never_accessed/foo/bar/baz.txt", b"baz\n", 0o644),
            File("never_accessed/foo/bar/xyz.txt", b"xyz\n", 0o644),
            File("never_accessed/foo/file.txt", b"data\n", 0o644),
            File("untracked/new/normal.txt", b"new src contents", 0o644),
            File("untracked/new/normal2.txt", b"extra src contents", 0o644),
            File("untracked/new/readonly.txt", b"new readonly contents", 0o400),
            File("untracked/executable.exe", b"do stuff", 0o755),
            Socket("untracked/everybody.sock", 0o666),
            Socket("untracked/owner_only.sock", 0o600),
        ]
        verifier.verify_directory(self.checkout_path, expected_files)

    def verify_hg_status(self, verifier: verify_mod.SnapshotVerifier) -> None:
        expected_status = {
            "main/materialized_subdir/script.sh": "M",
            "main/materialized_subdir/test.c": "M",
            "main/mode_changes/normal_to_exe.txt": "M",
            "main/mode_changes/exe_to_normal.txt": "M",
            # We changed the mode on main/mode_changes/normal_to_readonly.txt,
            # but the change isn't significant to mercurial.
            "untracked/new/normal.txt": "?",
            "untracked/new/normal2.txt": "?",
            "untracked/new/readonly.txt": "?",
            "untracked/executable.exe": "?",
            "untracked/everybody.sock": "?",
            "untracked/owner_only.sock": "?",
            "main/untracked.txt": "?",
            "main/ignored.txt": "I",
            "main/untracked_dir/foo.txt": "?",
        }
        repo = self.hg_repo(self.checkout_path)
        verifier.verify_hg_status(repo, expected_status)
