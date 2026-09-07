"""pytest plugin: force the WITHDRAWN fixed-stride rule back in as the module's sampler.

Used only to answer 'was this regression test ever seen red?'. Every test that pins the new
sampler's behaviour must FAIL under this plugin; any that still passes is testing nothing about
the sampler.
"""
from scms_sim_ref.datagen import realism_bench as rb

_REAL = rb._subsample


def _legacy(items, max_items, *, stream=None, seed=None):
    return rb._fixed_stride_subsample(items, max_items)


def pytest_configure(config):
    rb._subsample = _legacy


def pytest_unconfigure(config):
    rb._subsample = _REAL
