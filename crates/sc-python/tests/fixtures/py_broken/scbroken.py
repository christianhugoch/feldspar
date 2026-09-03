"""A PEP 517 backend that refuses, with a sentence a test can look for."""

MESSAGE = "this fixture backend refuses to build, on purpose"


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    raise RuntimeError(MESSAGE)


def build_sdist(sdist_directory, config_settings=None):
    raise RuntimeError(MESSAGE)


def get_requires_for_build_wheel(config_settings=None):
    raise RuntimeError(MESSAGE)
