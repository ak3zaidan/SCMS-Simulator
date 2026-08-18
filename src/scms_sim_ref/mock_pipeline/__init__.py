"""Closed-loop reference pipeline (pre-MOSAIC)."""

from .run import PipelineConfig, RunResult, run_pipeline, config_from_dict, validate_config

__all__ = ["PipelineConfig", "RunResult", "run_pipeline", "config_from_dict", "validate_config"]
