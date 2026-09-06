#!/usr/bin/env python3
"""Compatibility entry point for memory.py. Prefer that name on both OSes."""

import sys

from memory import main


if __name__ == "__main__":
    sys.exit(main())
