"""`python -I -m hotend_agent [--script <path or builtin:name>]`."""

import sys

from .main import main

sys.exit(main())
