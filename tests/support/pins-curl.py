#!/usr/bin/env python3
"""Offline curl substitute for the container pin contract tests."""
import json
import os
import sys
from pathlib import Path

fixtures = json.loads(Path(os.environ["AFHTTP_PIN_FIXTURES_PATH"]).read_text())
url = sys.argv[-1]
if url not in fixtures:
    raise SystemExit(f"unexpected upstream request: {url}")
print(json.dumps(fixtures[url]))
