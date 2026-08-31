#!/usr/bin/env python3
"""Refuse a workflow GitHub would refuse.

There is no catching this in CI: an invalid workflow is rejected before any
job runs, so the file that would have checked it never executes. It has to be
checked here, before a push.

The specific thing that got through: `runs-on` written twice in one job.
`yaml.safe_load` takes the last of a duplicated key and says nothing, so a
file that GitHub rejects outright parses cleanly in Python and looks verified.

    python3 scripts/check-workflows.py
"""

import pathlib
import sys

import yaml


class Strict(yaml.SafeLoader):
    """A loader that treats a repeated key as the error it is."""


def no_duplicates(loader, node, deep=False):
    seen = {}
    for key_node, _ in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in seen:
            raise yaml.YAMLError(
                f"line {key_node.start_mark.line + 1}: '{key}' is already "
                f"defined (first at line {seen[key]})"
            )
        seen[key] = key_node.start_mark.line + 1
    return yaml.SafeLoader.construct_mapping(loader, node, deep)


Strict.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, no_duplicates)


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    files = sorted((root / ".github" / "workflows").glob("*.yml"))
    if not files:
        print("no workflows found — is this the right directory?")
        return 1

    bad = False
    for path in files:
        name = path.relative_to(root)
        try:
            workflow = yaml.load(path.read_text(), Loader=Strict)
        except yaml.YAMLError as error:
            print(f"{name}: {error}")
            bad = True
            continue

        # `on` is the YAML 1.1 boolean True, which is its own small trap.
        triggers = workflow.get("on") or workflow.get(True)
        if not triggers:
            print(f"{name}: no triggers, so it can never run")
            bad = True
        if not workflow.get("jobs"):
            print(f"{name}: no jobs")
            bad = True
            continue

        jobs = ", ".join(workflow["jobs"])
        print(f"{name}: ok — {len(workflow['jobs'])} jobs ({jobs})")

    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
