"""Sample only selected battery properties. Whole-Mac power, not per-app power."""
import json
import re
import statistics
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

samples = []
for index in range(6):
    if index:
        time.sleep(5)
    raw = subprocess.check_output(["ioreg", "-r", "-c", "AppleSmartBattery"], text=True)
    selected = {}
    for key in ["Voltage", "Amperage", "InstantAmperage", "ExternalConnected", "IsCharging"]:
        match = re.search(r'^\s*"' + key + r'" = (\d+|Yes|No)\s*$', raw, re.MULTILINE)
        if match:
            selected[key] = match.group(1)
    mv, ma = int(selected["Voltage"]), int(selected["Amperage"])
    if ma >= 2**63:
        ma -= 2**64
    watts = -ma * mv / 1_000_000
    samples.append({"timestamp_utc": datetime.now(timezone.utc).isoformat(),
                    "millivolts": mv, "milliamps_signed": ma,
                    "estimated_discharge_watts": watts,
                    "external_power": selected.get("ExternalConnected") == "Yes"})
summary = {"median_discharge_watts": statistics.median(s["estimated_discharge_watts"] for s in samples),
           "min_discharge_watts": min(s["estimated_discharge_watts"] for s in samples),
           "max_discharge_watts": max(s["estimated_discharge_watts"] for s in samples)}
result = {"method": "V*I from Apple's battery IORegistry mV/mA properties. Six readings over about 25s. Hardware gauge can lag/smooth. Whole-machine estimate including display and all apps. T3 and research agents active; no A/B energy inference.",
          "summary": summary, "samples": samples}
Path("research/battery-sample.json").write_text(json.dumps(result, indent=2))
print(json.dumps(summary, indent=2))
