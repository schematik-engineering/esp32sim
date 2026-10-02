"""Verify and extract the retained three-chip input artifacts for external tests."""
import base64
import hashlib
import json
from pathlib import Path
import sys

source, destination = map(Path, sys.argv[1:])
expected = json.loads(Path(__file__).with_name("inputs.json").read_text())
for chip, receipt in expected.items():
    artifact = json.loads((source / chip / "artifact.json").read_text())
    assert artifact["flashBytes"] == receipt["flashBytes"]
    assert len(artifact["artifacts"]) == len(receipt["flash"])
    output = destination / chip
    output.mkdir(parents=True, exist_ok=True)
    for file, recorded in zip(artifact["artifacts"], receipt["flash"]):
        data = base64.b64decode(file["data"], validate=True)
        assert file["offset"] == recorded["offset"]
        assert file["filename"] == recorded["filename"]
        assert len(data) == recorded["bytes"]
        assert hashlib.sha256(data).hexdigest() == recorded["sha256"]
        (output / str(file["offset"])).write_bytes(data)
    (output / "flash.txt").write_text(
        "\n".join(str(file["offset"]) for file in receipt["flash"]) + "\n"
    )
    print(chip, "verified", len(receipt["flash"]), "flash segments")
