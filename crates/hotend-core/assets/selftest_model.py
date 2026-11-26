# The model `hotend doctor` builds to prove that the sandbox, the Python environment and
# the CAD kernel work together: a 10 x 20 x 30 mm block standing on the bed.
from build123d import *

PARAMS = {}


def build(p, ctx):
    import sys

    import build123d

    print("python", ".".join(str(n) for n in sys.version_info[:3]), "build123d", build123d.__version__)
    return Pos(20, 20, 15) * Box(10, 20, 30)
