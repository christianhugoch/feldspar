"""The fixture distribution's top-level package.

Deliberately **empty of declarations**: what this plugin supplies is registered
in `plugin.py`, which the distribution names as its `saltcorn.plugins` entry
point. So a host that imported this module and read whatever it found would see
nothing, and the tests would fail — which is the point of putting it here.
"""

NAME = "sc-plugin-fixture"
