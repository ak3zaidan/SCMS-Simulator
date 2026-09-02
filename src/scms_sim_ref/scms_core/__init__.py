"""SCMS cryptographic core: linkage values, butterfly keys, real ECDSA, 1609.2 certificates.

Import cost note: `certificate` / `ecdsa_p256` / `provisioning` / `secured` / `unlinkability` are
NOT imported here. `run.py` does `from ..scms_core.linkage import ...` at module scope, and the new
modules probe the ECDSA backend at import time; keeping them out of the package `__init__` means a
run that has not opted into real security pays nothing for their existence, which is the same
"default off is byte-identical and cost-free" discipline the plugin work established.
"""

from . import butterfly, ec
from .linkage import (
    CrlLinkageEntry,
    DeviceLinkageContext,
    crl_contains,
    linkage_seed_at,
    linkage_seed_next,
    linkage_value,
    pre_linkage_value,
    random_linkage_seed,
)

__all__ = [
    "CrlLinkageEntry",
    "DeviceLinkageContext",
    "crl_contains",
    "linkage_seed_at",
    "linkage_seed_next",
    "linkage_value",
    "pre_linkage_value",
    "random_linkage_seed",
]
