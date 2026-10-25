#!/usr/bin/env python3
"""Fill the expected ids in ids.json with Python's uuid.uuid5.

The expected ids come from this script, never from the Rust or Kotlin code
under test, so the fixture checks those implementations against an
independent UUIDv5. Edit inputs and expected normalized URLs by hand, then
run `python3 -I conformance/fill_ids.py` from the repo root.

`episode_ids` rows name their podcast by `podcast`, the `name` of a
`feed_urls` row, so the podcast ids stay in step with the URL table.
"""

import json
import pathlib
import uuid

path = pathlib.Path(__file__).with_name("ids.json")
fixture = json.loads(path.read_text(encoding="utf-8"))

ns_podcast = uuid.UUID(fixture["namespaces"]["podcast"])
ns_episode = uuid.UUID(fixture["namespaces"]["episode"])

podcast_ids = {}
for row in fixture["feed_urls"]:
    row["podcast_id"] = str(uuid.uuid5(ns_podcast, row["normalized"]))
    podcast_ids[row["name"]] = row["podcast_id"]

for row in fixture["episode_ids"]:
    row["podcast_id"] = podcast_ids[row["podcast"]]
    # str(UUID) is the lowercase hyphenated form the spec hashes.
    name = row["podcast_id"] + "\n" + row["key"]
    row["episode_id"] = str(uuid.uuid5(ns_episode, name))

path.write_text(
    json.dumps(fixture, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
)
