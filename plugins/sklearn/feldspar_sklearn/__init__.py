"""`feldspar-sklearn`: scikit-learn estimators, as Saltcorn model providers.

Deliberately empty of declarations. What this distribution supplies is
registered by importing :mod:`feldspar_sklearn.plugin`, which is what its
``saltcorn.plugins`` entry point names — so a host that imported *this* module
and read what it found would see nothing, and the entry point is doing the work
it is there to do.
"""

__all__ = ()
