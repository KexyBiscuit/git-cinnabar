from __future__ import unicode_literals


class Abort(Exception):
    """Raised if a command needs to print an error and exit."""


class HelperClosedError(RuntimeError):
    """Running a query with a closed helper."""


class NothingToGraftException(Exception):
    """Not found any tree to graft."""


class AmbiguousGraftAbort(Abort):
    """Cannot graft the changeset."""


class UpgradeAbort(Abort):
    """Metadata needs an upgrade."""
    def __init__(self, message=None):
        super(UpgradeAbort, self).__init__(
            message or
            'Git-cinnabar metadata needs upgrade. '
            'Please run `git cinnabar upgrade`.'
        )


class OldUpgradeAbort(UpgradeAbort):
    """Metadata needs a consistency check."""
    def __init__(self):
        super(OldUpgradeAbort, self).__init__(
            'Metadata from git-cinnabar versions older than 0.5.0 is not '
            'supported.\n'
            'Please run `git cinnabar upgrade` with version 0.5.x first.'
        )
