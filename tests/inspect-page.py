#!/usr/bin/env python3
"""Capture raw observation and readiness samples from an existing host tab.

Uses the driver's normal host/token discovery; never restarts the host or profile.
Example: DOCKER_CONTEXT=colima python3 tests/inspect-page.py \
    --tab TARGET --out /tmp/page-samples
"""

import argparse
import json
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", default="afhttp")
    parser.add_argument("--tab", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--sample-count", type=int, default=20)
    args = parser.parse_args()
    if args.sample_count < 1:
        parser.error("--sample-count must be positive")
    args.out.mkdir(parents=True, exist_ok=True)

    def evaluate(expression):
        result = subprocess.run(
            [args.driver, "cdp", "Runtime.evaluate", "--tab", args.tab,
             "--params", "@-"],
            input=json.dumps({"expression": expression, "returnByValue": True}),
            text=True, capture_output=True, check=True,
        )
        envelope = json.loads(result.stdout)
        response = envelope["result"]["result"]
        if "exceptionDetails" in response:
            raise RuntimeError(response["exceptionDetails"])
        return json.loads(response["result"]["value"])

    root = Path(__file__).resolve().parent.parent
    snapshot = evaluate((root / "assets/observation/snapshot.js").read_text())
    (args.out / "observation-raw.json").write_text(json.dumps(snapshot, indent=2))
    metrics_js = (root / "assets/readiness/metrics.js").read_text()
    expression = """JSON.stringify({
      metrics: (__METRICS__)('body').body,
      text: document.body ? document.body.innerText : '',
      html: document.documentElement.outerHTML,
      elements: Array.from(document.querySelectorAll('*')).map(el => ({
        tag: el.tagName, id: el.id,
        attributes: Object.fromEntries(Array.from(el.attributes).map(a => [a.name, a.value]))
      }))
    })""".replace("__METRICS__", metrics_js)
    samples = []
    for index in range(args.sample_count):
        samples.append(evaluate(expression))
        if index + 1 < args.sample_count:
            time.sleep(0.25)
    (args.out / "readiness-samples.json").write_text(json.dumps(samples, indent=2))
    summary = []
    for previous, current in zip(samples, samples[1:]):
        changes = []
        for before, after in zip(previous["elements"], current["elements"]):
            if before != after:
                changes.append({"before": before, "after": after})
        summary.append({
            "text_changed": previous["text"] != current["text"],
            "html_changed": previous["html"] != current["html"],
            "previous_node_count": len(previous["elements"]),
            "node_count": len(current["elements"]),
            "metrics": current["metrics"],
            "dom_metric_changed": (
                previous["metrics"]["dom_hash"] != current["metrics"]["dom_hash"]
            ),
            "element_changes": changes,
        })
    (args.out / "changes.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps({
        "out": str(args.out),
        "emitted_nodes": len(snapshot["nodes"]),
        "text_changes": sum(s["text_changed"] for s in summary),
        "dom_metric_changes": sum(s["dom_metric_changed"] for s in summary),
        "html_changes": sum(s["html_changed"] for s in summary),
    }))


if __name__ == "__main__":
    main()
