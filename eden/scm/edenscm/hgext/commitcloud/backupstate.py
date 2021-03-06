# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from __future__ import absolute_import

import hashlib
import os

from edenscm.mercurial import error, node as nodemod, util
from edenscm.mercurial.pycompat import encodeutf8

from . import dependencies


FORMAT_VERSION = "v1"


class BackupState(object):
    """Stores what commits have been successfully backed up to the cloud."""

    prefix = "commitcloud/backedupheads."

    def __init__(self, repo, remotepath):
        self.repo = repo
        self.remotepath = remotepath
        repo.sharedvfs.makedirs("commitcloud")
        self.filename = os.path.join(
            self.prefix + hashlib.sha256(encodeutf8(remotepath)).hexdigest()[0:8]
        )
        self.heads = set()
        if repo.sharedvfs.exists(self.filename):
            lines = repo.sharedvfs.readutf8(self.filename).splitlines()
            if len(lines) < 2 or lines[0].strip() != FORMAT_VERSION:
                repo.ui.debug(
                    "unrecognised backedupheads version '%s', ignoring\n"
                    % lines[0].strip()
                )
                self.initfromserver()
                return
            if lines[1].strip() != remotepath:
                repo.ui.debug(
                    "backupheads file is for a different remote ('%s' instead of '%s'), reinitializing\n"
                    % (lines[1].strip(), remotepath)
                )
                self.initfromserver()
                return
            heads = (nodemod.bin(head.strip()) for head in lines[2:])
            hasnode = repo.unfiltered().changelog.hasnode
            self.heads = {h for h in heads if hasnode(h)}
        else:
            self.initfromserver()

    def initfromserver(self):
        # Check with the server about all visible commits that we don't already
        # know are backed up.
        repo = self.repo
        remotepath = self.remotepath
        unfi = repo.unfiltered()
        unknown = [
            nodemod.hex(n)
            for n in unfi.nodes(
                "not public() - hidden() - (not public() & ::%ln)", self.heads
            )
        ]

        def getconnection():
            return repo.connectionpool.get(remotepath)

        nodes = {}
        if unknown:
            try:
                nodes = {
                    nodemod.bin(hexnode)
                    for hexnode, backedup in zip(
                        unknown,
                        dependencies.infinitepush.isbackedupnodes(
                            getconnection, unknown
                        ),
                    )
                    if backedup
                }
            except error.RepoError:
                pass
        self.update(nodes)

    @util.propertycache
    def backedup(self):
        unfi = self.repo.unfiltered()
        hasnode = unfi.changelog.hasnode
        heads = [head for head in self.heads if hasnode(head)]
        return set(unfi.nodes("not public() & ::%ln", heads))

    def _write(self, f):
        f.write(encodeutf8("%s\n" % FORMAT_VERSION))
        f.write(encodeutf8("%s\n" % self.remotepath))
        for h in self.heads:
            f.write(encodeutf8("%s\n" % nodemod.hex(h)))

    def update(self, newnodes, tr=None):
        unfi = self.repo.unfiltered()
        # The new backed up heads are the heads of all commits we already knew
        # were backed up plus the newly backed up commits.
        self.heads = list(
            unfi.nodes(
                "heads((not public() & ::%ln) + (not public() & ::%ln))",
                self.heads,
                newnodes,
            )
        )

        if tr is not None:
            tr.addfilegenerator(
                "commitcloudbackedupheads",
                (self.filename,),
                self._write,
                location="shared",
            )
        else:
            with self.repo.sharedvfs.open(self.filename, "w", atomictemp=True) as f:
                self._write(f)

        util.clearcachedproperty(self, "backedup")
